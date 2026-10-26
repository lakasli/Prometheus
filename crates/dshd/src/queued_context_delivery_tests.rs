//! Actual claim transactions, with no sender task or external network. Runs
//! enter through worker::enqueue; only completion/receipts are synthetic.
use super::*;
use crate::{conversation_commitments as commitments, conversation_focus, conversation_state};
use serde_json::json;

struct Fixture {
    _temporary: tempfile::TempDir,
    state: AppState,
    admin: crate::auth::Admin,
}

fn fixture() -> Fixture {
    let temporary = tempfile::tempdir().unwrap();
    let state = AppState::open(crate::state::Options {
        data_dir: temporary.path().into(),
        origin: "http://localhost".into(),
        allow_private_model: false,
        cookie_secure: false,
        web_dir: temporary.path().join("web"),
    })
    .unwrap();
    state.db(|db| {
        db.execute("INSERT INTO admins(id,username,password_hash,created_at)VALUES('manager','delivery-manager','unused',0)",[])?;
        for (id,owner) in [("a","qq:synthetic:c2c:member-a"),("b","qq:synthetic:c2c:member-b")] {
            db.execute("INSERT INTO conversations(id,owner_id,title,source_key,created_at)VALUES(?1,?2,'synthetic queued context',?2,?3)",params![id,owner,now()])?;
        }
        Ok(())
    }).unwrap();
    Fixture {
        _temporary: temporary,
        state,
        admin: crate::auth::Admin {
            id: "manager".into(),
            username: "delivery-manager".into(),
            role: "admin".into(),
            csrf_token: "synthetic-only".into(),
        },
    }
}

fn admitted(
    f: &Fixture,
    conversation: &str,
    proof: bool,
    proposal: bool,
) -> (String, Option<String>) {
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
    let run = crate::worker::enqueue(
        &f.state,
        conversation,
        &owner,
        &uuid::Uuid::new_v4().to_string(),
        "讨论合成阅读事项",
        None,
    )
    .unwrap();
    let candidate=f.state.db(|db| {
        let tx=db.transaction()?;
        tx.execute("UPDATE runs SET status='running' WHERE id=?1",[&run])?;
        let mut candidate=None;
        if proof {
            crate::conversation_identity::context_in(&tx,&run,conversation,&owner)?;
            conversation_focus::observe_input_in(&tx,&run,now(),&Default::default())?;
            let scope=conversation_state::scope_for_run_in(&tx,&run)?;
            if proposal {
                let value=commitments::propose_in(&tx,&scope,&commitments::Proposal {
                    intent:"阅读合成资料".into(),kind:"reading".into(),time_text:None,expires_at:None,sources:Vec::new(),
                },commitments::ProposalOrigin::ModelSuggestion,&format!("synthetic-proposal:{run}"),now())?;
                candidate=Some(value["id"].as_str().unwrap().to_string());
            }
            let focus=conversation_focus::context_for_run_in(&tx,&run,now())?;
            conversation_state::record_usage_in(&tx,&run,&focus.entity_refs,now())?;
            let context=commitments::context_for_run_in(&tx,&run,6,8000,now())?;
            commitments::record_usage_in(&tx,&run,&context.source_refs,now())?;
            let text=json!([{"role":"user","content":"讨论合成阅读事项"}]).to_string();
            tx.execute("INSERT INTO run_prompt_snapshots(run_id,messages_json,digest,compiler_version,created_at)VALUES(?1,?2,?3,'synthetic-claim-fixture-no-provider',?4)",params![run,text,crate::character::digest(text.as_bytes()),now()])?;
        }
        tx.execute("UPDATE runs SET status='completed',response='合成旧版本结果。' WHERE id=?1",[&run])?;
        tx.commit()?;
        Ok(candidate)
    }).unwrap();
    (run, candidate)
}

