use super::*;

fn database() -> Connection {
    let mut db = Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../schema.sql")).unwrap();
    db.execute_batch("ALTER TABLE runs ADD COLUMN run_kind TEXT NOT NULL DEFAULT 'model';ALTER TABLE runs ADD COLUMN character_snapshot_json TEXT;CREATE TABLE conversation_character_bindings(conversation_id TEXT PRIMARY KEY,draft_card_json TEXT);CREATE TABLE run_prompt_snapshots(run_id TEXT PRIMARY KEY,messages_json TEXT);
    INSERT INTO conversations VALUES('alice','owner-a','Alice',NULL,0),('bob','owner-b','Bob',NULL,0);").unwrap();
    memory::initialize(&mut db).unwrap();
    crate::conversation_summary::initialize(&db).unwrap();
    crate::relationships::initialize(&db).unwrap();
    initialize(&mut db).unwrap();
    crate::delivery_context::ensure_schema(&db).unwrap();
    initialize_delivery_capture(&db).unwrap();
    db
}
fn timestamp(day: &str) -> i64 {
    boundary(date(day).unwrap(), TIMEZONE.parse().unwrap()).unwrap() + 3600
}
fn input(text: &str, conversation: &str, day: &str) -> Note {
    Note {
        content: text.into(),
        source_conversation_id: Some(conversation.into()),
        visibility: "private".into(),
        publish: false,
        happened_at: Some(timestamp(day)),
    }
}
fn build(db: &mut Connection, owner: &str, period: &str, day: &str) -> Entry {
    let tx = db.transaction().unwrap();
    let e = consolidate_in(&tx, "private", Some(owner), period, day, TIMEZONE, now()).unwrap();
    tx.commit().unwrap();
    e
}

#[test]
fn source_acl_is_applied_before_search_and_pagination() {
    let mut db = database();
    let a = note(
        &mut db,
        "admin",
        &input("苹果蓝色鲸鱼", "alice", "2026-10-01"),
    )
    .unwrap();
    note(
        &mut db,
        "admin",
        &input("苹果不能给A看的红色鲨鱼", "bob", "2026-10-01"),
    )
    .unwrap();
    build(&mut db, "owner-a", "day", "2026-10-01");
    build(&mut db, "owner-b", "day", "2026-10-01");
    assert!(event(&db, &a.id, Some("owner-b")).is_err());
    let q = ListOptions {
        q: Some("苹果".into()),
        limit: 1,
        ..Default::default()
    };
    let page = list(&db, &q, Some("owner-a")).unwrap();
    assert_eq!(page.total, 1);
    assert!(page.items[0].facts.contains("蓝色鲸鱼"));
    assert!(!page.items[0].facts.contains("红色鲨鱼"));
    assert_eq!(events(&db, &q, Some("owner-b")).unwrap().total, 1);
}
#[test]
fn correction_clears_versions_and_all_period_derivatives() {
    let mut db = database();
    let event = note(
        &mut db,
        "admin",
        &input("旧秘密蓝鲸", "alice", "2026-10-01"),
    )
    .unwrap();
    let day = build(&mut db, "owner-a", "day", "2026-10-01");
    let week = build(&mut db, "owner-a", "week", "2026-10-01");
    let month = build(&mut db, "owner-a", "month", "2026-10-01");
    let changed = correct(&mut db, "admin", &event.id, 1, "更正为喜欢天空").unwrap();
    assert_eq!(changed.revision, 2);
    for id in [&day.id, &week.id, &month.id] {
        assert!(entry(&db, id, None).is_err());
    }
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM diary_entry_versions WHERE facts LIKE '%蓝鲸%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    assert!(correct(&mut db, "admin", &event.id, 1, "并发旧修改").is_err());
    let rebuilt = build(&mut db, "owner-a", "day", "2026-10-01");
    assert!(rebuilt.content.contains("天空"));
    assert!(!rebuilt.content.contains("旧秘密"));
}
#[test]
fn forgotten_origin_is_tombstoned_and_never_returns_on_reinitialization() {
    let mut db = database();
    let event = note(
        &mut db,
        "admin",
        &input("已删除的合成内容", "alice", "2026-10-01"),
    )
    .unwrap();
    let old = build(&mut db, "owner-a", "day", "2026-10-01");
    forget(&mut db, "admin", &event.id, 1).unwrap();
    assert!(entry(&db, &old.id, None).is_err());
    assert!(super::event(&db, &event.id, None).is_err());
    initialize(&mut db).unwrap();
    assert!(super::event(&db, &event.id, None).is_err());
    let rebuilt = build(&mut db, "owner-a", "day", "2026-10-01");
    assert_eq!(rebuilt.source_count, 0);
    assert!(!rebuilt.content.contains("合成内容"));
}
#[test]
fn no_interaction_and_unobserved_days_are_not_fabricated() {
    let mut db = database();
    let day = build(&mut db, "owner-a", "day", "2026-10-01");
    assert_eq!(day.source_count, 0);
    assert!(day.content.contains("未观测"));
    assert!(day.feelings.is_empty());
    assert_eq!(day.generator, "evidence_excerpt_v1");
    db.execute("UPDATE diary_meta SET started_at=0", [])
        .unwrap();
    let complete = build(&mut db, "owner-a", "day", "2026-10-02");
    assert_eq!(complete.status, "final");
    assert!(complete.content.contains("今日无新互动"));
}
#[test]
fn repeated_period_generation_is_idempotent_and_month_excludes_cross_month_events() {
    let mut db = database();
    note(&mut db, "admin", &input("九月资料", "alice", "2026-09-30")).unwrap();
    note(&mut db, "admin", &input("十月资料", "alice", "2026-10-01")).unwrap();
    let a = build(&mut db, "owner-a", "day", "2026-10-01");
    let b = build(&mut db, "owner-a", "day", "2026-10-01");
    assert_eq!((a.id, a.revision), (b.id, b.revision));
    let week = build(&mut db, "owner-a", "week", "2026-10-01");
    assert_eq!(week.period_start, "2026-09-28");
    assert_eq!(week.source_count, 2);
    let month = build(&mut db, "owner-a", "month", "2026-10-01");
    assert_eq!(month.source_count, 1);
    assert!(!month.facts.contains("九月资料"));
    let provenance = source_page(&db, &month.id, Some("owner-a"), 100, 0).unwrap();
    assert_eq!(provenance.total, 1);
    assert!(provenance.items[0].content.contains("十月资料"));
}
#[test]
fn old_memory_id_visibility_and_tombstone_are_retained() {
    let mut db = database();
    let legacy = memory::create(
        &mut db,
        "admin",
        &memory::CreateMemory {
            content: "迁移前私有记忆".into(),
            visibility: memory::Visibility::Private,
            source_conversation_id: Some("alice".into()),
            source_message_id: None,
            publish: false,
        },
    )
    .unwrap();
    let id = format!("legacy:{}", legacy.id);
    let imported = event(&db, &id, Some("owner-a")).unwrap();
    assert_eq!(imported.origin_id, legacy.id);
    assert_eq!(imported.content, legacy.content);
    assert!(event(&db, &id, Some("owner-b")).is_err());
    initialize(&mut db).unwrap();
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM diary_events WHERE origin_id=?1",
            [&legacy.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    memory::forget(&mut db, "admin", &legacy.id, 1).unwrap();
    assert!(event(&db, &id, None).is_err());
    initialize(&mut db).unwrap();
    assert!(event(&db, &id, None).is_err());
}
#[test]
fn source_authorization_and_model_egress_are_independent() {
    let mut db = database();
    let policy = SourcePolicy {
        source_owner_id: "owner-a".into(),
        enabled: true,
        allow_model: false,
        timezone: TIMEZONE.into(),
    };
    save_policy(&mut db, "admin", 0, &policy).unwrap();
    note(&mut db, "admin", &input("日记事实", "alice", "2026-10-01")).unwrap();
    build(&mut db, "owner-a", "day", "2026-10-01");
    assert!(prepare_generation_in(&db).unwrap().is_none());
    let policy = SourcePolicy {
        allow_model: true,
        ..policy
    };
    save_policy(&mut db, "admin", 1, &policy).unwrap();
    assert!(save_policy(&mut db, "admin", 1, &policy).is_err());
    note(
        &mut db,
        "admin",
        &input("新日记事实", "alice", "2026-10-02"),
    )
    .unwrap();
    build(&mut db, "owner-a", "day", "2026-10-02");
    assert!(prepare_generation_in(&db).unwrap().is_some());
}
#[test]
fn model_result_is_fenced_against_correction_forgetting_and_policy_changes() {
    let mut db = database();
    save_policy(
        &mut db,
        "admin",
        0,
        &SourcePolicy {
            source_owner_id: "owner-a".into(),
            enabled: true,
            allow_model: true,
            timezone: TIMEZONE.into(),
        },
    )
    .unwrap();
    let e = note(
        &mut db,
        "admin",
        &input("真实蓝色偏好", "alice", "2026-10-01"),
    )
    .unwrap();
    build(&mut db, "owner-a", "day", "2026-10-01");
    let claim = prepare_generation_in(&db).unwrap().unwrap();
    correct(&mut db, "admin", &e.id, 1, "改成绿色偏好").unwrap();
    let output = GenerationOutput {
        facts: "我记下对方喜欢蓝色".into(),
        feelings: String::new(),
    };
    assert!(commit_generation_in(&db, &claim, &output, "fixture").is_err());
    build(&mut db, "owner-a", "day", "2026-10-01");
    let next = prepare_generation_in(&db).unwrap().unwrap();
    let out = GenerationOutput {
        facts: "对方告诉我喜欢绿色".into(),
        feelings: "我觉得这次交流轻松。".into(),
    };
    let committed = commit_generation_in(&db, &next, &out, "fixture-model").unwrap();
    assert_eq!(committed.generator, "model:fixture-model");
    assert!(committed.content.contains("非他人事实"));
    assert!(commit_generation_in(&db, &next, &out, "fixture-model").is_err());
}
#[test]
fn auto_capture_excludes_controls_drafts_sensitive_and_automatic_runs() {
    let mut db = database();
    save_policy(
        &mut db,
        "admin",
        0,
        &SourcePolicy {
            source_owner_id: "owner-a".into(),
            enabled: true,
            allow_model: false,
            timezone: TIMEZONE.into(),
        },
    )
    .unwrap();
    for (id, text, kind) in [
        ("normal", "今天读了一首诗", "model"),
        ("control", "/memory list", "model"),
        ("secret", "验证码是123456", "model"),
        ("automatic", "自动任务结果", "trigger_model"),
    ] {
        db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,response,created_at,updated_at,run_kind) VALUES(?1,'alice','owner-a',?1,?2,'{}','completed','收到',?3,?3,?4)",params![id,text,now(),kind]).unwrap();
    }
    assert!(capture_completed_in(&db, "normal").unwrap());
    assert!(!capture_completed_in(&db, "normal").unwrap());
    for id in ["control", "secret", "automatic"] {
        assert!(!capture_completed_in(&db, id).unwrap());
    }
    db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,response,created_at,updated_at,character_snapshot_json) VALUES('frozen-draft','alice','owner-a','frozen-draft','冻结的草稿测试','{}','completed','测试结果',?1,?1,'{\"is_test\":true}')",[now()]).unwrap();
    assert!(!capture_completed_in(&db, "frozen-draft").unwrap());
    db.execute(
        "INSERT INTO conversation_character_bindings VALUES('alice','{}')",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,response,created_at,updated_at) VALUES('draft','alice','owner-a','draft','草稿测试','{}','completed','测试结果',?1,?1)",[now()]).unwrap();
    assert!(!capture_completed_in(&db, "draft").unwrap());
}
#[test]
fn shared_publishing_requires_explicit_confirmation_and_secret_text_is_rejected() {
    let mut db = database();
    let mut n = input("共享事实", "alice", "2026-10-01");
    n.visibility = "shared".into();
    assert!(note(&mut db, "admin", &n).is_err());
    n.publish = true;
    let e = note(&mut db, "admin", &n).unwrap();
    assert!(event(&db, &e.id, Some("owner-b")).is_ok());
    n.content = "api_key=should-never-be-stored".into();
    assert!(note(&mut db, "admin", &n).is_err());
}
#[test]
fn persisted_day_checkpoint_and_clock_rollback_are_explicit() {
    let mut db = database();
    note(&mut db, "admin", &input("一天经历", "alice", "2026-10-01")).unwrap();
    let ts = now();
    let tx = db.transaction().unwrap();
    tick_in(&tx, ts).unwrap();
    tx.commit().unwrap();
    let count: i64 = db
        .query_row("SELECT count(*) FROM diary_entries", [], |r| r.get(0))
        .unwrap();
    assert!(count >= 1);
    let tx = db.transaction().unwrap();
    assert_eq!(tick_in(&tx, ts - 100).unwrap()["reason"], "clock_rollback");
    tx.commit().unwrap();
    initialize(&mut db).unwrap();
    let after: i64 = db
        .query_row("SELECT count(*) FROM diary_entries", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, after);
}

