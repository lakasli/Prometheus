//! Bounded automatic-task cleanup. Never touches ordinary conversations,
//! long-term memory, credentials, task definitions or source deduplication keys.
use rusqlite::{params, Connection};

pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS trigger_result_tombstones(occurrence_id TEXT PRIMARY KEY REFERENCES trigger_occurrences(id),cleaned_at INTEGER NOT NULL);")?;
    Ok(())
}
pub fn result_expired_in(db: &Connection, occurrence: &str) -> anyhow::Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM trigger_result_tombstones WHERE occurrence_id=?1)",
        [occurrence],
        |r| r.get(0),
    )?)
}
/// Caller owns a short transaction. A pass handles at most64 completed results
/// and128 event bodies; unique keys and cursors deliberately remain intact.
pub fn cleanup_in(db: &Connection, stamp: i64) -> anyhow::Result<usize> {
    let cutoff = stamp.saturating_sub(30 * 86400);
    let events = stamp.saturating_sub(7 * 86400);
    let ids=db.prepare("SELECT o.id,o.run_id FROM trigger_occurrences o WHERE o.completed_at<?1 AND o.status IN ('completed','failed','cancelled','expired','needs_attention') AND NOT EXISTS(SELECT 1 FROM trigger_result_tombstones t WHERE t.occurrence_id=o.id) ORDER BY o.completed_at LIMIT 64")?.query_map([cutoff],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
    let mut cleaned = 0;
    for (occurrence, run) in &ids {
        // A cancellation marker does not prove the transport has been dropped.
        if let Some(run) = run {
            let leased: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM native_worker_leases WHERE run_id=?1)",
                [run],
                |r| r.get(0),
            )?;
            if leased {
                continue;
            }
            db.execute("UPDATE messages SET content='[自动任务执行正文已按30天保留策略清理]' WHERE run_id=?1 AND EXISTS(SELECT 1 FROM runs WHERE id=?1 AND run_kind IN ('trigger_model','trigger_image') AND status NOT IN ('queued','running'))",[run])?;
            db.execute("UPDATE runs SET input='[自动任务执行正文已清理]',response=NULL WHERE id=?1 AND run_kind IN ('trigger_model','trigger_image') AND status NOT IN ('queued','running')",[run])?;
            db.execute("DELETE FROM run_prompt_snapshots WHERE run_id=?1 AND EXISTS(SELECT 1 FROM runs WHERE id=?1 AND run_kind IN ('trigger_model','trigger_image') AND status NOT IN ('queued','running'))",[run])?;
        }
        db.execute("UPDATE trigger_occurrences SET response=NULL,snapshot_json=json_remove(snapshot_json,'$.event') WHERE id=?1",[occurrence])?;
        db.execute("INSERT OR IGNORE INTO trigger_result_tombstones(occurrence_id,cleaned_at) VALUES(?1,?2)",params![occurrence,stamp])?;
        cleaned += 1;
    }
    db.execute("UPDATE trigger_events SET data_json='{}' WHERE seq IN(SELECT seq FROM trigger_events WHERE received_at<?1 AND data_json!='{}' ORDER BY seq LIMIT 128)",[events])?;
    // Mutation idempotency responses may also contain a previously stored result.
    db.execute("UPDATE trigger_idempotency SET response_json=CASE WHEN json_type(response_json,'$.last_occurrence.response')='text' THEN json_remove(json_set(response_json,'$.last_occurrence.response',NULL),'$.last_occurrence.generated_image_url') ELSE json_remove(json_set(response_json,'$.response',NULL),'$.generated_image_url') END WHERE rowid IN(SELECT rowid FROM trigger_idempotency WHERE created_at<?1 AND (json_type(response_json,'$.response')='text' OR json_type(response_json,'$.last_occurrence.response')='text') ORDER BY created_at LIMIT 128)",[cutoff])?;
    Ok(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleanup_keeps_source_dedup_and_ordinary_history() {
        let temp = tempfile::TempDir::new().unwrap();
        let state = crate::state::AppState::open(crate::state::Options {
            data_dir: temp.path().into(),
            origin: "http://127.0.0.1:18419".into(),
            allow_private_model: true,
            cookie_secure: false,
            web_dir: temp.path().join("web"),
        })
        .unwrap();
        state.db(|db| {
            let tx=db.transaction()?;
            tx.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('ordinary','owner','keep',0)",[])?;
            tx.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at,run_kind) VALUES('ordinary','ordinary','owner','1','KEEP_ORIGINAL','{}','completed',0,0,'model')",[])?;
            tx.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES('ordinary','ordinary','ordinary','user','KEEP_ORIGINAL',0)",[])?;
            tx.execute("INSERT INTO trigger_events(event_id,source_id,event_type,owner_id,data_json,received_at) VALUES('event','dsh.runs','dsh.run.completed.v1','owner','{\"status\":\"completed\"}',1)",[])?;
            cleanup_in(&tx,40*86400)?;
            let body:String=tx.query_row("SELECT content FROM messages WHERE id='ordinary'",[],|r|r.get(0))?;
            assert_eq!(body,"KEEP_ORIGINAL");
            let (event,body):(String,String)=tx.query_row("SELECT event_id,data_json FROM trigger_events",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
            assert_eq!(event,"event");assert_eq!(body,"{}");
            let original:String=tx.query_row("SELECT input FROM runs WHERE id='ordinary'",[],|r|r.get(0))?;
            assert_eq!(original,"KEEP_ORIGINAL");
            tx.commit()?;Ok(())
        }).unwrap();
    }

    #[test]
    fn terminal_body_cleanup_scrubs_cached_results_but_waits_for_actual_lease() {
        let temp = tempfile::TempDir::new().unwrap();
        let state = crate::state::AppState::open(crate::state::Options {
            data_dir: temp.path().into(),
            origin: "http://127.0.0.1:18419".into(),
            allow_private_model: true,
            cookie_secure: false,
            web_dir: temp.path().join("web"),
        })
        .unwrap();
        state.db(|db| {
            let tx=db.transaction()?;
            tx.execute("INSERT INTO trigger_jobs(id,owner_id,title,revision,draft_revision,lifecycle,created_at,updated_at) VALUES('retention','owner','fixture',1,1,'completed',0,0)",[])?;
            for (run,leased) in [("auto",false),("leased",true)] {
                tx.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES(?1,'owner','automatic',0)",[run])?;
                tx.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,response,created_at,updated_at,run_kind) VALUES(?1,?1,'owner',?1,'AUTO_SECRET','{}','completed','AUTO_RESPONSE',0,0,'trigger_model')",[run])?;
                tx.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?1,?1,?1,'assistant','AUTO_RESPONSE',0)",[run])?;
                tx.execute("INSERT INTO run_prompt_snapshots(run_id,messages_json,digest,compiler_version,created_at) VALUES(?1,'[{\"content\":\"AUTO_SECRET\"}]','fixture','dsh-trigger-v1',0)",[run])?;
                tx.execute("INSERT INTO trigger_occurrences(id,job_id,job_revision,owner_id,schedule_epoch,fire_key,origin,nominal_at,deadline,definition_json,snapshot_json,status,run_id,response,delivery_status,created_at,completed_at) VALUES(?1,'retention',1,'owner',1,?1,'trigger',1,300,'{}','{\"event\":{\"data\":\"EVENT_SECRET\"}}','completed',?1,'AUTO_RESPONSE','stored',0,1)",[run])?;
                if leased {tx.execute("INSERT INTO native_worker_leases(run_id,claimed_at) VALUES(?1,0)",[run])?;}
            }
            tx.execute("INSERT INTO trigger_idempotency(owner_id,operation,request_key,request_hash,response_json,created_at) VALUES('owner','test','key','hash','{\"id\":\"auto\",\"response\":\"AUTO_RESPONSE\",\"generated_image_url\":\"/old.png\"}',1)",[])?;
            assert_eq!(cleanup_in(&tx,40*86400)?,1);
            assert!(result_expired_in(&tx,"auto")?);
            assert!(!result_expired_in(&tx,"leased")?);
            let body:Option<String>=tx.query_row("SELECT response FROM runs WHERE id='auto'",[],|r|r.get(0))?;
            assert!(body.is_none());
            let kept:String=tx.query_row("SELECT response FROM runs WHERE id='leased'",[],|r|r.get(0))?;
            assert_eq!(kept,"AUTO_RESPONSE");
            let saved:String=tx.query_row("SELECT snapshot_json FROM trigger_occurrences WHERE id='auto'",[],|r|r.get(0))?;
            assert!(!saved.contains("EVENT_SECRET"));
            let cached:String=tx.query_row("SELECT response_json FROM trigger_idempotency",[],|r|r.get(0))?;
            assert!(!cached.contains("AUTO_RESPONSE")&&!cached.contains("old.png"));
            let requests:i64=tx.query_row("SELECT count(*) FROM run_prompt_snapshots WHERE run_id='auto'",[],|r|r.get(0))?;
            assert_eq!(requests,0);
            tx.execute("DELETE FROM native_worker_leases WHERE run_id='leased'",[])?;
            assert_eq!(cleanup_in(&tx,40*86400)?,1);
            assert_eq!(cleanup_in(&tx,40*86400)?,0);
            tx.commit()?;Ok(())
        }).unwrap();
    }
}
