use super::*;

fn fixture() -> (tempfile::TempDir, AppState, RuntimeConfig) {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open(crate::state::Options {
        data_dir: directory.path().to_owned(),
        origin: "http://127.0.0.1:18419".into(),
        allow_private_model: true,
        cookie_secure: false,
        web_dir: directory.path().join("web"),
    })
    .unwrap();
    let config = state.current_config().unwrap().1;
    (directory, state, config)
}

fn conversation(state: &AppState, channel: &str) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    state.db(|db| {
        db.execute("INSERT INTO conversations(id,owner_id,title,source_key,created_at) VALUES(?1,'owner','delivery summary',?2,0)",
            params![id,(channel == "qq").then(||format!("qq:test:c2c:{id}"))])?;
        Ok(())
    }).unwrap();
    id
}

fn seed(state: &AppState, conversation: &str, count: usize, channel: &str) -> Vec<String> {
    state.db(|db| {
        let tx = db.transaction()?;
        let mut ids = Vec::new();
        for i in 0..count {
            let id = uuid::Uuid::new_v4().to_string();
            let question = format!("用户问题{i}尚须完整回答");
            tx.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,response,created_at,updated_at,run_kind)
                VALUES(?1,?2,'owner',?1,?3,'{}','completed','已确认句段。未确认候选勿当成已发送。',0,0,'model')", params![id,conversation,question])?;
            tx.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?1,?2,?3,'user',?4,0)",
                params![uuid::Uuid::new_v4().to_string(),conversation,id,question])?;
            tx.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?1,?2,?3,'assistant','已确认句段。未确认候选勿当成已发送。',0)",
                params![uuid::Uuid::new_v4().to_string(),conversation,id])?;
            if channel == "qq" {
                tx.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,platform_message_id,deadline,created_at)
                    VALUES(?1,?1,'test-account','test-ref','{}','已确认句段。未确认候选勿当成已发送。','unknown','receipt-one',100,0)",[&id])?;
                tx.execute("INSERT INTO outbox_reply_parts(outbox_id,seq,payload_json,delay_ms,status,platform_message_id,finished_ms)
                    VALUES(?1,1,'{\"type\":\"text\",\"text\":\"已确认句段。\"}',0,'sent','receipt-one',10)",[&id])?;
                tx.execute("INSERT INTO outbox_reply_parts(outbox_id,seq,payload_json,delay_ms,status,finished_ms)
                    VALUES(?1,2,'{\"type\":\"text\",\"text\":\"未确认候选勿当成已发送。\"}',0,'unknown',10)",[&id])?;
            }
            ids.push(id);
        }
        tx.commit()?;
        Ok(ids)
    }).unwrap()
}

fn current(state: &AppState, conversation: &str) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    state.db(|db| {
        db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at,run_kind)
            VALUES(?1,?2,'owner',?1,'继续回答','{}','running',0,0,'model')",params![id,conversation])?;
        Ok(())
    }).unwrap();
    id
}

fn settle(state: &AppState, run: &str) {
    state
        .db(|db| {
            db.execute("UPDATE runs SET status='completed' WHERE id=?1", [run])?;
            Ok(())
        })
        .unwrap();
}

fn accept_late_part(state: &AppState, source: &str) {
    state.db(|db| {
        let tx = db.transaction()?;
        tx.execute("UPDATE outbox_reply_parts SET status='sent',platform_message_id='late-receipt',finished_ms=200 WHERE outbox_id=?1 AND seq=2",[source])?;
        tx.execute("UPDATE outbox SET status='sent',platform_message_id='late-receipt' WHERE id=?1",[source])?;
        tx.commit()?;Ok(())
    }).unwrap();
}

#[test]
fn summary_sources_contain_only_receipted_parts_and_keep_unanswered_user_questions() {
    let (_dir, state, config) = fixture();
    let chat = conversation(&state, "qq");
    let ids = seed(&state, &chat, 12, "qq");
    state.db(|db|{db.execute("UPDATE outbox_reply_parts SET status='failed',platform_message_id=NULL WHERE outbox_id=?1",[&ids[1]])?;Ok(())}).unwrap();
    let trigger = current(&state, &chat);
    let plan = plan(&state, &trigger, &config).unwrap().unwrap();
    let sources: serde_json::Value = serde_json::from_str(&plan.source_json).unwrap();
    assert_eq!(sources[0]["assistant"], "已确认句段。");
    assert!(sources[1].get("assistant").is_none());
    assert!(sources[1]["user"].as_str().unwrap().contains("用户问题1"));
    assert!(!plan.source_json.contains("未确认候选勿当成已发送"));
    assert_eq!(sources[0]["delivery"]["parts"][1]["status"], "unknown");
    assert!(commit(&state, &plan, "用户的问题尚未完全回答。", &config).unwrap());
    // This is the management API's autocommit read path.
    assert!(state
        .db(|db| context(db, &chat, "owner"))
        .unwrap()
        .is_some());
}