fn qq_queue(f: &Fixture, run: &str, states: &[&str], parent: &str) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    let owner = text(f, "SELECT owner_id FROM runs WHERE id=?1", run);
    let target = ReplyTarget {
        kind: "c2c".into(),
        target: owner.rsplit(':').next().unwrap().into(),
        message_id: "synthetic-input".into(),
        account_id: "synthetic".into(),
        credential_ref: "no-real-key".into(),
        deadline: now() + 600,
    };
    f.state.db(|db| {
        db.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,deadline,created_at)VALUES(?1,?2,'synthetic','no-real-key',?3,'合成旧版本结果。',?4,?5,?6)",params![id,run,serde_json::to_string(&target)?,parent,target.deadline,now()])?;
        for (index,status) in states.iter().enumerate() {
            db.execute("INSERT INTO outbox_reply_parts(outbox_id,seq,payload_json,delay_ms,status,platform_message_id,error,finished_ms)VALUES(?1,?2,?3,1000,?4,?5,?6,?7)",params![id,(index+1) as i64,json!({"type":"text","text":"合成句段。"}).to_string(),status,(*status=="sent").then_some("synthetic-receipt"),(*status=="unknown").then_some("synthetic-unknown"),(*status=="sent").then_some(123)])?;
        }
        Ok(())
    }).unwrap();
    id
}

fn change(f: &Fixture, run: &str, candidate: &str, participants: bool) {
    f.state
        .db(|db| {
            let tx = db.transaction()?;
            let scope = conversation_state::management_scope_for_source_in(&tx, &f.admin, run)?;
            let key = format!("synthetic-change-after-queue:{candidate}");
            let transition = commitments::Transition {
                expected_revision: 1,
                idempotency_key: &key,
                now: now(),
            };
            if participants {
                commitments::review_participants_in(
                    &tx,
                    &scope,
                    &f.admin,
                    candidate,
                    &commitments::ParticipantReview {
                        subject_kind: commitments::SubjectKind::Assistant,
                        counterparty_kind: commitments::CounterpartyKind::Speaker,
                    },
                    &transition,
                )?;
            } else {
                commitments::confirm_in(&tx, &scope, &f.admin, candidate, &transition)?;
            }
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}

fn text(f: &Fixture, sql: &str, id: &str) -> String {
    f.state
        .db(|db| Ok(db.query_row(sql, [id], |r| r.get(0))?))
        .unwrap()
}
fn count(f: &Fixture, sql: &str, id: &str) -> i64 {
    f.state
        .db(|db| Ok(db.query_row(sql, [id], |r| r.get(0))?))
        .unwrap()
}
fn claim(f: &Fixture) -> Option<Delivery> {
    f.state.db(|db| claim_in(db, now_ms())).unwrap()
}

#[test]
fn confirmation_after_queue_cancels_only_remaining_parts_and_claims_unrelated_run() {
    let f = fixture();
    let (old, candidate) = admitted(&f, "a", true, true);
    let outbox = qq_queue(&f, &old, &["sent", "queued"], "queued");
    f.state
        .db(|db| {
            db.execute(
                "INSERT INTO native_worker_leases(run_id,claimed_at)VALUES(?1,?2)",
                params![old, now()],
            )?;
            Ok(())
        })
        .unwrap();
    let prompt = text(
        &f,
        "SELECT messages_json FROM run_prompt_snapshots WHERE run_id=?1",
        &old,
    );
    let (other, _) = admitted(&f, "b", true, false);
    let other_outbox = qq_queue(&f, &other, &["queued"], "queued");
    change(&f, &old, candidate.as_deref().unwrap(), false);
    assert_eq!(claim(&f).unwrap().id, other_outbox);
    assert_eq!(
        text(&f, "SELECT status FROM outbox WHERE id=?1", &outbox),
        "cancelled"
    );
    assert_eq!(
        text(
            &f,
            "SELECT status FROM outbox_reply_parts WHERE outbox_id=?1 AND seq=1",
            &outbox
        ),
        "sent"
    );
    assert_eq!(
        text(
            &f,
            "SELECT platform_message_id FROM outbox_reply_parts WHERE outbox_id=?1 AND seq=1",
            &outbox
        ),
        "synthetic-receipt"
    );
    assert_eq!(
        text(
            &f,
            "SELECT status FROM outbox_reply_parts WHERE outbox_id=?1 AND seq=2",
            &outbox
        ),
        "cancelled"
    );
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM native_worker_leases WHERE run_id=?1",
            &old
        ),
        1
    );
    assert_eq!(
        text(
            &f,
            "SELECT messages_json FROM run_prompt_snapshots WHERE run_id=?1",
            &old
        ),
        prompt
    );
    assert_eq!(
        text(
            &f,
            "SELECT decision FROM run_publication_decisions WHERE run_id=?1",
            &old
        ),
        "active"
    );
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM memory_context_watermarks WHERE conversation_id=?1",
            "a"
        ),
        0
    );
}

