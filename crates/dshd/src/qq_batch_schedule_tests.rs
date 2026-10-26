//! Exercise the real QQ ingestion transactions and worker claim boundary without
//! starting a model worker, transport, or background task.
use super::*;
use tempfile::TempDir;

fn fixture() -> (TempDir, AppState, RuntimeConfig) {
    let directory = TempDir::new().unwrap();
    let state = AppState::open(crate::state::Options {
        data_dir: directory.path().into(),
        origin: "http://127.0.0.1:18432".into(),
        allow_private_model: true,
        cookie_secure: false,
        web_dir: directory.path().join("web"),
    })
    .unwrap();
    let config = state
        .db(|db| {
            let (_, mut config) = character::current_config_in(db)?;
            config.qq.enabled = true;
            config.qq.app_id = "batch_app".into();
            config.qq.credential_ref = Some("synthetic_qq_reference".into());
            config.assistant.max_concurrent_runs = 2;
            db.execute(
                "UPDATE config_revisions SET config_json=?1 WHERE revision=1",
                [serde_json::to_string(&config)?],
            )?;
            Ok(config)
        })
        .unwrap();
    (directory, state, config)
}

fn ingest(state: &AppState, config: &RuntimeConfig, id: &str, text: &str) -> String {
    let event = format!("event_{id}");
    let payload = json!({"t":"C2C_MESSAGE_CREATE","id":event,"d":{
        "id":id,"content":text,"author":{"user_openid":"peer"}
    }});
    crate::qq::ingest(state, config, &payload).unwrap();
    state
        .db(|db| {
            Ok(db.query_row(
                "SELECT run_id FROM inbox WHERE event_key=?1",
                [format!("qq:batch_app:C2C_MESSAGE_CREATE:{event}")],
                |row| row.get(0),
            )?)
        })
        .unwrap()
}

fn keep_waiting(state: &AppState, run: &str) {
    let at = crate::qq::text_batch::now_ms();
    state
        .db(|db| {
            db.execute(
                "UPDATE qq_text_batches SET first_ms=?1,last_ms=?1,due_ms=?2,closed=0 WHERE run_id=?3",
                params![at, at + crate::qq::text_batch::MAX_WAIT_MS, run],
            )?;
            Ok(())
        })
        .unwrap();
}

fn make_due(state: &AppState, run: &str) {
    state
        .db(|db| {
            db.execute("UPDATE qq_text_batches SET due_ms=0 WHERE run_id=?1", [run])?;
            Ok(())
        })
        .unwrap();
}

fn lease_count(state: &AppState) -> i64 {
    state
        .db(|db| {
            Ok(
                db.query_row("SELECT count(*) FROM native_worker_leases", [], |r| {
                    r.get(0)
                })?,
            )
        })
        .unwrap()
}

#[test]
fn a_waiting_batch_uses_no_native_slot_and_another_conversation_can_run() {
    let (_directory, state, config) = fixture();
    let waiting = ingest(&state, &config, "first", "我今天");
    keep_waiting(&state, &waiting);
    assert!(claim(&state).unwrap().is_none());
    assert_eq!(lease_count(&state), 0);
    state
        .db(|db| {
            db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('web','web_owner','other conversation',0)", [])?;
            Ok(())
        })
        .unwrap();
    let web = enqueue(&state, "web", "web_owner", "web_message", "请继续", None).unwrap();
    let claimed = claim(&state).unwrap().expect("other conversation is ready");
    assert_eq!(claimed.input.id, web);
    assert_eq!(lease_count(&state), 1);
    let status: String = state
        .db(|db| {
            Ok(
                db.query_row("SELECT status FROM runs WHERE id=?1", [&waiting], |r| {
                    r.get(0)
                })?,
            )
        })
        .unwrap();
    assert_eq!(status, "queued");
    drop(claimed);
    assert_eq!(lease_count(&state), 0);
}

