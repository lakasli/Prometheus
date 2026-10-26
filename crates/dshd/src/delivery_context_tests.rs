use super::*;

fn database() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../schema.sql")).unwrap();
    db.execute_batch(
        "CREATE TABLE outbox_reply_parts(outbox_id TEXT NOT NULL,seq INTEGER NOT NULL,
           payload_json TEXT NOT NULL,status TEXT NOT NULL,platform_message_id TEXT,error TEXT,
           finished_ms INTEGER,PRIMARY KEY(outbox_id,seq));
         CREATE TABLE weixin_outbox(id TEXT PRIMARY KEY,run_id TEXT NOT NULL UNIQUE,
           status TEXT NOT NULL,error_code TEXT,server_message_id TEXT,updated_at INTEGER NOT NULL);
         CREATE TABLE memory_context_watermarks(conversation_id TEXT PRIMARY KEY,
           through_run_rowid INTEGER NOT NULL,updated_at INTEGER NOT NULL);
         CREATE TABLE run_publication_decisions(run_id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,
           conversation_id TEXT NOT NULL,decision TEXT NOT NULL,revision INTEGER NOT NULL);
         CREATE TABLE run_prompt_snapshots(run_id TEXT PRIMARY KEY,messages_json TEXT NOT NULL);
         INSERT INTO conversations VALUES('qq','owner','QQ','qq:account:c2c:peer',0);
         INSERT INTO conversations VALUES('wx','owner','Weixin','weixin:account:c2c:peer',0);
         INSERT INTO conversations VALUES('web','owner','Web',NULL,0);
         INSERT INTO conversations VALUES('other','different-owner','Web',NULL,0);",
    )
    .unwrap();
    ensure_schema(&db).unwrap();
    db
}

fn run(db: &Connection, id: &str, conversation: &str, status: &str, response: &str) {
    db.execute(
        "INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,response,created_at,updated_at)
         SELECT ?1,id,owner_id,?1,'用户尚待回答的问题','{}',?3,?4,0,0 FROM conversations WHERE id=?2",
        params![id, conversation, status, response],
    )
    .unwrap();
}

fn outbox(db: &Connection, id: &str, status: &str, receipt: Option<&str>) {
    db.execute(
        "INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,platform_message_id,deadline,created_at)
         SELECT ?1,?1,'account','secret-ref','{}',response,?2,?3,100,0 FROM runs WHERE id=?1",
        params![id, status, receipt],
    )
    .unwrap();
}

fn part(
    db: &Connection,
    id: &str,
    seq: i64,
    kind: &str,
    text: &str,
    state: &str,
    receipt: Option<&str>,
) {
    db.execute(
        "INSERT INTO outbox_reply_parts VALUES(?1,?2,?3,?4,?5,NULL,10)",
        params![
            id,
            seq,
            serde_json::to_string(&serde_json::json!({"type":kind,"text":text})).unwrap(),
            state,
            receipt
        ],
    )
    .unwrap();
}

#[test]
fn only_receipted_qq_segments_become_history_and_user_question_survives() {
    let mut db = database();
    run(
        &db,
        "r",
        "qq",
        "completed",
        "第一句。未发送秘密。结果未知秘密。",
    );
    outbox(&db, "r", "unknown", Some("first-platform-id"));
    part(
        &db,
        "r",
        1,
        "text",
        "第一句。",
        "sent",
        Some("first-platform-id"),
    );
    part(&db, "r", 2, "text", "未发送秘密。", "queued", None);
    part(&db, "r", 3, "text", "结果未知秘密。", "unknown", None);
    let tx = db.transaction().unwrap();
    let p = project_run_in(&tx, "owner", "qq", "r").unwrap();
    assert_eq!(p.user_content, "用户尚待回答的问题");
    assert_eq!(p.assistant_content.as_deref(), Some("第一句。"));
    assert_eq!(
        p.delivery
            .parts
            .iter()
            .map(|p| p.status.as_str())
            .collect::<Vec<_>>(),
        ["sent", "queued", "unknown"]
    );
    let metadata = serde_json::to_string(&p.delivery).unwrap();
    assert!(!metadata.contains("秘密"));
    assert!(!metadata.contains("first-platform-id"));
    tx.commit().unwrap();
}

