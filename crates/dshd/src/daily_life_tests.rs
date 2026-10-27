use super::*;
use crate::{daily_life as life, rest_store as rest, schedule};

fn configure(f: &Fixture, at: i64) {
    f.state
        .db(|db| {
            let tx = db.transaction()?;
            let admin = rest::requester_for_admin_in(&tx, &f.admin)?;
            rest::save_policy_in(
                &tx,
                &admin,
                rest::policy_in(&tx)?.0,
                &rest::RestPolicy {
                    schedule_enabled: true,
                    sleep_local: Some("23:00".into()),
                    wake_local: Some("07:00".into()),
                    anticipation_enabled: true,
                    reset_context_on_sleep: true,
                    ..Default::default()
                },
                at,
            )?;
            life::save_personal_policy(&tx, &f.admin.id, 0, true, 4, at)?;
            tx.execute(
                "UPDATE trigger_runtime SET clock_trusted=1,space_ok=1 WHERE id=1",
                [],
            )?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}
fn choice(decision: &str, delay: i64) -> life::Choice {
    life::Choice {
        decision: decision.into(),
        delay_minutes: delay,
    }
}

#[tokio::test]
async fn plain_qq_statement_is_semantically_committed_before_publication() {
    let f = fixture().await;
    configure(&f, now());
    f.state
        .db(|db| {
            crate::diary::save_policy(
                db,
                &f.admin.id,
                0,
                &crate::diary::SourcePolicy {
                    source_owner_id: "owner".into(),
                    enabled: true,
                    allow_model: false,
                    timezone: "Asia/Shanghai".into(),
                },
            )
            .map_err(|_| anyhow::anyhow!("synthetic diary policy failed"))?;
            Ok(())
        })
        .unwrap();
    *f.model.response.lock().unwrap() =
        json!([{"type":"text","text":"我趁这会儿去把《鹿柴》抄一抄，抄完也歇。","delay_ms":0}])
            .to_string();
    *f.model.plan_response.lock().unwrap()=json!({"self_activity":{"slot_id":"$first_slot","title":"文字记录 SOURCE_A_TITLE","content":"私有文字成果标记 SOURCE_A_ONLY"}}).to_string();
    let current = start(&f.state, "a", "你自己安排十分钟抄诗。");
    let run = current.input.id.clone();
    process(&f.state, current, &CancellationToken::new()).await;
    f.state.db(|db| {
        assert_eq!(db.query_row("SELECT status FROM runs WHERE id=?1",[&run],|r|r.get::<_,String>(0))?,"completed");
        assert_eq!(db.query_row("SELECT poem_id FROM daily_life_activities WHERE source_run_id=?1",[&run],|r|r.get::<_,String>(0))?,"text_v1");
        assert_eq!(db.query_row("SELECT count(*) FROM audit_events WHERE action='schedule.personal.reply_recovered' AND resource=?1",[&run],|r|r.get::<_,i64>(0))?,0);
        Ok(())
    }).unwrap();
    let other = enqueue(
        &f.state,
        "b",
        "second-owner",
        "privacy-other",
        "普通对话",
        None,
    )
    .unwrap();
    f.state.db(|db| {let tx=db.transaction()?;
        let start=now();
        tx.execute("UPDATE schedule_activities SET definition_json=json_set(definition_json,'$.start_at',?1,'$.end_at',?2) WHERE id=(SELECT activity_id FROM daily_life_activities WHERE source_run_id=?3)",params![start,start+600,run])?;
        schedule::tick_in(&tx,start,false).map_err(|e| {eprintln!("generic scheduled execution failed: {e:#}");e})?;
        let saved:String=tx.query_row("SELECT o.result_json FROM schedule_occurrences o JOIN daily_life_activities l ON l.activity_id=o.activity_id WHERE l.source_run_id=?1",[&run],|r|r.get(0))?;
        assert!(saved.contains("SOURCE_A_ONLY"));
        assert_eq!(tx.query_row("SELECT count(*) FROM diary_events WHERE visibility='shared' OR content LIKE '%SOURCE_A_ONLY%'",[],|r|r.get::<_,i64>(0))?,0);
        assert_eq!(tx.query_row("SELECT count(*) FROM diary_events WHERE origin_type='schedule_activity' AND source_owner_id='owner'",[],|r|r.get::<_,i64>(0))?,1);
        assert!(!life::prompt_in(&tx,&other,start)?.contains("SOURCE_A_TITLE"));
        tx.execute("UPDATE runs SET status='cancelled' WHERE id=?1",[&other])?;
        tx.commit()?;Ok(())}).unwrap();
    // A new explicit promise after revocation must not become a message.
    f.state
        .db(|db| {
            let tx = db.transaction()?;
            life::save_personal_policy(&tx, &f.admin.id, 1, false, 4, now())?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    let blocked = start(&f.state, "a", "再安排一次。");
    let blocked_run = blocked.input.id.clone();
    process(&f.state, blocked, &CancellationToken::new()).await;
    f.state
        .db(|db| {
            assert_eq!(
                db.query_row("SELECT status FROM runs WHERE id=?1", [&blocked_run], |r| r
                    .get::<_, String>(0))?,
                "failed"
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM messages WHERE run_id=?1 AND role='assistant'",
                    [&blocked_run],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM daily_life_activities", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn generic_offer_uses_standard_grants_without_hobby_prompts_or_source_leakage() {
    let f = fixture().await;
    configure(&f, now());
    let current = start(&f.state, "a", "给自己安排一件事。");
    f.state
        .db(|db| {
            let tx = db.transaction()?;
            let prompt = life::prompt_in(&tx, &current.input.id, now())?;
            for token in ["抄诗", "鹿柴", "春晓", "poem_id", "catalog"] {
                assert!(!prompt.contains(token), "{token}");
            }
            assert!(prompt.contains("保存一份文字成果"));
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn generic_plan_check_reuses_checkpoint_and_never_retries_an_unknown_attempt() {
    let f = fixture().await;
    configure(&f, now());
    let current = start(&f.state, "a", "普通文字安排。");
    let run = current.input.id.clone();
    prepare_messages(&f.state, &current.input).unwrap();
    let raw = json!([{"type":"text","text":"我趁这会儿先把想法整理成一段记录。","delay_ms":0}])
        .to_string();
    assert!(crate::rest_runtime::cache_generation(&f.state, &run, &raw).unwrap());
    *f.model.plan_response.lock().unwrap()=json!({"self_activity":{"slot_id":"$first_slot","title":"整理想法","content":"一份独立文字成果。"}}).to_string();
    let first = life::complete_reply_plan(&f.state, &run, &current.input.config, raw.clone())
        .await
        .unwrap();
    assert_eq!(
        life::complete_reply_plan(&f.state, &run, &current.input.config, raw.clone())
            .await
            .unwrap(),
        first
    );
    assert_eq!(f.model.calls.load(Ordering::SeqCst), 1);
    f.state.db(|db| {assert_eq!(crate::rest_runtime::cached_generation_in(db,&run)?.as_deref(),Some(raw.as_str()));
        db.execute("UPDATE rest_deferred_presentations SET self_plan_status='checking',self_plan_json=NULL WHERE run_id=?1",[&run])?;
        life::initialize_plan_cache(db)?;Ok(())}).unwrap();
    assert!(
        life::complete_reply_plan(&f.state, &run, &current.input.config, raw)
            .await
            .is_err()
    );
    assert_eq!(f.model.calls.load(Ordering::SeqCst), 1);
    f.state.db(|db| {db.execute("DELETE FROM rest_deferred_presentations WHERE run_id=?1",[&run])?;assert_eq!(db.query_row("SELECT count(*) FROM rest_deferred_presentations WHERE self_plan_json IS NOT NULL",[],|r|r.get::<_,i64>(0))?,0);Ok(())}).unwrap();
}

#[tokio::test]
async fn qq_jpeg_reaches_vision_native_and_generic_schedule_and_failure_notice_stays_human() {
    let f = fixture().await;
    configure(&f, now());
    let config = f
        .state
        .db(|db| {
            let (revision, mut c) = character::current_config_in(db)?;
            c.qq.enabled = true;
            c.qq.app_id = "synthetic-app".into();
            c.qq.credential_ref = Some("synthetic-qq-ref".into());
            db.execute(
                "UPDATE config_revisions SET config_json=?1 WHERE revision=?2",
                params![serde_json::to_string(&c)?, revision],
            )?;
            Ok(c)
        })
        .unwrap();
    *f.model.response.lock().unwrap() =
        json!([{"type":"text","text":"我趁这会儿去把《鹿柴》抄一抄，抄完也歇。","delay_ms":0}])
            .to_string();
    *f.model.plan_response.lock().unwrap()=json!({"self_activity":{"slot_id":"$first_slot","title":"整理一段文字","content":"SOURCE_QQ_PRIVATE_TEXT"}}).to_string();
    let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        16,
        16,
        image::Rgb([220, 30, 30]),
    ));
    let mut encoded = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut encoded, image::ImageFormat::Jpeg)
        .unwrap();
    let prepared =
        crate::qq::incoming_media::prepare_bytes(Some("image/jpeg"), encoded.into_inner()).await;
    assert!(matches!(
        &prepared,
        crate::qq::incoming_media::Prepared::Image(_)
    ));
    let payload = json!({"t":"C2C_MESSAGE_CREATE","id":"synthetic-jpeg-event","d":{"id":"synthetic-jpeg-msg","content":"看看这个杯子，接下来你自己安排点事吧。","author":{"user_openid":"synthetic_target"}}});
    crate::qq::ingest_prepared(
        &f.state,
        &config,
        &payload,
        now(),
        now() + 3600,
        prepared,
        None,
    )
    .unwrap();
    let claimed = claim(&f.state).unwrap().unwrap();
    let run = claimed.input.id.clone();
    process(&f.state, claimed, &CancellationToken::new()).await;
    f.state
        .db(|db| {
            assert_eq!(
                db.query_row("SELECT status FROM runs WHERE id=?1", [&run], |r| r
                    .get::<_, String>(0))?,
                "completed"
            );
            assert_eq!(
                db.query_row(
                    "SELECT status FROM conversation_images WHERE run_id=?1",
                    [&run],
                    |r| r.get::<_, String>(0)
                )?,
                "described"
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM daily_life_activities WHERE source_run_id=?1",
                    [&run],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM outbox WHERE run_id=?1 AND status='queued'",
                    [&run],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            Ok(())
        })
        .unwrap();
    let payload = json!({"t":"C2C_MESSAGE_CREATE","id":"synthetic-bad-event","d":{"id":"synthetic-bad-msg","content":"","author":{"user_openid":"synthetic_target"}}});
    crate::qq::ingest_prepared(
        &f.state,
        &config,
        &payload,
        now(),
        now() + 3600,
        crate::qq::incoming_media::Prepared::Unavailable {
            code: "media_image_invalid",
            message: "仅PNG/H264 technical diagnostic",
        },
        None,
    )
    .unwrap();
    f.state
        .db(|db| {
            let reply: String = db.query_row(
                "SELECT response FROM runs WHERE client_message_id='synthetic-bad-msg'",
                [],
                |r| r.get(0),
            )?;
            assert!(reply.contains("没打开"));
            for technical in ["PNG", "H264", "模型", "MiB"] {
                assert!(!reply.contains(technical));
            }
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn chat_schedule_is_atomic_real_and_idempotent_without_private_projection() {
    let f = fixture().await;
    // A fixed, in-window test date is independent of the test machine's hour.
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-27T10:00:00+08:00")
        .unwrap()
        .timestamp();
    configure(&f, at);
    let claimed = start(&f.state, "a", "我忙工作，你安排点自己的事吧。");
    let run = claimed.input.id.clone();
    f.state
        .db(|db| {
            let tx = db.transaction()?;
            life::prompt_in(&tx, &run, at)?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    let selection = f
        .state
        .db(|db| {
            let raw: String = db.query_row(
                "SELECT offer_json FROM daily_life_offers WHERE run_id=?1",
                [&run],
                |r| r.get(0),
            )?;
            let offer: Value = serde_json::from_str(&raw)?;
            Ok(life::Selection {
                poem_id: "chunxiao".into(),
                slot_id: offer["slots"][0]["slot_id"].as_str().unwrap().into(),
                ..Default::default()
            })
        })
        .unwrap();
    let saved = f
        .state
        .db(|db| {
            let tx = db.transaction()?;
            let saved = life::commit_selection(&tx, &run, &selection, at)?;
            assert_eq!(life::commit_selection(&tx, &run, &selection, at)?, saved);
            tx.commit()?;
            Ok(saved)
        })
        .unwrap();
    let aid = saved["id"].as_str().unwrap();
    let start_at = saved["definition"]["start_at"].as_i64().unwrap();
    let second = enqueue(
        &f.state,
        "b",
        "second-owner",
        "second-evidence",
        "普通来源隔离检查",
        None,
    )
    .unwrap();
    f.state.db(|db|{let tx=db.transaction()?;
        assert_eq!(schedule::detail(&tx,"owner",aid)?["occurrences"],json!([]));
        schedule::tick_in(&tx,start_at,false)?;
        let detail=schedule::detail(&tx,"owner",aid)?;
        assert_eq!(detail["occurrences"][0]["status"],"completed");
        assert!(detail["occurrences"][0]["result"]["text"].as_str().unwrap().contains("春眠不觉晓"));
        tx.execute("UPDATE schedule_activities SET title='PRIVATE_TITLE',definition_json=json_set(definition_json,'$.purpose','PRIVATE_PURPOSE') WHERE id=?1",[aid])?;
        let prompt=life::prompt_in(&tx,&second,at+1)?;
        assert!(!prompt.contains("PRIVATE_TITLE") && !prompt.contains("PRIVATE_PURPOSE"));
        tx.commit()?;Ok(())}).unwrap();
    drop(claimed);
}

#[tokio::test]
async fn actual_provider_system_has_no_mechanical_identity_and_uses_new_personality() {
    let f = fixture().await;
    let current = start(&f.state, "a", "咱爸叫刘海龙，咱妈叫王瑛。");
    let messages = prepare_messages(&f.state, &current.input).unwrap();
    let system = messages
        .iter()
        .filter(|v| v["role"] == "system")
        .map(|v| v["content"].as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    for forbidden in [
        "AI助手",
        "智能体",
        "人工智能",
        "不预设恋爱、亲属",
        "姓名、喜好和性情属于表达设定",
    ] {
        assert!(!system.contains(forbidden), "{forbidden}");
    }
    assert!(system.contains("你是刘嘉卉") && system.contains("家人姓名"));
    process(&f.state, current, &CancellationToken::new()).await;
    assert_eq!(f.model.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn sleep_cuts_raw_history_summary_focus_and_inflight_without_erasing_logs() {
    let f = fixture().await;
    configure(&f, now());
    let earlier = start(
        &f.state,
        "a",
        "BEFORE_SLEEP_PRIVATE_MARKER，帮我记住这一轮。",
    );
    settled(&f.state, earlier);
    let inflight = start(&f.state, "a", "入睡前最后的问题。");
    let id = inflight.input.id.clone();
    prepare_messages(&f.state, &inflight.input).unwrap();
    f.state.db(|db|{let tx=db.transaction()?;
        life::freeze_context_in(&tx,"synthetic-sleep",now())?;
        assert!(!life::context_valid(&tx,&id)?);
        assert!(!crate::conversation_context::validate_derived_in(&tx,&id)?);
        assert!(tx.query_row("SELECT count(*) FROM messages WHERE content LIKE '%BEFORE_SLEEP_PRIVATE_MARKER%'",[],|r|r.get::<_,i64>(0))?>0);
        tx.commit()?;Ok(())}).unwrap();
    settled(&f.state, inflight);
    let after = start(&f.state, "a", "早上好，今天聊点别的。");
    let messages = prepare_messages(&f.state, &after.input).unwrap();
    assert!(!serde_json::to_string(&messages)
        .unwrap()
        .contains("BEFORE_SLEEP_PRIVATE_MARKER"));
    assert!(serde_json::to_string(&messages).unwrap().contains("早上好"));
    drop(after);
}

#[tokio::test]
async fn routine_notice_cancellation_postponement_restart_and_stale_choice_are_bounded() {
    let f = fixture().await;
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-27T22:59:00+08:00")
        .unwrap()
        .timestamp();
    configure(&f, at);
    f.state
        .db(|db| {
            let tx = db.transaction()?;
            let clock = rest::ClockSample::new(at, 0, "synthetic-clock", true)?;
            life::routine_tick(&tx, &clock, at)?;
            let (d, messages) = life::prepare_decision(&tx, at)?.unwrap();
            assert_eq!(d.kind, "sleep");
            assert_eq!(d.due_at, at + 60);
            assert!(messages[0]["content"]
                .as_str()
                .unwrap()
                .contains("还有一分钟"));
            life::commit_decision(&tx, &d, &choice("postpone", 10), "synthetic", at + 1)?;
            assert!(life::commit_decision(&tx, &d, &choice("cancel", 0), "stale", at + 2).is_err());
            assert!(life::prepare_decision(&tx, at + 120)?.is_none());
            let (d2, _) = life::prepare_decision(&tx, at + 600)?.unwrap();
            life::commit_decision(&tx, &d2, &choice("postpone", 10), "synthetic", at + 601)?;
            let (d3, _) = life::prepare_decision(&tx, at + 1200)?.unwrap();
            assert!(life::commit_decision(
                &tx,
                &d3,
                &choice("postpone", 1),
                "synthetic",
                at + 1201
            )
            .is_err());
            life::commit_decision(&tx, &d3, &choice("cancel", 0), "synthetic", at + 1201)?;
            let clock = rest::ClockSample::new(at + 1260, 1260000, "synthetic-clock", true)?;
            life::routine_tick(&tx, &clock, at + 1260)?;
            assert_eq!(rest::state_in(&tx)?.phase, rest::RestPhase::Awake);
            let items = life::calendar(&tx, at, at + 7200)?;
            assert!(items
                .iter()
                .any(|v| v["type"] == "sleep" && v["status"] == "cancelled"));
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn routine_fallback_sleeps_cuts_context_and_wake_cancel_preserves_maximum() {
    let f = fixture().await;
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-27T22:59:00+08:00")
        .unwrap()
        .timestamp();
    configure(&f, at);
    f.state
        .db(|db| {
            let tx = db.transaction()?;
            life::routine_tick(&tx, &rest::ClockSample::new(at, 0, "boot", true)?, at)?;
            let (d, _) = life::prepare_decision(&tx, at)?.unwrap();
            life::fail_decision(&tx, &d, "synthetic_provider_failed", at + 2)?;
            let clock = rest::ClockSample::new(at + 60, 60000, "boot", true)?;
            life::routine_tick(&tx, &clock, at + 60)?;
            assert_eq!(rest::state_in(&tx)?.phase, rest::RestPhase::SleepPending);
            let wake = rest::state_in(&tx)?.wake_at.unwrap();
            let original = wake;
            rest::change_routine_wake_in(&tx, Some(wake + 600), wake - 60)?;
            assert_eq!(rest::state_in(&tx)?.wake_at, Some(original + 600));
            rest::change_routine_wake_in(&tx, None, wake - 60)?;
            assert_eq!(rest::state_in(&tx)?.wake_at, Some(at + 60 + 86400));
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn fixed_script_is_used_and_rejects_arbitrary_capabilities() {
    let result = life::run_script(r#"{"decision":"postpone","delay_minutes":5}"#)
        .await
        .unwrap();
    assert_eq!(result.delay_minutes, 5);
    for raw in [
        r#"{"decision":"execute","delay_minutes":1}"#,
        r#"{"decision":"postpone","delay_minutes":31}"#,
        r#"{"decision":"execute","path":"/etc/passwd"}"#,
        r#"{"decision":"execute","decision":"cancel"}"#,
    ] {
        assert!(life::run_script(raw).await.is_err());
    }
}

#[tokio::test]
async fn worker_publishes_only_after_schedule_commit_and_rejects_forged_slot_without_orphan() {
    let f = fixture().await;
    configure(&f, now());
    let current = start(&f.state, "a", "我先去忙，你自己安排抄首诗吧。");
    let run = current.input.id.clone();
    let messages = prepare_messages(&f.state, &current.input).unwrap();
    let final_contract = messages
        .iter()
        .rev()
        .find(|m| m["role"] == "system")
        .unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(
        final_contract.contains("DSH_DAILY_LIFE_COMPLETION_V2")
            && final_contract.contains("不能只说要做却不登记")
    );
    let slot = f
        .state
        .db(|db| {
            let raw: String = db.query_row(
                "SELECT offer_json FROM daily_life_offers WHERE run_id=?1",
                [&run],
                |r| r.get(0),
            )?;
            Ok(serde_json::from_str::<Value>(&raw)?["slots"][0]["slot_id"].clone())
        })
        .unwrap();
    assert!(slot.is_string());
    *f.model.response.lock().unwrap()=json!({"reply":[{"type":"text","text":"我去抄首《春晓》。","delay_ms":0}],"self_activity":{"poem_id":"chunxiao","slot_id":slot}}).to_string();
    process(&f.state, current, &CancellationToken::new()).await;
    f.state
        .db(|db| {
            assert_eq!(
                db.query_row("SELECT status FROM runs WHERE id=?1", [&run], |r| r
                    .get::<_, String>(0))?,
                "completed"
            );
            assert_eq!(
                schedule::list(db, "owner")?["items"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                db.query_row("SELECT response FROM runs WHERE id=?1", [&run], |r| r
                    .get::<_, String>(0))?,
                "我去抄首《春晓》。"
            );
            Ok(())
        })
        .unwrap();
    *f.model.response.lock().unwrap()=json!({"reply":[{"type":"text","text":"我去抄诗。","delay_ms":0}],"self_activity":{"poem_id":"chunxiao","slot_id":"forged"}}).to_string();
    let forged = start(&f.state, "a", "再安排一次。");
    let id = forged.input.id.clone();
    process(&f.state, forged, &CancellationToken::new()).await;
    f.state
        .db(|db| {
            assert_eq!(
                db.query_row("SELECT status FROM runs WHERE id=?1", [&id], |r| r
                    .get::<_, String>(0))?,
                "failed"
            );
            assert_eq!(
                schedule::list(db, "owner")?["items"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM messages WHERE run_id=?1 AND role='assistant'",
                    [&id],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn movable_activity_notice_postpones_one_occurrence_and_executes_at_new_time() {
    let f = fixture().await;
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-27T10:00:00+08:00")
        .unwrap()
        .timestamp();
    configure(&f, at);
    let poem_run = enqueue(
        &f.state,
        "a",
        "owner",
        "movable-source",
        "安排一个可移动活动",
        None,
    )
    .unwrap();
    f.state
        .db(|db| {
            let tx = db.transaction()?;
            life::prompt_in(&tx, &poem_run, at)?;
            let raw: String = tx.query_row(
                "SELECT offer_json FROM daily_life_offers WHERE run_id=?1",
                [&poem_run],
                |r| r.get(0),
            )?;
            let offer: Value = serde_json::from_str(&raw)?;
            let item = life::commit_selection(
                &tx,
                &poem_run,
                &life::Selection {
                    poem_id: "luchai".into(),
                    slot_id: offer["slots"][0]["slot_id"].as_str().unwrap().into(),
                    ..Default::default()
                },
                at,
            )?;
            let start = item["definition"]["start_at"].as_i64().unwrap();
            let clock = rest::ClockSample::new(start - 60, 0, "clock", true)?;
            life::routine_tick(&tx, &clock, start - 60)?;
            let (d, _) = life::prepare_decision(&tx, start - 60)?.unwrap();
            assert_eq!(d.kind, "writing");
            life::commit_decision(&tx, &d, &choice("postpone", 5), "synthetic", start - 59)?;
            schedule::tick_in(&tx, start, false)?;
            assert_eq!(
                schedule::detail(&tx, "owner", item["id"].as_str().unwrap())?["occurrences"],
                json!([])
            );
            schedule::tick_in(&tx, start + 300, false)?;
            let detail = schedule::detail(&tx, "owner", item["id"].as_str().unwrap())?;
            assert_eq!(detail["occurrences"][0]["status"], "completed");
            assert_eq!(detail["occurrences"][0]["start_at"], start + 300);
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}