#[test]
fn revoking_and_reauthorizing_never_resurrects_prior_source_events() {
    let mut db = database();
    let p = SourcePolicy {
        source_owner_id: "owner-a".into(),
        enabled: true,
        allow_model: false,
        timezone: TIMEZONE.into(),
    };
    save_policy(&mut db, "admin", 0, &p).unwrap();
    let old = note(
        &mut db,
        "admin",
        &input("撤权前的记录", "alice", "2026-10-01"),
    )
    .unwrap();
    let prior = build(&mut db, "owner-a", "day", "2026-10-01");
    save_policy(
        &mut db,
        "admin",
        1,
        &SourcePolicy {
            enabled: false,
            ..p.clone()
        },
    )
    .unwrap();
    assert!(event(&db, &old.id, None).is_err());
    assert!(entry(&db, &prior.id, None).is_err());
    save_policy(&mut db, "admin", 2, &p).unwrap();
    assert!(event(&db, &old.id, None).is_err());
    let fresh = note(
        &mut db,
        "admin",
        &input("新授权后记录", "alice", "2026-10-01"),
    )
    .unwrap();
    assert!(event(&db, &fresh.id, Some("owner-a")).is_ok());
    let rebuilt = build(&mut db, "owner-a", "day", "2026-10-01");
    assert_eq!(rebuilt.source_count, 1);
    assert!(!rebuilt.content.contains("撤权前"));
}

