use super::*;

fn block(id: &str, kind: BlockKind, priority: u16, source_id: &str) -> ContextBlock {
    let content = format!("synthetic evidence {id}");
    let mut block = ContextBlock::new(
        id,
        kind,
        false,
        priority,
        vec![json!({"role":"user","content":content})],
    );
    block.source_refs.push(source(
        if kind == BlockKind::Memory {
            "memory"
        } else {
            "diary_entry"
        },
        source_id,
        1,
        &content,
    ));
    block
}

fn fixture() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE diary_entry_sources(entry_id TEXT,event_id TEXT,event_revision INTEGER);
      CREATE TABLE runs(id TEXT PRIMARY KEY,config_json TEXT NOT NULL);
      CREATE TABLE run_prompt_snapshots(run_id TEXT PRIMARY KEY,messages_json TEXT);
      INSERT INTO runs VALUES('context-run','{\"model\":{\"model\":\"synthetic-model\",\"base_url\":\"https://provider.invalid/v1\"}}');").unwrap();
    context_budget::ensure_schema(&db).unwrap();
    db
}

#[test]
fn day_week_month_shared_lineage_is_selected_once_while_independent_evidence_survives() {
    let mut db = fixture();
    db.execute_batch(
        "INSERT INTO diary_entry_sources VALUES
      ('day','shared-event',1),('week','shared-event',1),('week','second-event',1),
      ('month','shared-event',1),('month','second-event',1),('month','third-event',1),
      ('independent','independent-event',1);",
    )
    .unwrap();
    let mut blocks = vec![
        ContextBlock::new(
            "host",
            BlockKind::HostPolicy,
            true,
            1000,
            vec![json!({"role":"system","content":"fixed synthetic host policy"})],
        ),
        block("day", BlockKind::Diary, 900, "day"),
        block("week", BlockKind::Diary, 800, "week"),
        block("month", BlockKind::Diary, 700, "month"),
        block("independent", BlockKind::Diary, 600, "independent"),
        ContextBlock::new(
            "current",
            BlockKind::CurrentInput,
            true,
            1000,
            vec![json!({"role":"user","content":"complete synthetic question"})],
        ),
    ];
    let tx = db.transaction().unwrap();
    deduplicate_lineage_in(&tx, &mut blocks).unwrap();
    let compiled = context_budget::compile_in(
        &tx,
        "context-run",
        &context_budget::BudgetPolicy::defaults("synthetic-model"),
        1024,
        &blocks,
        1,
    )
    .unwrap();
    assert!(compiled.accepted);
    assert!(compiled.selected_block_ids.contains(&"day".to_string()));
    assert!(compiled
        .selected_block_ids
        .contains(&"independent".to_string()));
    for duplicate in ["week", "month"] {
        let decision = compiled
            .snapshot
            .blocks
            .iter()
            .find(|b| b.id == duplicate)
            .unwrap();
        assert!(!decision.selected);
        assert_eq!(decision.reason, "duplicate_source");
    }
    let mut events = std::collections::BTreeSet::new();
    for selected in compiled
        .snapshot
        .blocks
        .iter()
        .filter(|b| b.selected && b.kind == BlockKind::Diary)
    {
        let mut query = tx
            .prepare("SELECT event_id FROM diary_entry_sources WHERE entry_id=?1")
            .unwrap();
        for event in query
            .query_map([&selected.source_refs[0].id], |r| r.get::<_, String>(0))
            .unwrap()
        {
            assert!(
                events.insert(event.unwrap()),
                "a selected event must not appear twice through derived periods"
            );
        }
    }
}

#[test]
fn compatibility_memory_and_its_derived_diary_cannot_both_repeat_the_same_event() {
    let mut db = fixture();
    db.execute_batch("INSERT INTO diary_entry_sources VALUES('day','legacy:memory-one',1);")
        .unwrap();
    let mut blocks = vec![
        ContextBlock::new(
            "host",
            BlockKind::HostPolicy,
            true,
            1000,
            vec![json!({"role":"system","content":"synthetic host"})],
        ),
        block("memory", BlockKind::Memory, 800, "memory-one"),
        block("day", BlockKind::Diary, 700, "day"),
        ContextBlock::new(
            "current",
            BlockKind::CurrentInput,
            true,
            1000,
            vec![json!({"role":"user","content":"synthetic question"})],
        ),
    ];
    let tx = db.transaction().unwrap();
    deduplicate_lineage_in(&tx, &mut blocks).unwrap();
    let compiled = context_budget::compile_in(
        &tx,
        "context-run",
        &context_budget::BudgetPolicy::defaults("synthetic-model"),
        1024,
        &blocks,
        1,
    )
    .unwrap();
    assert!(compiled.accepted);
    assert!(compiled.selected_block_ids.contains(&"memory".to_string()));
    assert!(!compiled.selected_block_ids.contains(&"day".to_string()));
}