#[test]
fn a_ready_later_run_cannot_jump_a_waiting_earlier_run_in_the_same_conversation() {
    let (_directory, state, config) = fixture();
    let earlier = ingest(&state, &config, "first", "我今天");
    let (conversation, owner): (String, String) = state
        .db(|db| {
            Ok(db.query_row(
                "SELECT conversation_id,owner_id FROM runs WHERE id=?1",
                [&earlier],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    // A second internal admission must obey FIFO even when it has no batch.
    let later = enqueue(
        &state,
        &conversation,
        &owner,
        "later",
        "另一条完整问题",
        None,
    )
    .unwrap();
    keep_waiting(&state, &earlier);
    assert!(claim(&state).unwrap().is_none());
    assert_eq!(lease_count(&state), 0);
    make_due(&state, &earlier);
    let claimed = claim(&state).unwrap().expect("earlier batch is now due");
    assert_eq!(claimed.input.id, earlier);
    assert!(claim(&state).unwrap().is_none());
    state
        .db(|db| {
            db.execute("UPDATE runs SET status='completed' WHERE id=?1", [&earlier])?;
            Ok(())
        })
        .unwrap();
    // A completed logical status still does not release the actual lease.
    assert!(claim(&state).unwrap().is_none());
    drop(claimed);
    let following = claim(&state).unwrap().expect("lease has been released");
    assert_eq!(following.input.id, later);
}

#[test]
fn claiming_a_due_batch_closes_it_and_new_user_text_gets_a_new_run() {
    let (_directory, state, config) = fixture();
    let first = ingest(&state, &config, "first", "我今天");
    make_due(&state, &first);
    let claimed = claim(&state).unwrap().expect("batch is due");
    assert_eq!(claimed.input.id, first);
    let (closed, status): (bool, String) = state
        .db(|db| {
            Ok(db.query_row(
                "SELECT b.closed,r.status FROM qq_text_batches b JOIN runs r ON r.id=b.run_id WHERE r.id=?1",
                [&first], |r|Ok((r.get(0)?,r.get(1)?)),
            )?)
        })
        .unwrap();
    assert!(closed);
    assert_eq!(status, "running");
    let second = ingest(&state, &config, "second", "有点累");
    assert_ne!(second, first);
    let (original, count, target): (String, i64, String) = state
        .db(|db| {
            Ok(db.query_row(
                "SELECT r.input,b.fragment_count,json_extract(r.reply_json,'$.message_id') FROM runs r JOIN qq_text_batches b ON b.run_id=r.id WHERE r.id=?1",
                [&first], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
            )?)
        })
        .unwrap();
    assert_eq!(original, "我今天");
    assert_eq!(count, 1);
    assert_eq!(target, "first");
    assert_eq!(lease_count(&state), 1);
    assert!(claim(&state).unwrap().is_none());
    drop(claimed);
}

#[test]
fn clock_rollback_makes_a_future_dated_waiting_batch_ready_instead_of_stalling() {
    let (_directory, state, config) = fixture();
    let run = ingest(&state, &config, "first", "一条消息");
    let future = crate::qq::text_batch::now_ms() + 60_000;
    state
        .db(|db| {
            db.execute("UPDATE qq_text_batches SET first_ms=?1,last_ms=?1,due_ms=?2,closed=0 WHERE run_id=?3", params![future,future+1500,run])?;
            Ok(())
        })
        .unwrap();
    let claimed = claim(&state)
        .unwrap()
        .expect("clock rollback cannot extend a wait");
    assert_eq!(claimed.input.id, run);
    let closed: bool = state
        .db(|db| {
            Ok(db.query_row(
                "SELECT closed FROM qq_text_batches WHERE run_id=?1",
                [&run],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert!(closed);
}

fn save_same_personality(state: &AppState, config: &RuntimeConfig) {
    state
        .db(|db| {
            let current = character::personality_state(db, config)?;
            let personality: character::Personality =
                serde_json::from_value(current["personality"].clone())?;
            character::save_personality(
                db,
                config,
                current["etag"].as_str().unwrap(),
                personality,
            )?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn saving_identical_personality_content_with_a_new_revision_is_a_batch_boundary() {
    let (_directory, state, config) = fixture();
    // Both snapshots use the saved personality compiler, avoiding the distinct
    // legacy-to-saved prompt transition as the reason for separating the Runs.
    save_same_personality(&state, &config);
    let first = ingest(&state, &config, "first", "我今天");
    keep_waiting(&state, &first);
    save_same_personality(&state, &config);
    let second = ingest(&state, &config, "second", "有点累");
    assert_ne!(second, first);
    let snapshots: Vec<(String, Value)> = state
        .db(|db| {
            let mut statement = db.prepare("SELECT character_digest,character_snapshot_json FROM runs WHERE id IN (?1,?2) ORDER BY rowid")?;
            let rows = statement.query_map(params![first,second], |r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?;
            rows.map(|row|{let(digest,text)=row?;Ok((digest,serde_json::from_str(&text)?))}).collect::<anyhow::Result<Vec<_>>>()
        })
        .unwrap();
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0].0, snapshots[1].0);
    assert_eq!(
        snapshots[0].1["system_prompt"],
        snapshots[1].1["system_prompt"]
    );
    assert_ne!(snapshots[0].1["revision"], snapshots[1].1["revision"]);
}