#[test]
fn long_chinese_conversation_query_respects_byte_budget_without_breaking_utf8() {
    let mut db = database();
    note(
        &mut db,
        "admin",
        &input("普通私有日记", "alice", "2026-10-01"),
    )
    .unwrap();
    build(&mut db, "owner-a", "day", "2026-10-01");
    let query = "中文".repeat(80);
    assert!(query.len() > 200);
    assert!(retrieve(&db, "owner-a", &query, 6, 8000).is_ok());
    assert!(clip(&query, 192).is_char_boundary(192));
    assert!(retrieve(&db, "owner-a", &format!("{}a", query), 6, 8000).is_ok());
}

#[test]
fn trigram_search_keeps_owner_acl_and_removes_obsolete_postings() {
    let mut db = database();
    let a = note(
        &mut db,
        "admin",
        &input("蓝色鲸鱼甲方私有", "alice", "2026-10-01"),
    )
    .unwrap();
    note(
        &mut db,
        "admin",
        &input("蓝色鲸鱼乙方私有", "bob", "2026-10-01"),
    )
    .unwrap();
    build(&mut db, "owner-a", "day", "2026-10-01");
    build(&mut db, "owner-b", "day", "2026-10-01");
    let q = ListOptions {
        q: Some("蓝色鲸鱼".into()),
        limit: 1,
        ..Default::default()
    };
    let page = list(&db, &q, Some("owner-a")).unwrap();
    assert_eq!(page.total, 1);
    assert!(page.items[0].content.contains("甲方"));
    assert_eq!(events(&db, &q, Some("owner-a")).unwrap().total, 1);
    correct(&mut db, "admin", &a.id, 1, "绿色海豚甲方私有").unwrap();
    assert_eq!(list(&db, &q, Some("owner-a")).unwrap().total, 0);
    assert_eq!(events(&db, &q, Some("owner-a")).unwrap().total, 0);
    let indexed:i64=db.query_row("SELECT count(*) FROM diary_event_fts WHERE diary_event_fts MATCH 'content : \"蓝色鲸鱼\" AND owner_token : \"owner-a\"'",[],|r|r.get(0)).unwrap();
    assert_eq!(indexed, 0);
    let malformed = ListOptions {
        q: Some("\" OR * : 其他".into()),
        ..Default::default()
    };
    assert!(list(&db, &malformed, Some("owner-a")).is_ok());
}