#[test]
fn pending_failed_expired_cancelled_and_unconfirmed_sent_never_use_generated_candidate() {
    let mut db = database();
    for state in [
        "queued",
        "sending",
        "unknown",
        "failed",
        "expired",
        "cancelled",
        "sent",
    ] {
        run(&db, state, "qq", "completed", "绝不伪装已发的候选");
        outbox(&db, state, state, None);
        part(&db, state, 1, "text", "绝不伪装已发的候选", state, None);
    }
    let tx = db.transaction().unwrap();
    for state in [
        "queued",
        "sending",
        "unknown",
        "failed",
        "expired",
        "cancelled",
        "sent",
    ] {
        let p = project_run_in(&tx, "owner", "qq", state).unwrap();
        assert!(p.assistant_content.is_none(), "{state}");
        assert!(!p.user_content.is_empty());
    }
}

#[test]
fn late_receipt_changes_future_projection_but_never_frozen_prompt_or_metadata() {
    let mut db = database();
    run(&db, "source", "qq", "completed", "一。二。");
    run(&db, "target", "qq", "running", "");
    run(&db, "next-target", "qq", "running", "");
    outbox(&db, "source", "queued", None);
    part(&db, "source", 1, "text", "一。", "sent", Some("one"));
    part(&db, "source", 2, "text", "二。", "sending", None);
    let original = {
        let tx = db.transaction().unwrap();
        let p = project_run_in(&tx, "owner", "qq", "source").unwrap();
        let snapshot = freeze_snapshot_in(&tx, "target", &[p], 100).unwrap();
        tx.execute(
            "INSERT INTO run_prompt_snapshots VALUES('target','original prompt: 一。')",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
        snapshot
    };
    db.execute("UPDATE outbox_reply_parts SET status='sent',platform_message_id='two',finished_ms=200 WHERE seq=2", []).unwrap();
    {
        let tx = db.transaction().unwrap();
        let p = project_run_in(&tx, "owner", "qq", "source").unwrap();
        assert_eq!(p.assistant_content.as_deref(), Some("一。\n二。"));
        assert!(!validate_sources_in(&tx, "owner", "qq", &original.sources).unwrap());
        let repeated = freeze_snapshot_in(&tx, "target", std::slice::from_ref(&p), 300).unwrap();
        assert_eq!(repeated, original);
        let next = freeze_snapshot_in(&tx, "next-target", &[p], 300).unwrap();
        assert_ne!(
            next.sources[0].delivery_revision,
            original.sources[0].delivery_revision
        );
        assert_ne!(next.digest, original.digest);
        let prompt: String = tx
            .query_row(
                "SELECT messages_json FROM run_prompt_snapshots WHERE run_id='target'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(prompt, "original prompt: 一。");
        tx.commit().unwrap();
    }
}

#[test]
fn channel_legacy_record_requires_evidence_but_local_completion_is_available() {
    let mut db = database();
    run(&db, "qq-no-outbox", "qq", "completed", "生成而未证明投递");
    run(&db, "wx-no-outbox", "wx", "completed", "生成而未证明投递");
    run(&db, "web-complete", "web", "completed", "本地可用回答");
    run(&db, "web-failed", "web", "failed", "失败时残留候选");
    run(&db, "legacy-qq", "qq", "completed", "旧版实际文本");
    outbox(&db, "legacy-qq", "sent", Some("legacy-receipt"));
    let tx = db.transaction().unwrap();
    for (id, conv) in [
        ("qq-no-outbox", "qq"),
        ("wx-no-outbox", "wx"),
        ("web-failed", "web"),
    ] {
        assert!(project_run_in(&tx, "owner", conv, id)
            .unwrap()
            .assistant_content
            .is_none());
    }
    let local = project_run_in(&tx, "owner", "web", "web-complete").unwrap();
    assert!(local.delivery.local_completion);
    assert_eq!(local.assistant_content.as_deref(), Some("本地可用回答"));
    let legacy = project_run_in(&tx, "owner", "qq", "legacy-qq").unwrap();
    assert_eq!(legacy.assistant_content.as_deref(), Some("旧版实际文本"));
    assert_eq!(legacy.delivery.parts[0].seq, None);
}

#[test]
fn qq_media_fallback_projects_exact_text_and_does_not_claim_media_or_caption_was_sent() {
    let mut db = database();
    run(&db, "media", "qq", "completed", "未发图片描述与视频描述");
    outbox(&db, "media", "sent", Some("video-fallback"));
    part(
        &db,
        "media",
        1,
        "voice",
        "朗读句。",
        "sent",
        Some("voice-fallback"),
    );
    part(
        &db,
        "media",
        2,
        "image",
        "未发图片描述",
        "sent",
        Some("image-fallback"),
    );
    part(
        &db,
        "media",
        3,
        "video",
        "未发视频描述",
        "sent",
        Some("video-fallback"),
    );
    db.execute("UPDATE outbox_reply_parts SET error='媒体发送失败后已改发文字；原因：QQ预上传返回HTTP 400'", []).unwrap();
    let tx = db.transaction().unwrap();
    let p = project_run_in(&tx, "owner", "qq", "media").unwrap();
    assert_eq!(
        p.assistant_content.as_deref(),
        Some("（语音暂不可用）朗读句。\n图片发送暂不可用。\n视频发送暂不可用。")
    );
    assert!(p.delivery.parts.iter().all(|part| part.text_fallback));
}

#[test]
fn accepted_media_keeps_only_actual_caption_and_tracks_media_kind_without_read_claim() {
    let mut db = database();
    run(&db, "media", "qq", "completed", "整段生成候选");
    outbox(&db, "media", "queued", None);
    part(
        &db,
        "media",
        1,
        "image",
        "实际发送的标题。",
        "sent",
        Some("image-receipt"),
    );
    part(&db, "media", 2, "voice", "还没发的声音。", "queued", None);
    let tx = db.transaction().unwrap();
    let p = project_run_in(&tx, "owner", "qq", "media").unwrap();
    assert_eq!(p.assistant_content.as_deref(), Some("实际发送的标题。"));
    assert_eq!(p.delivery.parts[0].kind, "image");
    assert!(p.delivery.parts[0].platform_accepted);
}

#[test]
fn weixin_uses_its_business_acceptance_and_preserves_oversize_fallback() {
    let mut db = database();
    for (id, state, code, text) in [
        ("accepted", "accepted", None, "微信文本。"),
        (
            "oversize",
            "accepted",
            Some("original_response_too_large"),
            "没真正发出的长回答",
        ),
        ("unknown", "unknown", None, "不可假定已发"),
        ("failed", "failed", None, "不可假定已发"),
    ] {
        run(&db, id, "wx", "completed", text);
        db.execute(
            "INSERT INTO weixin_outbox VALUES(?1,?1,?2,?3,NULL,10)",
            params![id, state, code],
        )
        .unwrap();
    }
    let tx = db.transaction().unwrap();
    let accepted = project_run_in(&tx, "owner", "wx", "accepted").unwrap();
    assert_eq!(accepted.assistant_content.as_deref(), Some("微信文本。"));
    assert!(accepted.delivery.parts[0].platform_accepted);
    assert!(accepted.delivery.parts[0].receipt_digest.is_none());
    assert_eq!(
        project_run_in(&tx, "owner", "wx", "oversize")
            .unwrap()
            .assistant_content
            .as_deref(),
        Some(WEIXIN_OVERSIZE_REPLY)
    );
    for id in ["unknown", "failed"] {
        assert!(project_run_in(&tx, "owner", "wx", id)
            .unwrap()
            .assistant_content
            .is_none());
    }
}

#[test]
fn authorization_watermark_invalidates_sources_and_metadata_without_changing_receipts() {
    let mut db = database();
    run(&db, "source", "qq", "completed", "原回答");
    run(&db, "target", "qq", "running", "");
    outbox(&db, "source", "sent", Some("original-receipt"));
    let original = {
        let tx = db.transaction().unwrap();
        let p = project_run_in(&tx, "owner", "qq", "source").unwrap();
        let snapshot = freeze_snapshot_in(&tx, "target", &[p], 100).unwrap();
        tx.commit().unwrap();
        snapshot
    };
    {
        let tx = db.transaction().unwrap();
        tx.execute("INSERT INTO memory_context_watermarks SELECT 'qq',max(rowid),20 FROM runs WHERE conversation_id='qq'", []).unwrap();
        assert!(!validate_sources_in(&tx, "owner", "qq", &original.sources).unwrap());
        assert!(snapshot_in(&tx, "run", "target")
            .unwrap()
            .unwrap()
            .invalidated_at
            .is_some());
        assert!(freeze_snapshot_in(&tx, "target", &[], 300).is_err());
        assert_eq!(
            tx.query_row(
                "SELECT platform_message_id FROM outbox WHERE id='source'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "original-receipt"
        );
        tx.commit().unwrap();
    }
}

#[test]
fn projection_and_snapshot_fail_closed_on_cross_owner_and_cross_conversation_sources() {
    let mut db = database();
    run(&db, "source", "web", "completed", "私有回答");
    run(&db, "target", "qq", "running", "");
    assert!(project_run_in(&db, "owner", "web", "source").is_err());
    let tx = db.transaction().unwrap();
    assert!(project_run_in(&tx, "different-owner", "web", "source").is_err());
    assert!(project_run_in(&tx, "owner", "qq", "source").is_err());
    let p = project_run_in(&tx, "owner", "web", "source").unwrap();
    assert!(freeze_snapshot_in(&tx, "target", &[p], 100).is_err());
}

#[test]
fn snapshots_join_caller_transaction_and_support_nonrun_derived_sources_without_body_copies() {
    let mut db = database();
    run(&db, "source", "web", "completed", "PRIVATE_ASSISTANT_BODY");
    {
        let tx = db.transaction().unwrap();
        let p = project_run_in(&tx, "owner", "web", "source").unwrap();
        snapshot_sources_in(&tx, "summary", "operation-id", "owner", "web", &[p], 100).unwrap();
        let raw: String = tx
            .query_row(
                "SELECT snapshot_json FROM delivery_context_snapshots",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!raw.contains("PRIVATE_ASSISTANT_BODY"));
        assert!(!raw.contains("用户尚待回答的问题"));
        tx.rollback().unwrap();
    }
    assert!(snapshot_in(&db, "summary", "operation-id")
        .unwrap()
        .is_none());
}

#[test]
fn metadata_revision_tracks_new_receipts_survives_reinitialize_and_transaction_rollback() {
    let mut db = database();
    run(&db, "source", "qq", "completed", "回答");
    outbox(&db, "source", "queued", None);
    let before = revision(&db, "source").unwrap();
    {
        let tx = db.transaction().unwrap();
        tx.execute(
            "UPDATE outbox SET status='sent',platform_message_id='receipt'",
            [],
        )
        .unwrap();
        assert!(revision(&tx, "source").unwrap() > before);
        tx.rollback().unwrap();
    }
    assert_eq!(revision(&db, "source").unwrap(), before);
    ensure_schema(&db).unwrap();
    assert_eq!(revision(&db, "source").unwrap(), before);
}

#[test]
fn media_projection_uses_actual_image_caption_limit_and_never_invents_voice_or_video_text() {
    let mut db = database();
    run(&db, "media", "qq", "completed", "不能直接使用整段生成结果");
    outbox(&db, "media", "sent", Some("receipt"));
    part(
        &db,
        "media",
        1,
        "image",
        &"图".repeat(1501),
        "sent",
        Some("image"),
    );
    part(
        &db,
        "media",
        2,
        "voice",
        "未经确证的合成文字",
        "sent",
        Some("voice"),
    );
    part(
        &db,
        "media",
        3,
        "video",
        "视频没有实际发送的文字标题",
        "sent",
        Some("video"),
    );
    let tx = db.transaction().unwrap();
    let p = project_run_in(&tx, "owner", "qq", "media").unwrap();
    assert_eq!(p.assistant_content, Some("图".repeat(1500)));
    assert!(p.delivery.parts.iter().all(|part| part.platform_accepted));
    assert!(p.delivery.parts[1].content_digest.is_none());
    assert!(p.delivery.parts[2].content_digest.is_none());
}

#[test]
fn publication_suppression_removes_local_candidate_but_preserves_actual_platform_receipts() {
    let mut db = database();
    run(&db, "local", "web", "completed", "被明确更正抑制的旧答案");
    run(&db, "remote", "qq", "completed", "已经取得平台回执的旧句段");
    outbox(&db, "remote", "sent", Some("receipt"));
    for (id, conversation) in [("local", "web"), ("remote", "qq")] {
        db.execute(
            "INSERT INTO run_publication_decisions VALUES(?1,'owner',?2,'suppressed',2)",
            params![id, conversation],
        )
        .unwrap();
    }
    let tx = db.transaction().unwrap();
    let local = project_run_in(&tx, "owner", "web", "local").unwrap();
    assert!(!local.delivery.publication_allowed);
    assert!(!local.delivery.local_completion);
    assert!(local.assistant_content.is_none());
    let remote = project_run_in(&tx, "owner", "qq", "remote").unwrap();
    assert!(!remote.delivery.publication_allowed);
    assert_eq!(
        remote.assistant_content.as_deref(),
        Some("已经取得平台回执的旧句段")
    );
}

#[test]
fn source_content_and_publication_changes_fence_previously_projected_sources() {
    let mut db = database();
    run(&db, "source", "web", "completed", "原回答");
    db.execute(
        "INSERT INTO run_publication_decisions VALUES('source','owner','web','active',1)",
        [],
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    let p = project_run_in(&tx, "owner", "web", "source").unwrap();
    tx.execute(
        "UPDATE runs SET response='修正后回答' WHERE id='source'",
        [],
    )
    .unwrap();
    assert!(!validate_sources_in(&tx, "owner", "web", &[p.delivery]).unwrap());
    let p = project_run_in(&tx, "owner", "web", "source").unwrap();
    tx.execute("UPDATE run_publication_decisions SET decision='suppressed',revision=revision+1 WHERE run_id='source'", []).unwrap();
    assert!(!validate_sources_in(&tx, "owner", "web", &[p.delivery]).unwrap());
}

#[test]
fn diary_capture_projects_authorized_original_delivery_independent_of_conversation_floor() {
    let mut db = database();
    run(&db, "source", "qq", "completed", "原始回答");
    outbox(&db, "source", "sent", Some("receipt"));
    let tx = db.transaction().unwrap();
    tx.execute("INSERT INTO memory_context_watermarks SELECT 'qq',max(rowid),20 FROM runs WHERE conversation_id='qq'",[]).unwrap();
    assert!(project_run_in(&tx, "owner", "qq", "source").is_err());
    let projection = project_capture_run_in(&tx, "owner", "qq", "source").unwrap();
    assert_eq!(projection.assistant_content.as_deref(), Some("原始回答"));
    assert!(!validate_sources_in(
        &tx,
        "owner",
        "qq",
        std::slice::from_ref(&projection.delivery)
    )
    .unwrap());
    assert!(validate_capture_sources_in(
        &tx,
        "owner",
        "qq",
        std::slice::from_ref(&projection.delivery)
    )
    .unwrap());
    let snapshot = snapshot_sources_in(
        &tx,
        "diary_capture",
        "event-ref",
        "owner",
        "qq",
        &[projection],
        100,
    )
    .unwrap();
    assert!(snapshot.history_floor > 0);
    assert!(project_capture_run_in(&tx, "different-owner", "qq", "source").is_err());
}
