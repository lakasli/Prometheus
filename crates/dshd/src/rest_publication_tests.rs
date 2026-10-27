use super::*;
use crate::{
    auth::Admin,
    rest_store as rest,
    state::{now, AppState, Options},
    worker::ReplyTarget,
};

struct Fixture {
    _dir: tempfile::TempDir,
    state: AppState,
    admin: Admin,
}
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(Options {
        data_dir: dir.path().into(),
        origin: "http://localhost".into(),
        allow_private_model: false,
        cookie_secure: false,
        web_dir: dir.path().join("web"),
    })
    .unwrap();
    let admin = Admin {
        id: "rest-claim-admin".into(),
        username: "rest_claim_admin".into(),
        role: "admin".into(),
        csrf_token: "synthetic-only".into(),
    };
    state.db(|db| {
        db.execute("INSERT INTO admins(id,username,password_hash,created_at)VALUES(?1,?2,'unused',0)",params![admin.id,admin.username])?;
        for (id,owner) in [("qq-conversation","qq:synthetic:c2c:member"),("weixin-conversation","weixin:synthetic:c2c:peer")] {
            db.execute("INSERT INTO conversations(id,owner_id,title,source_key,created_at)VALUES(?1,?2,'synthetic sender claims',?2,?3)",params![id,owner,now()])?;
        }
        let tx=db.transaction()?;
        let requester=rest::requester_for_admin_in(&tx,&admin)?;
        let (revision,_)=rest::policy_in(&tx)?;
        rest::save_policy_in(&tx,&requester,revision,&rest::RestPolicy {natural_enabled:true,default_sleep_seconds:Some(600),..Default::default()},now())?;
        tx.commit()?;Ok(())
    }).unwrap();
    Fixture {
        _dir: dir,
        state,
        admin,
    }
}
fn clock(at: i64, delta: i64) -> rest::ClockSample {
    rest::ClockSample::new(at + delta, delta * 1000, "synthetic_claim_boot", true).unwrap()
}
fn request(f: &Fixture, conversation: &str, input: &str) -> String {
    let owner = f
        .state
        .db(|db| {
            Ok(db.query_row(
                "SELECT owner_id FROM conversations WHERE id=?1",
                [conversation],
                |r| r.get::<_, String>(0),
            )?)
        })
        .unwrap();
    crate::worker::enqueue(
        &f.state,
        conversation,
        &owner,
        &uuid::Uuid::new_v4().to_string(),
        input,
        None,
    )
    .unwrap()
}
fn ordinary(f: &Fixture, conversation: &str) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    f.state.db(|db| {
        db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,response,created_at,updated_at)SELECT ?1,id,owner_id,?1,'synthetic original question','{}','completed','ordinary synthetic reply',?2,?2 FROM conversations WHERE id=?3",params![id,now(),conversation])?;
        Ok(())
    }).unwrap();
    id
}
fn queued_qq(f: &Fixture, run: &str, content: &str) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    let target = ReplyTarget {
        kind: "c2c".into(),
        target: "synthetic-member".into(),
        message_id: "synthetic-input".into(),
        account_id: "synthetic-account".into(),
        credential_ref: "no-real-key".into(),
        deadline: now() + 600,
    };
    f.state.db(|db| {db.execute("UPDATE runs SET status='completed',response=?1 WHERE id=?2",params![content,run])?;db.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,deadline,created_at)VALUES(?1,?2,?3,?4,?5,?6,'queued',?7,?8)",params![id,run,target.account_id,target.credential_ref,serde_json::to_string(&target)?,content,target.deadline,now()])?;Ok(())}).unwrap();
    id
}
fn begin_admin(f: &Fixture) {
    f.state
        .db(|db| {
            let tx = db.transaction()?;
            let a = rest::requester_for_admin_in(&tx, &f.admin)?;
            rest::sleep_in(
                &tx,
                &a,
                0,
                "admin-sleep",
                &Default::default(),
                &clock(now(), 0),
            )?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}
fn sleep_ack(f: &Fixture, conversation: &str, text: &str) -> String {
    let run = request(f, conversation, "去睡觉吧");
    f.state
        .db(|db| {
            let tx = db.transaction()?;
            let at = now();
            let admin = rest::requester_for_admin_in(&tx, &f.admin)?;
            let source = rest::requester_for_run_in(&tx, &run)?;
            rest::set_source_grant_in(
                &tx,
                &admin,
                &source,
                0,
                &rest::GrantPermissions {
                    can_sleep: true,
                    can_wake: true,
                    valid_until: at + 1000,
                },
                at,
            )?;
            let op = rest::sleep_in(
                &tx,
                &source,
                0,
                "source-sleep",
                &Default::default(),
                &clock(at, 0),
            )?;
            rest::bind_ack_payload_in(
                &tx,
                &source,
                op.ack_permit_id.as_deref().unwrap(),
                &payload_digest(text),
                at,
            )?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    run
}
fn wake_admin(f: &Fixture) {
    f.state
        .db(|db| {
            let tx = db.transaction()?;
            let admin = rest::requester_for_admin_in(&tx, &f.admin)?;
            let generation = rest::state_in(&tx)?.generation;
            let clock = clock(now(), 1);
            rest::wake_in(&tx, &admin, generation, "admin-wake", &clock)?;
            rest::advance_in(&tx, &clock)?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}
fn scalar(f: &Fixture, sql: &str, id: &str) -> String {
    f.state
        .db(|db| Ok(db.query_row(sql, [id], |r| r.get(0))?))
        .unwrap()
}
fn qq_claim(f: &Fixture) -> Option<String> {
    f.state
        .db(|db| crate::reply_delivery::claim_id_for_rest_test(db, crate::reply_delivery::now_ms()))
        .unwrap()
}
fn used(f: &Fixture, run: &str) -> bool {
    f.state
        .db(|db| {
            Ok(db.query_row(
                "SELECT used_at IS NOT NULL FROM rest_ack_permits WHERE source_run_id=?1",
                [run],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}
fn queued_weixin(f: &Fixture, run: &str, content: &str) -> String {
    f.state.db(|db| {
        let tx=db.transaction()?;
        tx.execute("INSERT OR IGNORE INTO weixin_account(singleton,account_id,base_url,credential_ref,linked_user_id,revision,generation,enabled,connection_status,updated_at)VALUES(1,'synthetic-bot@im.bot','https://example.invalid','synthetic-only-key','synthetic-linked-user',1,'synthetic-generation',1,'connected',?1)",[now()])?;
        let context=uuid::Uuid::new_v4().to_string();
        tx.execute("INSERT INTO weixin_contexts(id,account_id,peer,generation,token,created_at,expires_at)VALUES(?1,'synthetic-bot@im.bot','synthetic-peer@im.wechat','synthetic-generation','synthetic-only-context',?2,?3)",params![context,now(),now()+600])?;
        tx.execute("INSERT INTO weixin_run_replies(run_id,account_id,peer,context_ref,credential_ref,generation,deadline)VALUES(?1,'synthetic-bot@im.bot','synthetic-peer@im.wechat',?2,'synthetic-only-key','synthetic-generation',?3)",params![run,context,now()+600])?;
        tx.execute("UPDATE runs SET status='completed',response=?1 WHERE id=?2",params![content,run])?;
        anyhow::ensure!(crate::weixin_store::queue_reply_in(&tx,run,content).is_ok_and(|value|value),"synthetic queue was rejected");
        let id=tx.query_row("SELECT id FROM weixin_outbox WHERE run_id=?1",[run],|r|r.get::<_,String>(0))?;
        tx.commit()?;Ok(id)
    }).unwrap()
}

#[test]
fn absent_rest_schema_is_legacy_only_but_partial_schema_fails_closed() {
    let mut db = Connection::open_in_memory().unwrap();
    let tx = db.transaction().unwrap();
    assert!(claim_allowed_in(&tx, "legacy", None, "text").unwrap());
    tx.execute_batch("CREATE TABLE rest_ack_permits(source_run_id TEXT)")
        .unwrap();
    assert!(claim_allowed_in(&tx, "legacy", None, "text").is_err());
}

#[test]
fn actual_ordered_claim_holds_ordinary_then_waking_releases_only_the_queued_claim() {
    let f = fixture();
    let run = ordinary(&f, "qq-conversation");
    let outbox = queued_qq(&f, &run, "ordinary synthetic reply");
    begin_admin(&f);
    assert!(qq_claim(&f).is_none());
    assert_eq!(
        scalar(&f, "SELECT status FROM outbox WHERE id=?1", &outbox),
        "queued"
    );
    wake_admin(&f);
    assert_eq!(qq_claim(&f).as_deref(), Some(outbox.as_str()));
    assert_eq!(
        scalar(&f, "SELECT status FROM outbox WHERE id=?1", &outbox),
        "sending"
    );
}

#[test]
fn exact_ack_is_prioritized_beyond_sixty_four_held_predecessors_without_breaking_pacing() {
    let f = fixture();
    for _ in 0..70 {
        let run = ordinary(&f, "qq-conversation");
        queued_qq(&f, &run, "ordinary synthetic reply");
    }
    let run = sleep_ack(&f, "qq-conversation", "固定入睡受理文字。");
    let ack = queued_qq(&f, &run, "固定入睡受理文字。");
    f.state.db(|db|{db.execute("INSERT INTO reply_account_pacing(account_id,next_ms)VALUES('synthetic-account',?1)",[crate::reply_delivery::now_ms()+60000])?;Ok(())}).unwrap();
    assert!(qq_claim(&f).is_none());
    assert!(!used(&f, &run));
    f.state
        .db(|db| {
            db.execute("DELETE FROM reply_account_pacing", [])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(qq_claim(&f).as_deref(), Some(ack.as_str()));
    assert!(used(&f, &run));
    let queued: i64 = f
        .state
        .db(|db| {
            Ok(db.query_row(
                "SELECT count(*) FROM outbox WHERE status='queued'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(queued, 70);
    assert!(qq_claim(&f).is_none());
}

#[test]
fn mismatched_or_part_based_ack_is_cancelled_before_any_permit_is_consumed() {
    for parts in [false, true] {
        let f = fixture();
        let run = sleep_ack(&f, "qq-conversation", "固定入睡受理文字。");
        let ack = queued_qq(
            &f,
            &run,
            if parts {
                "固定入睡受理文字。"
            } else {
                "模型改写的不同文字。"
            },
        );
        if parts {
            f.state.db(|db|{db.execute("INSERT INTO outbox_reply_parts(outbox_id,seq,payload_json,delay_ms,status)VALUES(?1,1,?2,1000,'queued')",params![ack,json!({"type":"text","text":"固定入睡受理文字。"}).to_string()])?;Ok(())}).unwrap();
        }
        assert!(qq_claim(&f).is_none());
        assert!(!used(&f, &run));
        assert_eq!(
            scalar(&f, "SELECT status FROM outbox WHERE id=?1", &ack),
            "cancelled"
        );
    }
}

#[test]
fn stale_sleep_ack_does_not_turn_into_ordinary_text_after_waking() {
    let f = fixture();
    let run = sleep_ack(&f, "qq-conversation", "固定入睡受理文字。");
    let ack = queued_qq(&f, &run, "固定入睡受理文字。");
    wake_admin(&f);
    assert!(qq_claim(&f).is_none());
    assert!(!used(&f, &run));
    assert_eq!(
        scalar(&f, "SELECT status FROM outbox WHERE id=?1", &ack),
        "cancelled"
    );
}

#[test]
fn claimed_or_unknown_predecessors_remain_absolute_fences_for_an_ack() {
    for predecessor in ["sending", "unknown"] {
        let f = fixture();
        let old = ordinary(&f, "qq-conversation");
        let older = queued_qq(&f, &old, "ordinary synthetic reply");
        f.state
            .db(|db| {
                db.execute(
                    "UPDATE outbox SET status=?1 WHERE id=?2",
                    params![predecessor, older],
                )?;
                Ok(())
            })
            .unwrap();
        let run = sleep_ack(&f, "qq-conversation", "固定入睡受理文字。");
        let ack = queued_qq(&f, &run, "固定入睡受理文字。");
        assert!(qq_claim(&f).is_none());
        assert!(!used(&f, &run));
        assert_eq!(
            scalar(&f, "SELECT status FROM outbox WHERE id=?1", &older),
            predecessor
        );
        assert_eq!(
            scalar(&f, "SELECT status FROM outbox WHERE id=?1", &ack),
            "queued"
        );
    }
}

#[test]
fn rejected_publication_cancels_only_queued_parts_and_preserves_receipts_unknown_and_native_lease()
{
    let f = fixture();
    let run = ordinary(&f, "qq-conversation");
    let outbox = queued_qq(&f, &run, "ordinary synthetic reply");
    f.state.db(|db| {
        let tx=db.transaction()?;
        for (seq,status) in [(1,"sent"),(2,"unknown"),(3,"sending"),(4,"queued")] {
            tx.execute("INSERT INTO outbox_reply_parts(outbox_id,seq,payload_json,delay_ms,status,platform_message_id,finished_ms)VALUES(?1,?2,'{}',1000,?3,?4,?5)",params![outbox,seq,status,(status=="sent").then_some("synthetic-receipt"),(status=="sent").then_some(123)])?;
        }
        tx.execute("UPDATE outbox SET status='unknown' WHERE id=?1",[&outbox])?;
        tx.execute("INSERT INTO native_worker_leases(run_id,claimed_at)VALUES(?1,?2)",params![run,now()])?;
        assert!(!claim_allowed_in(&tx,&run,Some(now()-1),"ordinary synthetic reply")?);
        tx.commit()?;Ok(())
    }).unwrap();
    assert_eq!(
        scalar(&f, "SELECT status FROM outbox WHERE id=?1", &outbox),
        "unknown"
    );
    let rows = f
        .state
        .db(|db| {
            let mut s = db
                .prepare("SELECT status FROM outbox_reply_parts WHERE outbox_id=?1 ORDER BY seq")?;
            let rows = s
                .query_map([&outbox], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap();
    assert_eq!(rows, ["sent", "unknown", "sending", "cancelled"]);
    assert_eq!(
        scalar(
            &f,
            "SELECT platform_message_id FROM outbox_reply_parts WHERE outbox_id=?1 AND seq=1",
            &outbox
        ),
        "synthetic-receipt"
    );
    assert_eq!(
        f.state
            .db(|db| Ok(db.query_row(
                "SELECT count(*) FROM native_worker_leases WHERE run_id=?1",
                [&run],
                |r| r.get::<_, i64>(0)
            )?))
            .unwrap(),
        1
    );
}

#[test]
fn legacy_actual_claim_uses_the_same_ack_fence_and_does_not_starve_behind_hold() {
    let f = fixture();
    for _ in 0..70 {
        let run = ordinary(&f, "qq-conversation");
        queued_qq(&f, &run, "ordinary synthetic reply");
    }
    let run = sleep_ack(&f, "qq-conversation", "固定入睡受理文字。");
    let ack = queued_qq(&f, &run, "固定入睡受理文字。");
    let claimed = f
        .state
        .db(crate::qq::claim_legacy_outbox_in)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.0, ack);
    assert!(used(&f, &run));
}

#[test]
fn partial_or_newer_rest_schema_never_sends_even_when_other_claim_proofs_are_valid() {
    for partial in [true, false] {
        let f = fixture();
        let run = ordinary(&f, "qq-conversation");
        let id = queued_qq(&f, &run, "ordinary synthetic reply");
        f.state
            .db(|db| {
                if partial {
                    db.execute("DROP TABLE rest_ack_permits", [])?;
                } else {
                    db.execute("UPDATE rest_meta SET schema_version=2 WHERE id=1", [])?;
                }
                Ok(())
            })
            .unwrap();
        assert!(f
            .state
            .db(|db| crate::reply_delivery::claim_id_for_rest_test(
                db,
                crate::reply_delivery::now_ms()
            ))
            .is_err());
        assert_eq!(
            scalar(&f, "SELECT status FROM outbox WHERE id=?1", &id),
            "queued"
        );
    }
}

#[test]
fn actual_weixin_claim_prioritizes_exact_ack_and_keeps_other_rows_queued_without_releasing_attempt()
{
    let f = fixture();
    let old = ordinary(&f, "weixin-conversation");
    let pending = queued_weixin(&f, &old, "ordinary synthetic reply");
    let run = sleep_ack(&f, "weixin-conversation", "固定入睡受理文字。");
    let ack = queued_weixin(&f, &run, "固定入睡受理文字。");
    let item = f
        .state
        .db(|db| Ok(crate::weixin_store::claim_send(db)?))
        .unwrap()
        .unwrap();
    assert_eq!(item.id, ack);
    assert!(used(&f, &run));
    assert_eq!(
        scalar(&f, "SELECT status FROM weixin_outbox WHERE id=?1", &pending),
        "queued"
    );
    assert!(f
        .state
        .db(|db| Ok(crate::weixin_store::claim_send(db)?))
        .unwrap()
        .is_none());
    assert_eq!(
        scalar(&f, "SELECT status FROM weixin_outbox WHERE id=?1", &ack),
        "sending"
    );
}

#[test]
fn actual_weixin_claim_does_not_allow_an_ack_to_bypass_unknown_or_expired_context() {
    for unknown in [true, false] {
        let f = fixture();
        let old = ordinary(&f, "weixin-conversation");
        let pending = queued_weixin(&f, &old, "ordinary synthetic reply");
        let run = sleep_ack(&f, "weixin-conversation", "固定入睡受理文字。");
        let ack = queued_weixin(&f, &run, "固定入睡受理文字。");
        f.state.db(|db| {
            if unknown {db.execute("UPDATE weixin_outbox SET status='unknown',attempt_id='synthetic-old-attempt',error_code='synthetic_unknown' WHERE id=?1",[&pending])?;}
            else {db.execute("UPDATE weixin_contexts SET expires_at=?1 WHERE id=(SELECT context_ref FROM weixin_outbox WHERE id=?2)",params![now()-1,ack])?;}
            Ok(())
        }).unwrap();
        assert!(f
            .state
            .db(|db| Ok(crate::weixin_store::claim_send(db)?))
            .unwrap()
            .is_none());
        assert!(!used(&f, &run));
        assert_eq!(
            scalar(&f, "SELECT status FROM weixin_outbox WHERE id=?1", &ack),
            if unknown { "queued" } else { "failed" }
        );
        if unknown {
            assert_eq!(
                scalar(
                    &f,
                    "SELECT attempt_id FROM weixin_outbox WHERE id=?1",
                    &pending
                ),
                "synthetic-old-attempt"
            );
        }
    }
}

#[test]
fn awake_ack_uses_the_actual_awake_phase_and_bypasses_queued_ordinary_predecessors() {
    let f = fixture();
    for _ in 0..70 {
        let old = ordinary(&f, "qq-conversation");
        queued_qq(&f, &old, "ordinary synthetic reply");
    }
    begin_admin(&f);
    let run = request(&f, "qq-conversation", "醒醒");
    f.state
        .db(|db| {
            let tx = db.transaction()?;
            let at = now();
            let admin = rest::requester_for_admin_in(&tx, &f.admin)?;
            let source = rest::requester_for_run_in(&tx, &run)?;
            rest::set_source_grant_in(
                &tx,
                &admin,
                &source,
                0,
                &rest::GrantPermissions {
                    can_sleep: true,
                    can_wake: true,
                    valid_until: at + 1000,
                },
                at,
            )?;
            let sample = clock(at, 1);
            let operation = rest::wake_in(&tx, &source, 1, "source-wake", &sample)?;
            rest::advance_in(&tx, &sample)?;
            rest::bind_ack_payload_in(
                &tx,
                &source,
                operation.ack_permit_id.as_deref().unwrap(),
                &payload_digest("固定唤醒受理文字。"),
                at,
            )?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    let ack = queued_qq(&f, &run, "固定唤醒受理文字。");
    assert_eq!(qq_claim(&f).as_deref(), Some(ack.as_str()));
    assert!(used(&f, &run));
}

#[test]
fn acknowledgement_consumption_rolls_back_with_the_claim_transaction() {
    let f = fixture();
    let run = sleep_ack(&f, "qq-conversation", "固定入睡受理文字。");
    let ack = queued_qq(&f, &run, "固定入睡受理文字。");
    f.state
        .db(|db| {
            let tx = db.transaction()?;
            anyhow::ensure!(
                claim_allowed_in(&tx, &run, Some(now() + 600), "固定入睡受理文字。")?,
                "synthetic exact ack not admitted"
            );
            // Simulate a failure before the existing sender can write sending and
            // commit. The permit must not survive this rollback independently.
            drop(tx);
            Ok(())
        })
        .unwrap();
    assert!(!used(&f, &run));
    assert_eq!(qq_claim(&f).as_deref(), Some(ack.as_str()));
    assert!(used(&f, &run));
}

#[test]
fn independent_connections_cannot_claim_or_consume_the_same_ack_twice() {
    let f = fixture();
    let run = sleep_ack(&f, "qq-conversation", "固定入睡受理文字。");
    let ack = queued_qq(&f, &run, "固定入睡受理文字。");
    let path = f._dir.path().join("dsh.db");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut db = Connection::open(path).unwrap();
                db.busy_timeout(std::time::Duration::from_secs(3)).unwrap();
                barrier.wait();
                crate::reply_delivery::claim_id_for_rest_test(
                    &mut db,
                    crate::reply_delivery::now_ms(),
                )
                .ok()
                .flatten()
            })
        })
        .collect();
    let claimed: Vec<_> = threads
        .into_iter()
        .filter_map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(claimed, vec![ack]);
    assert!(used(&f, &run));
}