#[test]
fn oversized_week_keeps_completed_days_and_other_source_checkpoints_committed() {
    let mut db = database();
    for i in 0..201 {
        let day = if i < 101 { "2026-10-01" } else { "2026-10-02" };
        note(
            &mut db,
            "admin",
            &input(&format!("有界整理合成来源{i}"), "alice", day),
        )
        .unwrap();
    }
    note(
        &mut db,
        "admin",
        &input("另一来源不能被阻断", "bob", "2026-10-01"),
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    let outcome = tick_in(&tx, now()).unwrap();
    tx.commit().unwrap();
    assert_eq!(outcome["status"], "completed");
    let a_days:i64=db.query_row("SELECT count(*) FROM diary_entries WHERE source_owner_id='owner-a' AND period='day' AND status!='invalidated'",[],|r|r.get(0)).unwrap();
    assert!(a_days >= 4);
    let b_days:i64=db.query_row("SELECT count(*) FROM diary_entries WHERE source_owner_id='owner-b' AND period='day' AND status!='invalidated'",[],|r|r.get(0)).unwrap();
    assert!(b_days >= 4);
    let checkpoint: String = db
        .query_row(
            "SELECT last_date FROM diary_scope_checkpoints WHERE scope_key='private:owner-a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(checkpoint, "2026-10-04");
    assert_eq!(status(&db).unwrap()["blocked_consolidations"], 0);
}

#[test]
fn shared_source_forget_invalidates_other_owners_copied_context() {
    let mut db = database();
    let mut n = input("合成共享来源，仍保留A的会话出处", "alice", "2026-10-01");
    n.visibility = "shared".into();
    n.publish = true;
    let event = note(&mut db, "admin", &n).unwrap();
    db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,response,created_at,updated_at) VALUES('bob-derived','bob','owner-b','bob-derived','generic','{}','completed','old shared derivation',1,1)",[]).unwrap();
    assert_eq!(crate::memory_context::history_floor(&db, "bob").unwrap(), 0);
    forget(&mut db, "admin", &event.id, 1).unwrap();
    assert!(crate::memory_context::history_floor(&db, "bob").unwrap() > 0);
}

#[test]
fn month_310_sources_reduces_bounded_parent_batches_resumes_and_forgets_full_chain() {
    let mut db = database();
    db.execute("UPDATE diary_meta SET started_at=0", [])
        .unwrap();
    let mut policy = SourcePolicy {
        source_owner_id: "owner-a".into(),
        enabled: true,
        allow_model: false,
        timezone: TIMEZONE.into(),
    };
    save_policy(&mut db, "admin", 0, &policy).unwrap();
    let mut removed = None;
    for d in 1..=31 {
        let day = format!("2026-08-{d:02}");
        for i in 0..10 {
            let e = note(
                &mut db,
                "admin",
                &input(
                    &format!(
                        "合成第{d}天第{i}件经历：{}",
                        "今天读了一小段描写山川的文字，记下了作者视角和时间顺序的问题。".repeat(3)
                    ),
                    "alice",
                    &day,
                ),
            )
            .unwrap();
            if d == 20 && i == 0 {
                removed = Some(e);
            }
        }
        let e = build(&mut db, "owner-a", "day", &day);
        assert_eq!(e.source_count, 10);
        assert!(!e.facts.contains("摘录达到正文预算"));
    }
    policy.allow_model = true;
    save_policy(&mut db, "admin", 1, &policy).unwrap();
    let month = build(&mut db, "owner-a", "month", "2026-08-01");
    let week = build(&mut db, "owner-a", "week", "2026-08-20");
    assert_eq!(month.source_count, 310);
    assert_eq!(week.source_count, 70);
    let first = source_page(&db, &month.id, Some("owner-a"), 100, 0).unwrap();
    assert_eq!(first.total, 310);
    assert!(first.has_more);
    assert_eq!(first.items.len(), 100);
    let last = source_page(&db, &month.id, Some("owner-a"), 200, 300).unwrap();
    assert_eq!(last.items.len(), 10);
    assert!(!last.has_more);
    assert!(source_page(&db, &month.id, Some("owner-b"), 100, 0).is_err());
    let mut calls = 0;
    let mut month_parts = 0;
    let mut restarted = false;
    for _ in 0..12 {
        let Some(input) = prepare_generation_in(&db).unwrap() else {
            break;
        };
        calls += 1;
        assert!(input.events.is_empty());
        assert!(
            serde_json::to_vec(&generation_payload(&input))
                .unwrap()
                .len()
                <= 44000
        );
        if input.entry_id == month.id && !input.reduction.as_ref().unwrap().final_batch {
            month_parts += 1;
        }
        let tx = db.transaction().unwrap();
        commit_generation_in(
            &tx,
            &input,
            &GenerationOutput {
                facts: format!("合成层次归纳第{calls}批，事实只来自本批父日记。"),
                feelings: "这是合成整理感受。".into(),
            },
            "bounded-fixture",
        )
        .unwrap();
        tx.commit().unwrap();
        if month_parts == 1 && !restarted {
            initialize(&mut db).unwrap();
            restarted = true;
        }
    }
    assert!(calls <= 8);
    assert!(month_parts >= 2);
    let final_month = entry(&db, &month.id, None).unwrap();
    assert_eq!(final_month.generator, "model:bounded-fixture");
    assert_eq!(final_month.source_count, 310);
    assert_eq!(
        entry(&db, &week.id, None).unwrap().generator,
        "model:bounded-fixture"
    );
    // A parent's later model-enrichment revision cannot create a new aggregate
    // loop: the earlier complete evidence version remains the frozen parent.
    let daily = build(&mut db, "owner-a", "day", "2026-08-01");
    retry_generation_in(&db, &daily.id).unwrap();
    let enriched = prepare_generation_in(&db).unwrap().unwrap();
    assert_eq!(enriched.entry_id, daily.id);
    let tx = db.transaction().unwrap();
    commit_generation_in(
        &tx,
        &enriched,
        &GenerationOutput {
            facts: "同一原始资料的后续模型整理版本，不更改原始事实。".into(),
            feelings: String::new(),
        },
        "enriched-fixture",
    )
    .unwrap();
    tx.commit().unwrap();
    assert!(entry(&db, &daily.id, None).unwrap().revision > daily.revision);
    let before = final_month.revision;
    let again = build(&mut db, "owner-a", "month", "2026-08-01");
    assert_eq!(again.revision, before);
    let e = removed.unwrap();
    forget(&mut db, "admin", &e.id, 1).unwrap();
    assert!(entry(&db, &month.id, None).is_err());
    assert!(entry(&db, &week.id, None).is_err());
    let fragments: i64 = db
        .query_row(
            "SELECT count(*) FROM diary_model_fragments WHERE entry_id=?1",
            [&month.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(fragments, 0);
    initialize(&mut db).unwrap();
    assert!(event(&db, &e.id, None).is_err());
    assert!(entry(&db, &month.id, None).is_err());
}

#[test]
fn exhausted_model_attempts_require_manual_retry_while_scheduled_noop_keeps_failure() {
    let mut db = database();
    save_policy(
        &mut db,
        "admin",
        0,
        &SourcePolicy {
            source_owner_id: "owner-a".into(),
            enabled: true,
            allow_model: true,
            timezone: TIMEZONE.into(),
        },
    )
    .unwrap();
    note(
        &mut db,
        "admin",
        &input("受控失败恢复资料", "alice", "2026-10-01"),
    )
    .unwrap();
    let e = build(&mut db, "owner-a", "day", "2026-10-01");
    for _ in 0..3 {
        let c = prepare_generation_in(&db).unwrap().unwrap();
        generation_failed_in(&db, &c.entry_id, "mock_transient_failure").unwrap();
        db.execute("UPDATE diary_model_jobs SET updated_at=0", [])
            .unwrap();
    }
    assert!(prepare_generation_in(&db).unwrap().is_none());
    build(&mut db, "owner-a", "day", "2026-10-01");
    assert!(prepare_generation_in(&db).unwrap().is_none());
    retry_generation_in(&db, &e.id).unwrap();
    assert!(prepare_generation_in(&db).unwrap().is_some());
}

#[test]
fn diary_reader_one_migrates_to_two_and_unknown_future_reader_fails() {
    let mut db = database();
    db.execute("UPDATE diary_meta SET schema_version=1", [])
        .unwrap();
    initialize(&mut db).unwrap();
    assert_eq!(status(&db).unwrap()["schema_version"], 2);
    db.execute("UPDATE diary_meta SET schema_version=3", [])
        .unwrap();
    assert!(initialize(&mut db).is_err());
}