#[test]
fn participant_review_after_queue_is_rechecked_without_a_source_forget_operation() {
    let f = fixture();
    let (run, candidate) = admitted(&f, "a", true, true);
    let outbox = qq_queue(&f, &run, &["queued"], "queued");
    change(&f, &run, candidate.as_deref().unwrap(), true);
    assert!(claim(&f).is_none());
    assert_eq!(
        text(&f, "SELECT error FROM outbox WHERE id=?1", &outbox),
        "conversation_context_stale"
    );
}

#[test]
fn expired_used_focus_fences_an_already_queued_result_at_claim() {
    let f = fixture();
    let (run, _) = admitted(&f, "a", true, false);
    let outbox = qq_queue(&f, &run, &["queued"], "queued");
    f.state
        .db(|db| {
            db.execute(
                "UPDATE conversation_state_entities SET expires_at=?1 WHERE id=?2",
                params![now() - 1, format!("focus:{run}")],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(claim(&f).is_none());
    assert_eq!(
        text(&f, "SELECT status FROM outbox WHERE id=?1", &outbox),
        "cancelled"
    );
}

#[test]
fn stale_remaining_parts_are_fenced_while_sending_unknown_sent_and_lease_facts_survive() {
    for parent in ["sending", "unknown"] {
        let f = fixture();
        let (run, candidate) = admitted(&f, "a", true, true);
        let outbox = qq_queue(&f, &run, &["sent", "sending", "unknown", "queued"], parent);
        f.state
            .db(|db| {
                db.execute(
                    "INSERT INTO native_worker_leases VALUES(?1,?2)",
                    params![run, now()],
                )?;
                Ok(())
            })
            .unwrap();
        change(&f, &run, candidate.as_deref().unwrap(), false);
        assert!(claim(&f).is_none());
        assert_eq!(
            text(&f, "SELECT status FROM outbox WHERE id=?1", &outbox),
            parent
        );
        for (seq, status) in [
            (1, "sent"),
            (2, "sending"),
            (3, "unknown"),
            (4, "cancelled"),
        ] {
            assert_eq!(
                text(
                    &f,
                    &format!(
                        "SELECT status FROM outbox_reply_parts WHERE outbox_id=?1 AND seq={seq}"
                    ),
                    &outbox
                ),
                status
            );
        }
        assert_eq!(
            text(
                &f,
                "SELECT error FROM outbox_reply_parts WHERE outbox_id=?1 AND seq=3",
                &outbox
            ),
            "synthetic-unknown"
        );
        assert_eq!(
            count(
                &f,
                "SELECT count(*) FROM native_worker_leases WHERE run_id=?1",
                &run
            ),
            1
        );
    }
}

#[test]
fn legacy_qq_claim_also_fences_stale_new_proof_before_claiming_later_work() {
    let f = fixture();
    let (run, candidate) = admitted(&f, "a", true, true);
    let old = qq_queue(&f, &run, &[], "queued");
    change(&f, &run, candidate.as_deref().unwrap(), false);
    let (other, _) = admitted(&f, "b", false, false);
    let newer = qq_queue(&f, &other, &[], "queued");
    let claimed = f
        .state
        .db(crate::qq::claim_legacy_outbox_in)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.0, newer);
    assert_eq!(
        text(&f, "SELECT status FROM outbox WHERE id=?1", &old),
        "cancelled"
    );
}

#[test]
fn no_new_snapshot_legacy_run_is_not_reinterpreted_or_retrofitted() {
    let f = fixture();
    let (run, _) = admitted(&f, "a", false, false);
    f.state
        .db(|db| {
            db.execute(
                "DELETE FROM run_conversation_bindings WHERE run_id=?1",
                [&run],
            )?;
            Ok(())
        })
        .unwrap();
    let outbox = qq_queue(&f, &run, &["queued"], "queued");
    assert_eq!(claim(&f).unwrap().id, outbox);
    for table in [
        "run_conversation_bindings",
        "run_prompt_snapshots",
        "conversation_state_snapshots",
        "conversation_commitment_contexts",
    ] {
        assert_eq!(
            count(
                &f,
                &format!("SELECT count(*) FROM {table} WHERE run_id=?1"),
                &run
            ),
            0
        );
    }
}

#[test]
fn partial_feature_schema_fails_closed_and_does_not_leave_stale_queued_records_blocking() {
    let f = fixture();
    let (run, candidate) = admitted(&f, "a", true, true);
    let outbox = qq_queue(&f, &run, &["queued"], "queued");
    assert!(candidate.is_some());
    f.state
        .db(|db| {
            db.execute_batch("DROP TABLE conversation_commitment_sources;")?;
            Ok(())
        })
        .unwrap();
    assert!(claim(&f).is_none());
    assert_eq!(
        text(&f, "SELECT status FROM outbox WHERE id=?1", &outbox),
        "cancelled"
    );
    assert_eq!(
        text(&f, "SELECT error FROM outbox WHERE id=?1", &outbox),
        "conversation_context_unavailable"
    );
    assert_eq!(
        text(
            &f,
            "SELECT decision FROM run_publication_decisions WHERE run_id=?1",
            &run
        ),
        "active"
    );
}

#[test]
fn a_frozen_new_result_with_missing_original_proof_is_not_repaired_during_claim() {
    for table in [
        "run_conversation_bindings",
        "run_conversation_contexts",
        "run_prompt_snapshots",
    ] {
        let f = fixture();
        let (run, _) = admitted(&f, "a", true, false);
        f.state
            .db(|db| {
                db.execute(&format!("DELETE FROM {table} WHERE run_id=?1"), [&run])?;
                Ok(())
            })
            .unwrap();
        let outbox = qq_queue(&f, &run, &["queued"], "queued");
        assert!(claim(&f).is_none());
        assert_eq!(
            text(&f, "SELECT status FROM outbox WHERE id=?1", &outbox),
            "cancelled"
        );
        assert_eq!(
            count(
                &f,
                &format!("SELECT count(*) FROM {table} WHERE run_id=?1"),
                &run
            ),
            0
        );
    }
}

#[test]
fn entirely_absent_feature_schema_is_an_ordinary_legacy_claim() {
    let f = fixture();
    let (run, _) = admitted(&f, "a", false, false);
    f.state.db(|db| {
        db.execute_batch("PRAGMA foreign_keys=OFF;")?;
        let mut query=db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND (name LIKE 'conversation_state_%' OR name LIKE 'conversation_commitment%')")?;
        let tables=query.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        drop(query);
        for table in tables {db.execute_batch(&format!("DROP TABLE {table};"))?;}
        db.execute_batch("PRAGMA foreign_keys=ON;")?;Ok(())
    }).unwrap();
    let outbox = qq_queue(&f, &run, &[], "queued");
    assert_eq!(claim(&f).unwrap().id, outbox);
}

fn weixin_setup(f: &Fixture) {
    let credential = uuid::Uuid::new_v4().to_string();
    f.state.db(|db| {
        db.execute("INSERT INTO secrets(id,kind,created_at)VALUES(?1,'weixin_bot_token',0)",[&credential])?;
        let account=crate::weixin_store::replace_account(db,0,&f.admin.id,&crate::weixin_store::AccountInput {
            account_id:"synthetic-bot@im.bot".into(),base_url:"https://ilinkai.weixin.qq.com/".into(),
            credential_ref:credential.clone(),linked_user_id:"synthetic-user@im.wechat".into(),enabled:true,
        })?;
        let owner=format!("weixin:{}:c2c:synthetic-peer",account.account_id);
        db.execute("INSERT INTO conversations(id,owner_id,title,source_key,created_at)VALUES('wx',?1,'synthetic wx',?1,?2)",params![owner,now()])?;
        Ok(())
    }).unwrap();
}

fn weixin_queue(f: &Fixture, run: &str) -> String {
    f.state.db(|db| {
        let tx=db.transaction()?;
        let account=crate::weixin_store::snapshot(&tx)?.unwrap();
        let context=uuid::Uuid::new_v4().to_string();
        tx.execute("INSERT INTO weixin_contexts(id,account_id,peer,generation,token,created_at,expires_at)VALUES(?1,?2,'synthetic-peer',?3,'synthetic-private-context',?4,?5)",params![context,account.account_id,account.generation,now(),now()+600])?;
        tx.execute("INSERT INTO weixin_run_replies(run_id,account_id,peer,context_ref,credential_ref,generation,deadline)VALUES(?1,?2,'synthetic-peer',?3,?4,?5,?6)",params![run,account.account_id,context,account.credential_ref,account.generation,now()+300])?;
        assert!(crate::weixin_store::queue_reply_in(&tx,run,"合成旧版本结果。")?);
        let id=tx.query_row("SELECT id FROM weixin_outbox WHERE run_id=?1",[run],|r|r.get(0))?;
        tx.commit()?;Ok(id)
    }).unwrap()
}

#[test]
fn weixin_claim_rechecks_confirmation_and_keeps_unknown_or_sending_outcomes() {
    let f = fixture();
    weixin_setup(&f);
    let (run, candidate) = admitted(&f, "wx", true, true);
    let old = weixin_queue(&f, &run);
    change(&f, &run, candidate.as_deref().unwrap(), false);
    assert!(f
        .state
        .db(|db| Ok(crate::weixin_store::claim_send(db)?))
        .unwrap()
        .is_none());
    assert_eq!(
        text(&f, "SELECT status FROM weixin_outbox WHERE id=?1", &old),
        "cancelled"
    );
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM weixin_outbox WHERE id=?1 AND content IS NULL",
            &old
        ),
        1
    );
    for status in ["unknown", "sending", "accepted"] {
        let (run, candidate) = admitted(&f, "wx", true, true);
        let outbox = weixin_queue(&f, &run);
        f.state.db(|db| {db.execute("UPDATE weixin_outbox SET status=?1,attempt_id='existing-attempt',server_message_id='existing-receipt' WHERE id=?2",params![status,outbox])?;Ok(())}).unwrap();
        change(&f, &run, candidate.as_deref().unwrap(), false);
        assert!(f
            .state
            .db(|db| Ok(crate::weixin_store::claim_send(db)?))
            .unwrap()
            .is_none());
        assert_eq!(
            text(&f, "SELECT status FROM weixin_outbox WHERE id=?1", &outbox),
            status
        );
        assert_eq!(
            text(
                &f,
                "SELECT server_message_id FROM weixin_outbox WHERE id=?1",
                &outbox
            ),
            "existing-receipt"
        );
    }
}

#[test]
fn weixin_stale_predecessor_is_cancelled_before_the_same_claim_selects_legacy_successor() {
    let f = fixture();
    weixin_setup(&f);
    let (run, candidate) = admitted(&f, "wx", true, true);
    let old = weixin_queue(&f, &run);
    change(&f, &run, candidate.as_deref().unwrap(), true);
    let (next, _) = admitted(&f, "wx", false, false);
    let next_box = weixin_queue(&f, &next);
    let claim = f
        .state
        .db(|db| Ok(crate::weixin_store::claim_send(db)?))
        .unwrap()
        .unwrap();
    assert_eq!(claim.id, next_box);
    assert_eq!(
        text(&f, "SELECT status FROM weixin_outbox WHERE id=?1", &old),
        "cancelled"
    );
}