#[test]
fn multiple_hobbies_and_differently_dated_ages_are_not_conflicting() {
    let data = json!({"people":[{"person_id":"person-a","assertions":[
        {"field":"hobby","value":"阅读"},{"field":"hobby","value":"游泳"},
        {"field":"name","value":"甲"},{"field":"name","value":"另一个称呼"},
        {"field":"age","value":{"years":20,"stated_at":"2025-01-01"}},
        {"field":"age","value":{"years":21,"stated_at":"2026-01-01"}}
    ]}]});
    assert!(!relevant_person_conflict("他有什么爱好", &data));
    assert!(!relevant_person_conflict("他的年龄和姓名", &data));
}

#[test]
fn same_date_age_conflict_is_preserved_per_person_and_only_when_relevant() {
    let data = json!({"people":[{"person_id":"person-a","assertions":[
        {"field":"age","value":{"years":20,"stated_at":"2026-01-01"}},
        {"field":"age","value":{"years":23,"stated_at":"2026-01-01"}}
    ]}]});
    assert!(relevant_person_conflict("他的年龄是多少", &data));
    assert!(!relevant_person_conflict("他喜欢什么音乐", &data));
    let separate = json!({"people":[
        {"person_id":"person-a","assertions":[{"field":"age","value":{"years":20,"stated_at":"2026-01-01"}}]},
        {"person_id":"person-b","assertions":[{"field":"age","value":{"years":23,"stated_at":"2026-01-01"}}]}
    ]});
    assert!(!relevant_person_conflict("两人的年龄", &separate));
}

#[test]
fn birthday_conflicts_require_matching_calendar_and_precision() {
    let birthday = |month, day, calendar, precision| json!({"field":"birthday","value":{"month":month,"day":day,"calendar":calendar,"precision":precision}});
    let data = |a: Value, b: Value| json!({"people":[{"person_id":"person-a","assertions":[a,b]}]});
    assert!(relevant_person_conflict(
        "她的生日",
        &data(
            birthday(5, 1, "gregorian", "month_day"),
            birthday(5, 2, "gregorian", "month_day")
        )
    ));
    assert!(!relevant_person_conflict(
        "她的生日",
        &data(
            birthday(5, 1, "gregorian", "month_day"),
            birthday(5, 2, "lunar", "month_day")
        )
    ));
    assert!(!relevant_person_conflict(
        "她的生日",
        &data(
            birthday(5, 1, "gregorian", "month_day"),
            json!({"field":"birthday","value":{"year":2000,"calendar":"gregorian","precision":"year"}})
        )
    ));
}

#[test]
fn matching_age_and_birthday_facts_do_not_conflict_just_because_notes_differ() {
    let data = json!({"people":[{"person_id":"person-a","assertions":[
        {"field":"age","value":{"years":23,"stated_at":"2026-01-01","note":"合成来源甲的附注"}},
        {"field":"age","value":{"years":23,"stated_at":"2026-01-01","note":"合成来源乙的附注"}},
        {"field":"birthday","value":{"calendar":"gregorian","precision":"month_day","month":5,"day":2,"note":"合成备注甲"}},
        {"field":"birthday","value":{"calendar":"gregorian","precision":"month_day","month":5,"day":2,"note":"合成备注乙"}}
    ]}]});
    assert!(!relevant_person_conflict("年龄是多少", &data));
    assert!(!relevant_person_conflict("生日是什么时候", &data));
}

#[test]
fn unavailable_conflicting_and_partial_recall_states_are_not_reported_as_absence() {
    for (signals, expected) in [
        (
            RecallSignals {
                unavailable: true,
                ..Default::default()
            },
            "unavailable",
        ),
        (
            RecallSignals {
                conflicting: true,
                lexical: true,
                ..Default::default()
            },
            "conflicting",
        ),
        (
            RecallSignals {
                incomplete: true,
                lexical: true,
                ..Default::default()
            },
            "incomplete",
        ),
        (
            RecallSignals {
                expired: true,
                ..Default::default()
            },
            "expired",
        ),
        (
            RecallSignals {
                ambiguous: true,
                ..Default::default()
            },
            "ambiguous",
        ),
    ] {
        let decision = recall_decision(&signals);
        assert_eq!(decision["status"], expected);
        assert_eq!(decision["fallback_is_question_match"], false);
        assert_ne!(decision["status"], "not_found");
    }
}
