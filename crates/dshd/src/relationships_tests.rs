use super::*;

fn database() -> Connection {
    let mut db = Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../schema.sql")).unwrap();
    memory::initialize(&mut db).unwrap();
    crate::conversation_summary::initialize(&db).unwrap();
    db.execute_batch("CREATE TABLE IF NOT EXISTS run_prompt_snapshots(run_id TEXT PRIMARY KEY,messages_json TEXT NOT NULL);
 INSERT INTO conversations(id,owner_id,title,created_at) VALUES('alice','owner-a','same-name',1),('bob','owner-b','same-name',1),('alice2','owner-a','another-chat',1);").unwrap();
    initialize(&db).unwrap();
    db
}
fn create_person(db: &mut Connection, name: &str, conversation: &str) -> Person {
    create(
        db,
        Access::Admin("admin"),
        &CreatePerson {
            display_name: name.into(),
            scope: ScopeInput {
                visibility: "private".into(),
                source_conversation_id: Some(conversation.into()),
                publish: false,
            },
            reason: "经用户确认建立档案".into(),
        },
    )
    .unwrap()
}
fn manual() -> Evidence {
    Evidence {
        source_type: "manual".into(),
        source_id: None,
        source_revision: None,
        statement_kind: "self_report".into(),
        reason: "本人明确陈述，待人工审核".into(),
    }
}
fn propose(db: &mut Connection, id: &str, field: &str, value: Value) -> Assertion {
    let rev = person(db, Access::Admin("admin"), id).unwrap().revision;
    propose_assertion(
        db,
        Access::Admin("admin"),
        id,
        rev,
        &CreateAssertion {
            field: field.into(),
            value,
            evidence: manual(),
            valid_from: None,
            valid_until: None,
        },
    )
    .unwrap()
}
fn approve(db: &mut Connection, id: &str, supersedes: Vec<String>) -> ApiResult<Assertion> {
    review_assertion(
        db,
        Access::Admin("admin"),
        id,
        1,
        &ReviewAssertion {
            decision: "approve".into(),
            supersedes,
            reason: "已核对陈述与来源".into(),
        },
    )
}

#[test]
fn same_names_do_not_merge_and_private_nodes_do_not_leak() {
    let mut db = database();
    let a = create_person(&mut db, "张同学", "alice");
    let b = create_person(&mut db, "张同学", "bob");
    assert_ne!(a.id, b.id);
    let list_a = list(&db, Access::Owner("owner-a"), &ListOptions::default()).unwrap();
    assert_eq!(list_a.items.len(), 1);
    assert_eq!(list_a.items[0].id, a.id);
    assert!(person(&db, Access::Owner("owner-a"), &b.id).is_err());
    let request = MergePeople {
        target_person_id: b.id,
        target_revision: 1,
        confirmed: true,
        reason: "同名尝试".into(),
    };
    assert_eq!(
        merge(&mut db, Access::Admin("admin"), &a.id, 1, &request)
            .unwrap_err()
            .code,
        "merge_scope_mismatch"
    );
}

#[test]
fn assertions_require_review_and_conflicts_are_explicit() {
    let mut db = database();
    let p = create_person(&mut db, "某人", "alice");
    let a = propose(&mut db, &p.id, "job", json!("教师"));
    assert!(detail(&mut db, Access::Owner("owner-a"), &p.id, now())
        .unwrap()
        .assertions
        .is_empty());
    approve(&mut db, &a.id, vec![]).unwrap();
    let b = propose(&mut db, &p.id, "job", json!("工程师"));
    assert_eq!(
        approve(&mut db, &b.id, vec![]).unwrap_err().code,
        "profile_conflict"
    );
    approve(&mut db, &b.id, vec![a.id.clone()]).unwrap();
    let owner = detail(&mut db, Access::Owner("owner-a"), &p.id, now()).unwrap();
    assert_eq!(owner.assertions.len(), 1);
    assert_eq!(owner.assertions[0].value, json!("工程师"));
    assert_eq!(
        assertion(&db, Access::Admin("admin"), &a.id)
            .unwrap()
            .status,
        "superseded"
    );
    assert!(assertion(&db, Access::Owner("owner-b"), &b.id).is_err());
}

#[test]
fn age_and_birthday_keep_precision_and_do_not_infer_dates() {
    assert!(validate_value("age", &json!({"years":21,"stated_at":"2026-10-26"})).is_ok());
    assert!(validate_value("age", &json!(21)).is_err());
    assert!(validate_value(
        "birthday",
        &json!({"calendar":"lunar","precision":"month_day","month":7,"day":12})
    )
    .is_ok());
    assert!(validate_value(
        "birthday",
        &json!({"calendar":"gregorian","precision":"date","year":2001,"month":2,"day":29})
    )
    .is_err());
    assert!(validate_value(
        "birthday",
        &json!({"calendar":"gregorian","precision":"month_day","year":2000,"month":1,"day":1})
    )
    .is_err());
}

#[test]
fn merge_and_split_preserve_original_provenance_and_identity() {
    let mut db = database();
    let a = create_person(&mut db, "小张", "alice");
    let b = create_person(&mut db, "老张", "alice");
    let assertion = propose(&mut db, &a.id, "hobby", json!("象棋"));
    approve(&mut db, &assertion.id, vec![]).unwrap();
    let a = person(&db, Access::Admin("admin"), &a.id).unwrap();
    bind_identity(
        &mut db,
        Access::Admin("admin"),
        &a.id,
        a.revision,
        &BindIdentity {
            conversation_id: "alice".into(),
            alias: "QQ昵称".into(),
            confirmed: true,
            reason: "本人核验".into(),
        },
    )
    .unwrap();
    let a = person(&db, Access::Admin("admin"), &a.id).unwrap();
    let b = person(&db, Access::Admin("admin"), &b.id).unwrap();
    merge(
        &mut db,
        Access::Admin("admin"),
        &a.id,
        a.revision,
        &MergePeople {
            target_person_id: b.id.clone(),
            target_revision: b.revision,
            confirmed: true,
            reason: "本人确认两个昵称属于同一人".into(),
        },
    )
    .unwrap();
    let merged = detail(&mut db, Access::Owner("owner-a"), &b.id, now()).unwrap();
    assert_eq!(merged.assertions[0].person_id, a.id);
    assert_eq!(merged.identities[0].person_id, a.id);
    let rev = person(&db, Access::Admin("admin"), &a.id).unwrap().revision;
    split(
        &mut db,
        Access::Admin("admin"),
        &a.id,
        rev,
        &ConfirmChange {
            confirmed: true,
            reason: "身份误合并更正".into(),
        },
    )
    .unwrap();
    assert!(detail(&mut db, Access::Owner("owner-a"), &b.id, now())
        .unwrap()
        .assertions
        .is_empty());
    assert_eq!(
        detail(&mut db, Access::Owner("owner-a"), &a.id, now())
            .unwrap()
            .assertions
            .len(),
        1
    );
}

#[test]
fn directed_edges_require_review_and_are_not_implicitly_reciprocal() {
    let mut db = database();
    let a = create_person(&mut db, "甲", "alice");
    let b = create_person(&mut db, "乙", "alice");
    let e = create_edge(
        &mut db,
        Access::Admin("admin"),
        &CreateEdge {
            from_person_id: a.id.clone(),
            to_person_id: b.id.clone(),
            relationship_type: "friend".into(),
            evidence: manual(),
        },
    )
    .unwrap();
    assert!(edges(&db, Access::Owner("owner-a")).unwrap().is_empty());
    review_edge(
        &mut db,
        Access::Admin("admin"),
        &e.id,
        1,
        &ReviewAssertion {
            decision: "approve".into(),
            supersedes: vec![],
            reason: "甲称乙是朋友，不推断乙的看法".into(),
        },
    )
    .unwrap();
    let visible = edges(&db, Access::Owner("owner-a")).unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].from_person_id, a.id);
    assert_eq!(visible[0].to_person_id, b.id);
    assert!(edges(&db, Access::Owner("owner-b")).unwrap().is_empty());
}