#[test]
fn summary_does_not_promote_suppressed_local_candidate_into_confirmed_conversation() {
    let (_dir, state, config) = fixture();
    let chat = conversation(&state, "local");
    let ids = seed(&state, &chat, 12, "local");
    state.db(|db|{
        db.execute("INSERT INTO run_publication_decisions(run_id,conversation_id,owner_id,speaker_scope,first_seq,last_seq,eligible_plain_text,effects_started,decision,revision,reason,created_ms,updated_ms)
            VALUES(?1,?2,'owner','owner',1,1,1,0,'suppressed',2,'explicit_correction',0,0)",params![ids[0],chat])?;Ok(())
    }).unwrap();
    let trigger = current(&state, &chat);
    let plan = plan(&state, &trigger, &config).unwrap().unwrap();
    let sources: serde_json::Value = serde_json::from_str(&plan.source_json).unwrap();
    assert!(sources[0].get("assistant").is_none());
    assert_eq!(sources[0]["delivery"]["publication_allowed"], false);
    assert!(sources[0]["user"].as_str().unwrap().contains("用户问题0"));
}

#[test]
fn late_receipt_fences_inflight_summary_without_advancing_the_user_history_floor() {
    let (_dir, state, config) = fixture();
    let chat = conversation(&state, "qq");
    let ids = seed(&state, &chat, 12, "qq");
    let trigger = current(&state, &chat);
    let planned = plan(&state, &trigger, &config).unwrap().unwrap();
    accept_late_part(&state, &ids[0]);
    assert!(!still_current(&state, &planned).unwrap());
    assert!(!commit(&state, &planned, "基于旧投递证据的过时摘要", &config).unwrap());
    assert_eq!(
        state
            .db(|db| crate::memory_context::history_floor(db, &chat))
            .unwrap(),
        0
    );
    assert!(state
        .db(|db| context(db, &chat, "owner"))
        .unwrap()
        .is_none());
}

#[test]
fn late_receipt_in_first_summary_batch_invalidates_cumulative_second_revision_and_rebuilds() {
    let (_dir, state, config) = fixture();
    let chat = conversation(&state, "qq");
    let ids = seed(&state, &chat, 12, "qq");
    let first_trigger = current(&state, &chat);
    let first = plan(&state, &first_trigger, &config).unwrap().unwrap();
    assert!(commit(&state, &first, "首批实际已确认内容", &config).unwrap());
    settle(&state, &first_trigger);
    seed(&state, &chat, 8, "qq");
    let second_trigger = current(&state, &chat);
    let second = plan(&state, &second_trigger, &config).unwrap().unwrap();
    assert_eq!(second.previous_revision, Some(1));
    assert!(!second.source_ids.contains(&ids[0]));
    assert!(commit(&state, &second, "累计摘要包含首批来源", &config).unwrap());
    let summary = state.db(|db| context(db, &chat, "owner")).unwrap().unwrap();
    assert_eq!(summary.source_count, 16);
    assert!(!summary.source_run_ids.contains(&ids[0]));
    accept_late_part(&state, &ids[0]);
    assert!(state
        .db(|db| context(db, &chat, "owner"))
        .unwrap()
        .is_none());
    assert_eq!(
        state
            .db(|db| crate::memory_context::history_floor(db, &chat))
            .unwrap(),
        0
    );
    settle(&state, &second_trigger);
    let third_trigger = current(&state, &chat);
    let rebuilt = plan(&state, &third_trigger, &config).unwrap().unwrap();
    assert!(rebuilt.previous.is_none());
    assert_eq!(rebuilt.source_ids[0], ids[0]);
    assert!(rebuilt.source_json.contains("未确认候选勿当成已发送"));
    assert!(commit(&state, &rebuilt, "重新从实际投递来源构建", &config).unwrap());
    let new_summary = state.db(|db| context(db, &chat, "owner")).unwrap().unwrap();
    assert_eq!(new_summary.revision, 3);
    assert_eq!(new_summary.source_count, 8);
}

#[test]
fn historical_raw_summary_stays_audit_only_and_does_not_block_new_delivery_based_summary() {
    let (_dir, state, config) = fixture();
    let chat = conversation(&state, "qq");
    seed(&state, &chat, 12, "qq");
    let trigger = current(&state, &chat);
    let original = plan(&state, &trigger, &config).unwrap().unwrap();
    assert!(commit(&state, &original, "模拟旧版摘要审计正文", &config).unwrap());
    state
        .db(|db| {
            db.execute(
                "DELETE FROM delivery_context_snapshots WHERE kind='summary_revision'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(state
        .db(|db| context(db, &chat, "owner"))
        .unwrap()
        .is_none());
    assert_eq!(
        state
            .db(|db| Ok(db.query_row(
                "SELECT content FROM conversation_summaries WHERE conversation_id=?1",
                [&chat],
                |r| r.get::<_, String>(0)
            )?))
            .unwrap(),
        "模拟旧版摘要审计正文"
    );
    settle(&state, &trigger);
    let next = current(&state, &chat);
    let rebuilt = plan(&state, &next, &config).unwrap().unwrap();
    assert!(rebuilt.previous.is_none());
    assert!(commit(&state, &rebuilt, "可用的新投递摘要", &config).unwrap());
    assert!(state
        .db(|db| context(db, &chat, "owner"))
        .unwrap()
        .is_some());
}
