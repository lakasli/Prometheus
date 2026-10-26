use super::*;

#[tokio::test]
async fn suppressed_candidates_remain_audit_only_across_all_conversation_read_apis() {
    let directory = TempDir::new().unwrap();
    let state = open(directory.path());
    let (cookie, csrf) = bootstrap(&state).await;
    let (_, created, _) = request(
        &state,
        "POST",
        "/api/admin/v1/conversations",
        json!({"title":"publication proof"}),
        Some(&cookie),
        Some(&csrf),
        Some(ORIGIN),
    )
    .await;
    let conversation = created["id"].as_str().unwrap();
    let owner = state
        .db(|db| {
            Ok(db.query_row(
                "SELECT owner_id FROM conversations WHERE id=?1",
                [conversation],
                |r| r.get::<_, String>(0),
            )?)
        })
        .unwrap();
    let old = worker::enqueue(
        &state,
        conversation,
        &owner,
        "original",
        "请按今天制定计划",
        None,
    )
    .unwrap();
    state
        .db(|db| {
            db.execute("UPDATE runs SET status='running' WHERE id=?1", [&old])?;
            Ok(())
        })
        .unwrap();
    let successor = worker::enqueue(
        &state,
        conversation,
        &owner,
        "correction",
        "等等，改成明天",
        None,
    )
    .unwrap();
    state.db(|db| {
        db.execute("UPDATE runs SET status='completed',response='UNPUBLISHED_PRIVATE_CANDIDATE' WHERE id=?1",[&old])?;
        db.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES('audit-candidate',?1,?2,'assistant','UNPUBLISHED_PRIVATE_CANDIDATE',?3)",params![conversation,old,now()])?;
        Ok(())
    }).unwrap();
    let run_path = format!("/api/admin/v1/runs/{old}");
    let (status, record, _) = request(
        &state,
        "GET",
        &run_path,
        Value::Null,
        Some(&cookie),
        None,
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(record["status"], "completed");
    assert_eq!(record["publication"]["state"], "suppressed");
    assert!(record["response"].is_null() && record["reply_plan"].is_null());
    let (_, list, _) = request(
        &state,
        "GET",
        "/api/admin/v1/runs",
        Value::Null,
        Some(&cookie),
        None,
        Some(ORIGIN),
    )
    .await;
    assert!(!list.to_string().contains("UNPUBLISHED_PRIVATE_CANDIDATE"));
    let path = format!("/api/admin/v1/conversations/{conversation}/messages");
    let (_, messages, _) = request(
        &state,
        "GET",
        &path,
        Value::Null,
        Some(&cookie),
        None,
        Some(ORIGIN),
    )
    .await;
    assert!(!messages
        .to_string()
        .contains("UNPUBLISHED_PRIVATE_CANDIDATE"));
    assert!(messages.to_string().contains("等等，改成明天"));
    state
        .db(|db| {
            assert_eq!(
                db.query_row("SELECT response FROM runs WHERE id=?1", [&old], |r| r
                    .get::<_, String>(0))?,
                "UNPUBLISHED_PRIVATE_CANDIDATE"
            );
            let details:String=db.query_row("SELECT details_json FROM runtime_log_events WHERE run_id=?1 AND reference_type='publication' AND status='suppressed'",[&old],|r|r.get(0))?;
            assert!(details.contains("explicit_correction") && !details.contains("UNPUBLISHED_PRIVATE_CANDIDATE"));
            assert_eq!(
                db.query_row("SELECT status FROM runs WHERE id=?1", [successor], |r| {
                    r.get::<_, String>(0)
                })?,
                "queued"
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM outbox WHERE run_id=?1", [old], |r| {
                    r.get::<_, i64>(0)
                })?,
                0
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn image_admission_cannot_use_its_caption_as_a_plain_text_correction() {
    let directory = TempDir::new().unwrap();
    let state = open(directory.path());
    state.db(|db|{db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('nontext','owner','synthetic',0)",[])?;Ok(())}).unwrap();
    let old = worker::enqueue(&state, "nontext", "owner", "one", "请计算原始问题", None).unwrap();
    state
        .db(|db| {
            let tx = db.transaction()?;
            tx.execute("UPDATE runs SET status='running' WHERE id=?1", [&old])?;
            let image =
                worker::enqueue_non_text_in(&tx, "nontext", "owner", "image", "等等，改成明天")?
                    .unwrap();
            assert!(crate::turn_control::publication_allowed_in(&tx, &old)?);
            assert!(crate::turn_control::correction_context_in(&tx, &image)?.is_none());
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}