#[test]
fn forgetting_removes_live_and_revision_content() {
    let mut db = database();
    let p = create_person(&mut db, "待遗忘人物", "alice");
    let a = propose(&mut db, &p.id, "preference", json!("私有金丝雀内容"));
    approve(&mut db, &a.id, vec![]).unwrap();
    forget_assertion(&mut db, Access::Admin("admin"), &a.id, 2).unwrap();
    let live: i64 = db
        .query_row(
            "SELECT count(*) FROM relationship_assertions WHERE value_json LIKE '%金丝雀%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let old: i64 = db
        .query_row(
            "SELECT count(*) FROM relationship_assertion_versions WHERE value_json LIKE '%金丝雀%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!((live, old), (0, 0));
    assert!(detail(&mut db, Access::Owner("owner-a"), &p.id, now())
        .unwrap()
        .assertions
        .is_empty());
}

#[test]
fn source_forgetting_invalidates_all_dependent_records_and_stale_context() {
    let mut db = database();
    let p = create_person(&mut db, "某人", "alice");
    let a = propose(&mut db, &p.id, "hobby", json!("网球"));
    approve(&mut db, &a.id, vec![]).unwrap();
    let tx = db.transaction().unwrap();
    invalidate_source_in(&tx, "manual", &a.source_id).unwrap();
    tx.commit().unwrap();
    assert!(assertion(&db, Access::Admin("admin"), &a.id).is_err());
    let old:i64=db.query_row("SELECT count(*) FROM relationship_assertion_versions WHERE value_json IS NOT NULL AND assertion_id=?1",[&a.id],|r|r.get(0)).unwrap();
    assert_eq!(old, 0);
}

#[test]
fn fixed_point_decay_is_frequency_independent_and_clock_rollback_is_harmless() {
    let mut a = database();
    let p = create_person(&mut a, "某人", "alice");
    let t = now();
    a.execute(
        "UPDATE relationship_people SET created_at=?1 WHERE id=?2",
        params![t, p.id],
    )
    .unwrap();
    a.execute(
        "UPDATE relationship_affinity_states SET settled_at=?1 WHERE person_id=?2",
        params![t, p.id],
    )
    .unwrap();
    let mut b = database();
    let q = create_person(&mut b, "某人", "alice");
    b.execute(
        "UPDATE relationship_people SET created_at=?1 WHERE id=?2",
        params![t, q.id],
    )
    .unwrap();
    b.execute(
        "UPDATE relationship_affinity_states SET settled_at=?1 WHERE person_id=?2",
        params![t, q.id],
    )
    .unwrap();
    for step in 1..=1000 {
        settle_in(&a, &p.id, t + GRACE + step * 71).unwrap();
    }
    settle_in(&b, &q.id, t + GRACE + 71000).unwrap();
    assert_eq!(
        affinity(&a, &p.id).unwrap().score,
        affinity(&b, &q.id).unwrap().score
    );
    let before = affinity(&a, &p.id).unwrap();
    settle_in(&a, &p.id, t - 100).unwrap();
    let after = affinity(&a, &p.id).unwrap();
    assert_eq!(before.score, after.score);
    assert_eq!(before.settled_at, after.settled_at);
}

#[test]
fn manual_affinity_is_idempotent_capped_and_contact_is_separate() {
    let mut db = database();
    let p = create_person(&mut db, "某人", "alice");
    let b = AffinityInput {
        event_key: "once".into(),
        kind: "manual_adjustment".into(),
        delta: 5.0,
        evidence: manual(),
    };
    assert_eq!(
        add_affinity(&mut db, Access::Admin("admin"), &p.id, 1, &b)
            .unwrap()
            .score,
        55.0
    );
    assert_eq!(
        add_affinity(&mut db, Access::Admin("admin"), &p.id, 1, &b)
            .unwrap()
            .score,
        55.0
    );
    let permission = contact(&db, &p.id).unwrap();
    assert_eq!(permission.status, "not_granted");
    assert!(!permission.delivery_available);
    let denied = ContactInput {
        status: "granted".into(),
        conversation_id: None,
        expires_at: None,
        evidence: "管理员自己决定".into(),
    };
    assert_eq!(
        set_contact(&mut db, Access::Admin("admin"), &p.id, 0, &denied)
            .unwrap_err()
            .code,
        "recipient_verification_required"
    );
}

#[test]
fn query_context_binds_verified_owner_and_tracks_exact_assertion_revision() {
    let mut db = database();
    let p = create_person(&mut db, "甲", "alice");
    let a = propose(&mut db, &p.id, "hobby", json!("甲的专属标记"));
    approve(&mut db, &a.id, vec![]).unwrap();
    let rev = person(&db, Access::Admin("admin"), &p.id).unwrap().revision;
    bind_identity(
        &mut db,
        Access::Admin("admin"),
        &p.id,
        rev,
        &BindIdentity {
            conversation_id: "alice".into(),
            alias: "甲".into(),
            confirmed: true,
            reason: "本人核验".into(),
        },
    )
    .unwrap();
    db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at) VALUES('run-a','alice','owner-a','a','你好','{}','running',1,1),('run-b','bob','owner-b','b','你好','{}','running',1,1)",[]).unwrap();
    let tx = db.transaction().unwrap();
    let candidate = context_candidate_for_run_in(&tx, "run-a", 8000).unwrap();
    assert_eq!(
        tx.query_row(
            "SELECT count(*) FROM relationship_context_uses WHERE run_id='run-a'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    let ca = context_for_run_in(&tx, "run-a", 8000).unwrap();
    assert_eq!(candidate, ca);
    assert!(
        tx.query_row(
            "SELECT count(*) FROM relationship_context_uses WHERE run_id='run-a'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap()
            > 0
    );
    let cb = context_for_run_in(&tx, "run-b", 8000).unwrap();
    assert!(ca.to_string().contains("甲的专属标记"));
    assert!(!cb.to_string().contains("专属标记"));
    tx.commit().unwrap();
    forget_assertion(&mut db, Access::Admin("admin"), &a.id, 2).unwrap();
    let status: String = db
        .query_row("SELECT status FROM runs WHERE id='run-a'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(status, "cancelled");
}

#[test]
fn merge_conflicts_and_expired_fields_do_not_become_context() {
    let mut db = database();
    let p = create_person(&mut db, "甲", "alice");
    let b = CreateAssertion {
        field: "job".into(),
        value: json!("临时状态"),
        evidence: manual(),
        valid_from: Some(1),
        valid_until: Some(2),
    };
    let a = propose_assertion(&mut db, Access::Admin("admin"), &p.id, 1, &b).unwrap();
    approve(&mut db, &a.id, vec![]).unwrap();
    assert!(detail(&mut db, Access::Owner("owner-a"), &p.id, now())
        .unwrap()
        .assertions
        .is_empty());
}

#[test]
fn cross_account_identity_group_does_not_grant_other_source_fields_or_node_labels() {
    let mut db = database();
    let a = create_person(&mut db, "QQ称呼", "alice");
    let b = create_person(&mut db, "微信私有称呼", "bob");
    let aa = propose(&mut db, &a.id, "hobby", json!("QQ_PRIVATE_CANARY"));
    let bb = propose(&mut db, &b.id, "job", json!("WX_PRIVATE_CANARY"));
    approve(&mut db, &aa.id, vec![]).unwrap();
    approve(&mut db, &bb.id, vec![]).unwrap();
    let a = person(&db, Access::Admin("admin"), &a.id).unwrap();
    let b = person(&db, Access::Admin("admin"), &b.id).unwrap();
    let linked = identity_groups::link(
        &mut db,
        Access::Admin("admin"),
        &a.id,
        a.revision,
        &identity_groups::LinkInput {
            other_person_id: b.id.clone(),
            other_revision: b.revision,
            confirmed: true,
            reason: "两边本人分别完成身份核验".into(),
        },
    )
    .unwrap();
    assert_eq!(linked.members.len(), 2);
    let view_a = detail(&mut db, Access::Owner("owner-a"), &a.id, now()).unwrap();
    let view_b = detail(&mut db, Access::Owner("owner-b"), &b.id, now()).unwrap();
    let a_json = serde_json::to_string(&view_a).unwrap();
    let b_json = serde_json::to_string(&view_b).unwrap();
    assert!(a_json.contains("QQ_PRIVATE_CANARY"));
    assert!(!a_json.contains("WX_PRIVATE_CANARY"));
    assert!(!a_json.contains("微信私有称呼"));
    assert!(!b_json.contains("QQ_PRIVATE_CANARY"));
    assert_eq!(view_a.identity_group.unwrap().members.len(), 1);
    let admin = detail(&mut db, Access::Admin("admin"), &a.id, now()).unwrap();
    assert_eq!(admin.assertions.len(), 2);
    let current = person(&db, Access::Admin("admin"), &a.id).unwrap();
    identity_groups::unlink(
        &mut db,
        Access::Admin("admin"),
        &a.id,
        current.revision,
        &ConfirmChange {
            confirmed: true,
            reason: "撤销跨账号同人映射".into(),
        },
    )
    .unwrap();
    assert!(identity_groups::get(&db, Access::Admin("admin"), &a.id)
        .unwrap()
        .is_none());
    assert!(identity_groups::get(&db, Access::Admin("admin"), &b.id)
        .unwrap()
        .is_none());
    assert_eq!(
        detail(&mut db, Access::Admin("admin"), &a.id, now())
            .unwrap()
            .assertions
            .len(),
        1
    );
}

#[test]
fn cross_account_group_cannot_be_created_from_model_or_unconfirmed_link() {
    let mut db = database();
    let a = create_person(&mut db, "甲", "alice");
    let b = create_person(&mut db, "乙", "bob");
    let input = identity_groups::LinkInput {
        other_person_id: b.id,
        other_revision: 1,
        confirmed: false,
        reason: "待核验".into(),
    };
    assert!(identity_groups::link(&mut db, Access::Owner("owner-a"), &a.id, 1, &input).is_err());
    assert_eq!(
        identity_groups::link(&mut db, Access::Admin("admin"), &a.id, 1, &input)
            .unwrap_err()
            .code,
        "identity_group_confirmation"
    );
}

#[test]
fn affinity_policy_versions_apply_only_after_their_effective_watermark() {
    let mut db = database();
    let p = create_person(&mut db, "甲", "alice");
    let t = now();
    db.execute(
        "UPDATE relationship_people SET created_at=?1 WHERE id=?2",
        params![t - 10 * DAY, p.id],
    )
    .unwrap();
    db.execute(
        "UPDATE relationship_affinity_states SET settled_at=?1 WHERE person_id=?2",
        params![t - 10 * DAY, p.id],
    )
    .unwrap();
    let input = affinity_policy::PolicyInput {
        initial: 50,
        minimum: 0,
        maximum: 100,
        daily_decay: 2.0,
        grace_seconds: 0,
        downtime_cap_seconds: 7 * DAY,
        confirmed: true,
        reason: "经预演调整后续速率".into(),
    };
    let policy = affinity_policy::save(&mut db, Access::Admin("admin"), 1, &input).unwrap();
    let current = affinity(&db, &p.id).unwrap();
    assert!((current.score - 43.0).abs() < 0.001);
    assert_eq!(policy.revision, 2);
    settle_in(&db, &p.id, policy.effective_at + DAY).unwrap();
    assert!((affinity(&db, &p.id).unwrap().score - 41.0).abs() < 0.001);
    assert!(affinity_policy::save(&mut db, Access::Admin("admin"), 1, &input).is_err());
}

#[test]
fn downtime_cap_is_applied_once_and_not_on_every_read_or_restart() {
    let mut db = database();
    let p = create_person(&mut db, "甲", "alice");
    let end = now();
    let heartbeat = end - 40 * DAY;
    db.execute(
        "UPDATE relationship_people SET created_at=?1 WHERE id=?2",
        params![heartbeat, p.id],
    )
    .unwrap();
    db.execute(
        "UPDATE relationship_affinity_states SET settled_at=?1 WHERE person_id=?2",
        params![heartbeat, p.id],
    )
    .unwrap();
    db.execute(
        "UPDATE relationship_checkpoints SET value=?1 WHERE key='heartbeat'",
        [heartbeat],
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    affinity_policy::recover_in(&tx, end).unwrap();
    tx.commit().unwrap();
    assert_eq!(affinity(&db, &p.id).unwrap().score, 43.0);
    let tx = db.transaction().unwrap();
    affinity_policy::recover_in(&tx, end).unwrap();
    tx.commit().unwrap();
    assert_eq!(affinity(&db, &p.id).unwrap().score, 43.0);
    settle_in(&db, &p.id, end).unwrap();
    assert_eq!(affinity(&db, &p.id).unwrap().score, 43.0);
    affinity_policy::replay_in(&db, &p.id).unwrap();
    assert_eq!(affinity(&db, &p.id).unwrap().score, 43.0);
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM relationship_affinity_events WHERE kind='outage_checkpoint'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn policy_preview_is_side_effect_free_and_forgetting_replays_original_policy_versions() {
    let mut db = database();
    let p = create_person(&mut db, "甲", "alice");
    let b = AffinityInput {
        event_key: "adjust".into(),
        kind: "manual_adjustment".into(),
        delta: 5.0,
        evidence: manual(),
    };
    add_affinity(&mut db, Access::Admin("admin"), &p.id, 1, &b).unwrap();
    let input = affinity_policy::PolicyInput {
        initial: 50,
        minimum: 0,
        maximum: 100,
        daily_decay: 0.5,
        grace_seconds: 0,
        downtime_cap_seconds: 3 * DAY,
        confirmed: true,
        reason: "版本化策略".into(),
    };
    let preview = affinity_policy::preview(&db, &input, 2 * DAY).unwrap();
    assert_eq!(preview["projected_from_initial"], json!(49.0));
    assert_eq!(affinity_policy::current(&db).unwrap().revision, 1);
    affinity_policy::save(&mut db, Access::Admin("admin"), 1, &input).unwrap();
    let source: String = db
        .query_row(
            "SELECT source_id FROM relationship_affinity_events WHERE event_key='adjust'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let tx = db.transaction().unwrap();
    invalidate_source_in(&tx, "manual", &source).unwrap();
    tx.commit().unwrap();
    assert!((affinity(&db, &p.id).unwrap().score - 50.0).abs() < 0.001);
}

#[test]
fn create_person_json_has_explicit_scope_and_rejects_unknown_owner_injection() {
    let body = json!({"display_name":"甲","visibility":"private","source_conversation_id":"alice","publish":false,"reason":"核验"});
    assert!(serde_json::from_value::<CreatePerson>(body).is_ok());
    let body = json!({"display_name":"甲","visibility":"private","source_conversation_id":"alice","publish":false,"reason":"核验","source_owner_id":"injected"});
    assert!(serde_json::from_value::<CreatePerson>(body).is_err());
}

#[test]
fn profile_and_relationship_reasons_share_the_complete_serialized_context_budget() {
    let mut db = database();
    let a = create_person(&mut db, "甲", "alice");
    let b = create_person(&mut db, "乙", "alice");
    let mut source = manual();
    source.reason = "x".repeat(1900);
    let edge = create_edge(
        &mut db,
        Access::Admin("admin"),
        &CreateEdge {
            from_person_id: a.id.clone(),
            to_person_id: b.id.clone(),
            relationship_type: "friend".into(),
            evidence: source,
        },
    )
    .unwrap();
    review_edge(
        &mut db,
        Access::Admin("admin"),
        &edge.id,
        1,
        &ReviewAssertion {
            decision: "approve".into(),
            supersedes: vec![],
            reason: "核对长来源内容".into(),
        },
    )
    .unwrap();
    let rev = person(&db, Access::Admin("admin"), &a.id).unwrap().revision;
    bind_identity(
        &mut db,
        Access::Admin("admin"),
        &a.id,
        rev,
        &BindIdentity {
            conversation_id: "alice".into(),
            alias: "甲".into(),
            confirmed: true,
            reason: "核验".into(),
        },
    )
    .unwrap();
    db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at) VALUES('budget-run','alice','owner-a','budget','你好','{}','running',1,1)",[]).unwrap();
    let tx = db.transaction().unwrap();
    let context = context_for_run_in(&tx, "budget-run", 1000).unwrap();
    assert!(serde_json::to_vec(&context).unwrap().len() <= 1000);
    assert!(context["people"][0]["relationship_edges"]
        .as_array()
        .unwrap()
        .is_empty());
    tx.commit().unwrap();
}
