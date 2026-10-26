use super::*;

fn db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT);INSERT INTO settings VALUES('assistant.identity.v1','stable');CREATE TABLE audit_events(actor TEXT,action TEXT,resource TEXT,created_at INTEGER);").unwrap();
    initialize(&db).unwrap();
    db
}
fn at(s: &str) -> i64 {
    DateTime::parse_from_rfc3339(s).unwrap().timestamp()
}
fn activity(start: i64) -> Value {
    json!({"title":"private reading","type":"reading","purpose":"source-grounded notes","start_at":start,"end_at":start+1800,"time_zone":"Asia/Shanghai","resource_id":null,"target_ref":null,"repeat":null,"movable":true,"grant_id":null})
}
fn resource(db: &Connection, owner: &str, text: &str, now: i64) -> String {
    create_resource(
        db,
        owner,
        &json!({"title":"authorized document","type":"text","content":text}),
        now,
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .into()
}
fn grant(db: &Connection, owner: &str, id: &str, now: i64) -> String {
    create_grant(db,owner,&json!({"title":"reading authorization","allowed_types":["reading"],"resource_ids":[id],"time_zone":"Asia/Shanghai","valid_from":now-60,"valid_until":now+86400*30,"horizon_days":14,"window_start":"09:00","window_end":"18:00","max_daily_activities":2,"max_daily_minutes":60,"max_daily_changes":5}),now).unwrap()["id"].as_str().unwrap().into()
}
#[test]
fn owner_acl_idempotency_conflict_and_input_validation() {
    let db = db();
    let now = at("2026-10-26T09:00:00+08:00");
    let id = resource(&db, "a", "private marker", now);
    let mut a = activity(now + 60);
    a["resource_id"] = json!(id);
    let first = create(&db, "a", &a, "one", false, now).unwrap();
    assert_eq!(create(&db, "a", &a, "one", false, now).unwrap(), first);
    assert!(create(&db, "b", &a, "two", false, now).is_err());
    assert!(detail(&db, "b", first["id"].as_str().unwrap()).is_err());
    assert!(calendar(
        &db,
        "b",
        now,
        now + 86400,
        "Asia/Shanghai",
        None,
        None,
        None,
        0,
        100
    )
    .unwrap()["items"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(create(&db, "a", &a, "three", false, now).is_err());
    a["title"] = json!("changed");
    assert!(create(&db, "a", &a, "one", false, now).is_err());
    assert!(calendar(
        &db,
        "a",
        now,
        now + 94 * 86400,
        "Asia/Shanghai",
        None,
        None,
        None,
        0,
        100
    )
    .is_err());
}
#[test]
fn autonomous_ranges_budgets_and_revocation_are_enforced() {
    let db = db();
    let now = at("2026-10-26T09:00:00+08:00");
    let id = resource(&db, "a", "data", now);
    let gid = grant(&db, "a", &id, now);
    let mut a = activity(now + 60);
    a["resource_id"] = json!(id);
    a["grant_id"] = json!(gid);
    assert_eq!(
        create(&db, "a", &a, "a", true, now).unwrap()["state"],
        "enabled"
    );
    a["start_at"] = json!(now + 3600);
    a["end_at"] = json!(now + 5400);
    assert_eq!(
        create(&db, "a", &a, "b", true, now).unwrap()["state"],
        "enabled"
    );
    a["start_at"] = json!(now + 7200);
    a["end_at"] = json!(now + 9000);
    assert_eq!(
        create(&db, "a", &a, "c", true, now).unwrap()["state"],
        "pending_approval"
    );
    change_grant(&db, "a", &gid, 1, None, now + 1).unwrap();
    tick_in(&db, now + 61, false).unwrap();
    assert!(prepare_read_chunk(&db, now + 61).unwrap().is_none());
}
#[test]
fn repeated_calendar_preserves_wall_time_and_skips_invalid_dst() {
    let a:Activity=serde_json::from_value(json!({"title":"study","type":"consolidation","purpose":"test","start_at":at("2026-03-07T02:30:00-05:00"),"end_at":at("2026-03-07T03:00:00-05:00"),"time_zone":"America/New_York","repeat":{"frequency":"daily","interval":1,"until":null,"count":3},"movable":false,"resource_id":null,"target_ref":null,"grant_id":null})).unwrap();
    validate(&a).unwrap();
    let occurrences = expand(&a, a.start_at, a.start_at + 4 * 86400, 100).unwrap();
    assert_eq!(occurrences.len(), 2);
    assert_eq!(
        instant(occurrences[1].0)
            .unwrap()
            .with_timezone(&tz(&a.time_zone).unwrap())
            .hour(),
        2
    );
    let mut monthly = a.clone();
    monthly.time_zone = "Asia/Shanghai".into();
    monthly.start_at = at("2026-01-31T09:00:00+08:00");
    monthly.end_at = monthly.start_at + 1800;
    monthly.repeat = Some(Repeat {
        frequency: "monthly".into(),
        interval: 1,
        until: None,
        count: Some(3),
    });
    assert_eq!(
        expand(
            &monthly,
            monthly.start_at,
            monthly.start_at + 100 * 86400,
            100
        )
        .unwrap()
        .len(),
        2
    );
}
#[test]
fn reading_only_advances_after_fenced_model_note_and_resumes() {
    let db = db();
    let now = at("2026-10-26T09:00:00+08:00");
    let text = "文".repeat(5000);
    let rid = resource(&db, "a", &text, now);
    let mut a = activity(now);
    a["resource_id"] = json!(rid);
    let act = create(&db, "a", &a, "reading", false, now).unwrap();
    tick_in(&db, now, false).unwrap();
    let chunk = prepare_read_chunk(&db, now).unwrap().unwrap();
    assert_eq!(chunk.start_char, 0);
    assert_eq!(chunk.end_char, 4096);
    assert!(prepare_read_chunk(&db, now).unwrap().is_none());
    assert_eq!(
        detail(&db, "a", act["id"].as_str().unwrap()).unwrap()["occurrences"][0]["processed_chars"],
        0
    );
    commit_read_note(
        &db,
        &chunk,
        "真实读取片段后的笔记",
        "test-provider",
        now + 1,
    )
    .unwrap();
    assert!(commit_read_note(&db, &chunk, "stale", "test-provider", now + 1).is_err());
    initialize(&db).unwrap();
    let next = prepare_read_chunk(&db, now + 2).unwrap().unwrap();
    assert_eq!(next.start_char, 4096);
    commit_read_note(&db, &next, "第二段笔记", "test-provider", now + 3).unwrap();
    let detail = detail(&db, "a", act["id"].as_str().unwrap()).unwrap();
    assert_eq!(detail["occurrences"][0]["status"], "completed");
    assert_eq!(detail["occurrences"][0]["processed_chars"], 5000);
    assert_eq!(
        detail["occurrences"][0]["result"]["fragments"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
#[test]
fn cancellation_and_resource_revocation_reject_late_read_results() {
    let db = db();
    let now = at("2026-10-26T09:00:00+08:00");
    let rid = resource(&db, "a", "text", now);
    let mut a = activity(now);
    a["resource_id"] = json!(rid);
    let act = create(&db, "a", &a, "reading", false, now).unwrap();
    let id = act["id"].as_str().unwrap();
    tick_in(&db, now, false).unwrap();
    let c = prepare_read_chunk(&db, now).unwrap().unwrap();
    change(
        &db,
        "a",
        id,
        1,
        "series",
        None,
        None,
        Some("cancelled"),
        false,
        now + 1,
    )
    .unwrap();
    assert!(commit_read_note(&db, &c, "late note", "test", now + 2).is_err());
    revoke_resource(&db, "a", &rid, 1, now + 3).unwrap();
    let d = detail(&db, "a", id).unwrap();
    assert!(d["occurrences"][0]["result"].is_null());
}
#[test]
fn single_and_following_edits_keep_revisions_and_history() {
    let db = db();
    let now = at("2026-10-26T08:00:00+08:00");
    let rid = resource(&db, "a", "abc", now);
    let mut a = activity(now + 3600);
    a["resource_id"] = json!(rid);
    a["repeat"] = json!({"frequency":"daily","interval":1,"until":null,"count":5});
    let original = create(&db, "a", &a, "series", false, now).unwrap();
    let id = original["id"].as_str().unwrap();
    let mut changed = a.clone();
    changed["start_at"] = json!(now + 7200);
    changed["end_at"] = json!(now + 9000);
    changed["repeat"] = Value::Null;
    let saved = change(
        &db,
        "a",
        id,
        1,
        "single",
        Some(now + 3600),
        Some(&changed),
        None,
        false,
        now,
    )
    .unwrap();
    assert_eq!(saved["revision"], 2);
    assert_eq!(saved["exceptions"][0]["definition"]["start_at"], now + 7200);
    assert!(change(
        &db,
        "a",
        id,
        1,
        "series",
        None,
        None,
        Some("paused"),
        false,
        now
    )
    .is_err());
    change(
        &db,
        "a",
        id,
        2,
        "following",
        Some(now + 86400 + 3600),
        None,
        Some("paused"),
        false,
        now,
    )
    .unwrap();
    assert_eq!(
        list(&db, "a").unwrap()["items"].as_array().unwrap().len(),
        2
    );
    let future = list(&db, "a").unwrap()["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] != id)
        .unwrap()
        .clone();
    assert_eq!(future["definition"]["repeat"]["count"], 4);
}
#[test]
fn missing_video_and_contact_capability_cannot_complete() {
    let db = db();
    let now = at("2026-10-26T09:00:00+08:00");
    let mut a = activity(now);
    a["type"] = json!("video");
    let id = create(&db, "a", &a, "video", false, now).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    tick_in(&db, now, false).unwrap();
    let d = detail(&db, "a", &id).unwrap();
    assert_eq!(d["occurrences"][0]["status"], "blocked");
    assert_eq!(
        d["occurrences"][0]["block_reason"],
        "video_provider_unavailable"
    );
    assert!(prepare_read_chunk(&db, now).unwrap().is_none());
}
#[test]
fn bounded_planner_creates_one_authorized_activity_and_no_duplicate() {
    let db = db();
    let now = at("2026-10-26T09:00:00+08:00");
    let rid = resource(&db, "a", "abc", now);
    let gid = grant(&db, "a", &rid, now);
    create_plan(&db,"a",&json!({"goal":"学习授权资料","grant_id":gid,"resource_ids":[rid],"duration_minutes":20,"state":"enabled"}),now).unwrap();
    assert_eq!(tick_in(&db, now, false).unwrap()["planned"], 1);
    assert_eq!(tick_in(&db, now + 61, false).unwrap()["planned"], 0);
    assert_eq!(
        list(&db, "a").unwrap()["items"].as_array().unwrap().len(),
        1
    );
}
#[test]
fn moved_exception_crosses_range_and_executes_at_effective_time() {
    let db = db();
    let now = at("2026-10-26T08:00:00+08:00");
    let rid = resource(&db, "a", "abc", now);
    let mut a = activity(now + 3600);
    a["resource_id"] = json!(rid);
    a["repeat"] = json!({"frequency":"daily","interval":1,"until":null,"count":2});
    let original = create(&db, "a", &a, "series", false, now).unwrap();
    let id = original["id"].as_str().unwrap();
    let moved = at("2026-11-03T11:00:00+08:00");
    let mut x = a.clone();
    x["start_at"] = json!(moved);
    x["end_at"] = json!(moved + 1800);
    x["repeat"] = Value::Null;
    change(
        &db,
        "a",
        id,
        1,
        "single",
        Some(now + 3600),
        Some(&x),
        None,
        false,
        now,
    )
    .unwrap();
    let cal = calendar(
        &db,
        "a",
        moved - 3600,
        moved + 3600,
        "Asia/Shanghai",
        None,
        None,
        None,
        0,
        100,
    )
    .unwrap();
    assert_eq!(cal["items"].as_array().unwrap().len(), 1);
    assert_eq!(cal["items"][0]["start_at"], moved);
    tick_in(&db, moved, false).unwrap();
    let chunk = prepare_read_chunk(&db, moved).unwrap().unwrap();
    assert_eq!(chunk.resource_id, rid);
}
#[test]
fn autonomous_reschedule_cannot_release_already_reserved_day_budget() {
    let db = db();
    let now = at("2026-10-26T09:00:00+08:00");
    let rid = resource(&db, "a", "abc", now);
    let gid = grant(&db, "a", &rid, now);
    let mut a = activity(now + 60);
    a["resource_id"] = json!(rid);
    a["grant_id"] = json!(gid);
    let first = create(&db, "a", &a, "first", true, now).unwrap();
    let id = first["id"].as_str().unwrap();
    let mut moved = a.clone();
    moved["start_at"] = json!(now + 86400 + 60);
    moved["end_at"] = json!(now + 86400 + 1860);
    change(
        &db,
        "a",
        id,
        1,
        "series",
        None,
        Some(&moved),
        None,
        true,
        now,
    )
    .unwrap();
    a["start_at"] = json!(now + 3600);
    a["end_at"] = json!(now + 5400);
    assert_eq!(
        create(&db, "a", &a, "second", true, now).unwrap()["state"],
        "enabled"
    );
    a["start_at"] = json!(now + 7200);
    a["end_at"] = json!(now + 9000);
    assert_eq!(
        create(&db, "a", &a, "third", true, now).unwrap()["state"],
        "pending_approval"
    );
}
#[test]
fn new_occurrence_and_different_goal_continue_one_resource_cursor() {
    let db = db();
    let now = at("2026-10-26T09:00:00+08:00");
    let rid = resource(&db, "a", &"文".repeat(5000), now);
    let mut a = activity(now);
    a["resource_id"] = json!(rid);
    let first = create(&db, "a", &a, "first", false, now).unwrap();
    tick_in(&db, now, false).unwrap();
    let c = prepare_read_chunk(&db, now).unwrap().unwrap();
    commit_read_note(&db, &c, "first fragment", "test", now + 1).unwrap();
    tick_in(&db, now + 1801, false).unwrap();
    a["start_at"] = json!(now + 3600);
    a["end_at"] = json!(now + 5400);
    a["purpose"] = json!("a different goal, same authorized resource");
    let second = create(&db, "a", &a, "second", false, now + 1801).unwrap();
    tick_in(&db, now + 3600, false).unwrap();
    let c2 = prepare_read_chunk(&db, now + 3600).unwrap().unwrap();
    assert_eq!(c2.start_char, 4096);
    commit_read_note(&db, &c2, "continued fragment", "test", now + 3601).unwrap();
    let d = detail(&db, "a", second["id"].as_str().unwrap()).unwrap();
    assert_eq!(
        d["occurrences"][0]["result"]["fragments"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(d["occurrences"][0]["status"], "completed");
    assert_eq!(
        detail(&db, "a", first["id"].as_str().unwrap()).unwrap()["occurrences"][0]
            ["processed_chars"],
        4096
    );
    a["start_at"] = json!(now + 7200);
    a["end_at"] = json!(now + 9000);
    let done = create(&db, "a", &a, "already", false, now + 5401).unwrap();
    tick_in(&db, now + 7200, false).unwrap();
    assert!(prepare_read_chunk(&db, now + 7200).unwrap().is_none());
    assert_eq!(
        detail(&db, "a", done["id"].as_str().unwrap()).unwrap()["occurrences"][0]["block_reason"],
        "resource_already_read"
    );
}
#[test]
fn explicit_restart_creates_new_generation_and_revocation_clears_all_notes() {
    let db = db();
    let now = at("2026-10-26T09:00:00+08:00");
    let rid = resource(&db, "a", "first book", now);
    let mut a = activity(now);
    a["resource_id"] = json!(rid);
    create(&db, "a", &a, "first", false, now).unwrap();
    tick_in(&db, now, false).unwrap();
    let c = prepare_read_chunk(&db, now).unwrap().unwrap();
    commit_read_note(&db, &c, "original notes", "test", now + 1).unwrap();
    a["start_at"] = json!(now + 3600);
    a["end_at"] = json!(now + 5400);
    a["restart_reading"] = json!(true);
    create(&db, "a", &a, "restart", false, now + 1801).unwrap();
    tick_in(&db, now + 3600, false).unwrap();
    let restarted = prepare_read_chunk(&db, now + 3600).unwrap().unwrap();
    assert_eq!(restarted.start_char, 0);
    assert!(restarted.reading_generation > c.reading_generation);
    commit_read_note(&db, &restarted, "new pass notes", "test", now + 3601).unwrap();
    assert!(resource_progress(&db, "b", &rid).unwrap().is_none());
    revoke_resource(&db, "a", &rid, 1, now + 3602).unwrap();
    assert!(resource_progress(&db, "a", &rid).unwrap().is_none());
    let remaining: i64 = db
        .query_row(
            "SELECT count(*) FROM schedule_occurrences WHERE result_json IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0);
}
#[test]
fn explicit_admin_approval_preserves_ai_creator_and_binds_exact_definition() {
    let db = db();
    let now = at("2026-10-26T09:00:00+08:00");
    let rid = resource(&db, "a", "reading text", now);
    let mut a = activity(now + 60);
    a["resource_id"] = json!(rid);
    let pending = create(&db, "a", &a, "pending", true, now).unwrap();
    let id = pending["id"].as_str().unwrap();
    assert_eq!(pending["state"], "pending_approval");
    assert!(change(
        &db,
        "a",
        id,
        1,
        "series",
        None,
        None,
        Some("enabled"),
        false,
        now
    )
    .is_err());
    assert!(approve_admin_in(&db, "a", "a", id, 1, false, "series", now).is_err());
    assert!(approve_admin_in(&db, "a", "model", id, 1, true, "series", now).is_err());
    assert!(approve_admin_in(&db, "a", "a", id, 1, true, "single", now).is_err());
    assert!(approve_admin_in(&db, "a", "a", id, 2, true, "series", now).is_err());
    let approved = approve_admin_in(&db, "a", "a", id, 1, true, "series", now).unwrap();
    assert_eq!(approved["state"], "enabled");
    assert_eq!(approved["creator"], "autonomous");
    assert_eq!(approved["admin_approval"]["actor_id"], "a");
    assert_eq!(approved["admin_approval"]["approved_revision"], 2);
    assert_eq!(
        approved["admin_approval"]["definition_hash"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert!(approve_admin_in(&db, "a", "a", id, 2, true, "series", now).is_err());
    tick_in(&db, now + 60, false).unwrap();
    let chunk = prepare_read_chunk(&db, now + 60).unwrap().unwrap();
    commit_read_note(
        &db,
        &chunk,
        "actual processed source note",
        "test",
        now + 61,
    )
    .unwrap();
    a["title"] = json!("changed definition");
    let changed = change(
        &db,
        "a",
        id,
        2,
        "series",
        None,
        Some(&a),
        None,
        false,
        now + 62,
    )
    .unwrap();
    assert_eq!(changed["state"], "pending_approval");
    assert!(changed["admin_approval"].is_null());
    let mut forged = a.clone();
    forged["admin_approval"] = json!(true);
    assert!(create(&db, "a", &forged, "forged", true, now).is_err());
}
#[test]
fn approval_cannot_bypass_source_ownership_conflicts_or_missing_capability() {
    let db = db();
    let now = at("2026-10-26T09:00:00+08:00");
    let rid = resource(&db, "a", "text", now);
    let mut a = activity(now + 60);
    a["resource_id"] = json!(rid);
    let pending = create(&db, "a", &a, "pending", true, now).unwrap();
    let id = pending["id"].as_str().unwrap();
    create(&db, "a", &a, "manual-conflict", false, now).unwrap();
    assert!(approve_admin_in(&db, "a", "a", id, 1, true, "series", now).is_err());
    assert!(approve_admin_in(&db, "b", "b", id, 1, true, "series", now).is_err());
    revoke_resource(&db, "a", &rid, 1, now + 1).unwrap();
    assert!(approve_admin_in(&db, "a", "a", id, 1, true, "series", now + 2).is_err());
    let mut video = activity(now + 3600);
    video["type"] = json!("video");
    let pending_video = create(&db, "a", &video, "video-pending", true, now).unwrap();
    let video_id = pending_video["id"].as_str().unwrap();
    approve_admin_in(&db, "a", "a", video_id, 1, true, "series", now).unwrap();
    tick_in(&db, now + 3600, false).unwrap();
    assert_eq!(
        detail(&db, "a", video_id).unwrap()["occurrences"][0]["block_reason"],
        "video_provider_unavailable"
    );
    assert!(prepare_read_chunk(&db, now + 3600).unwrap().is_none());
}
#[test]
fn changing_reviewed_exception_revokes_entire_series_approval() {
    let db = db();
    let now = at("2026-10-26T09:00:00+08:00");
    let rid = resource(&db, "a", "text", now);
    let mut a = activity(now + 60);
    a["resource_id"] = json!(rid);
    a["repeat"] = json!({"frequency":"daily","interval":1,"until":null,"count":3});
    let pending = create(&db, "a", &a, "pending-series", true, now).unwrap();
    let id = pending["id"].as_str().unwrap();
    approve_admin_in(&db, "a", "a", id, 1, true, "series", now).unwrap();
    let mut moved = a.clone();
    moved["start_at"] = json!(now + 7200);
    moved["end_at"] = json!(now + 9000);
    moved["repeat"] = Value::Null;
    let changed = change(
        &db,
        "a",
        id,
        2,
        "single",
        Some(now + 60),
        Some(&moved),
        None,
        false,
        now,
    )
    .unwrap();
    assert_eq!(changed["state"], "pending_approval");
    assert!(changed["admin_approval"].is_null());
    assert!(!admin_approved_in(&db, "a", id).unwrap());
    let revoked:i64=db.query_row("SELECT count(*) FROM schedule_admin_approvals WHERE activity_id=?1 AND revoked_at IS NOT NULL",[id],|r|r.get(0)).unwrap();
    assert_eq!(revoked, 1);
    let again = approve_admin_in(&db, "a", "a", id, 3, true, "series", now).unwrap();
    assert_eq!(again["admin_approval"]["approved_revision"], 4);
    assert_eq!(again["creator"], "autonomous");
    let changed_again = change(
        &db,
        "a",
        id,
        4,
        "single",
        Some(now + 60),
        None,
        Some("cancelled"),
        false,
        now,
    )
    .unwrap();
    assert_eq!(changed_again["state"], "pending_approval");
    assert!(changed_again["admin_approval"].is_null());
}
