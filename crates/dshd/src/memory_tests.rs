use super::*;
use crate::{
    api,
    state::{token_hash, AppState, Options},
};
use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

fn database() -> Connection {
    let mut db = Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../schema.sql")).unwrap();
    initialize(&mut db).unwrap();
    db
}

fn conversation(db: &Connection, id: &str, owner: &str) {
    db.execute(
        "INSERT INTO conversations(id,owner_id,title,created_at) VALUES(?1,?2,'fixture',?3)",
        params![id, owner, now()],
    )
    .unwrap();
}

fn private(content: &str, conversation: &str) -> CreateMemory {
    CreateMemory {
        content: content.into(),
        visibility: Visibility::Private,
        source_conversation_id: Some(conversation.into()),
        source_message_id: None,
        publish: false,
    }
}

fn shared(content: &str) -> CreateMemory {
    CreateMemory {
        content: content.into(),
        visibility: Visibility::Shared,
        source_conversation_id: None,
        source_message_id: None,
        publish: true,
    }
}

fn change(content: &str) -> UpdateMemory {
    UpdateMemory {
        content: content.into(),
        visibility: None,
        publish: false,
    }
}

fn query(q: &str) -> ListOptions {
    ListOptions {
        q: Some(q.into()),
        ..Default::default()
    }
}

#[test]
fn assistant_identity_and_private_domains_survive_reopen_and_new_conversations() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("memory.sqlite");
    let (assistant, remembered) = {
        let mut db = Connection::open(&path).unwrap();
        db.execute_batch(include_str!("../schema.sql")).unwrap();
        initialize(&mut db).unwrap();
        conversation(&db, "old-chat", "qq:account:c2c:alice");
        create(&mut db, "admin", &shared("助手喜欢用清晰的中文表达")).unwrap();
        let record = create(&mut db, "admin", &private("蓝色鲸鱼是私有偏好", "old-chat")).unwrap();
        (identity(&db).unwrap(), record.id)
    };
    let mut db = Connection::open(&path).unwrap();
    initialize(&mut db).unwrap();
    conversation(&db, "new-chat-after-persona-change", "qq:account:c2c:alice");
    conversation(&db, "group", "qq:account:group:team");
    assert_eq!(identity(&db).unwrap(), assistant);
    assert_eq!(
        visible_list(&db, "qq:account:c2c:alice", &Default::default())
            .unwrap()
            .total,
        2
    );
    assert_eq!(
        visible_list(&db, "qq:account:group:team", &Default::default())
            .unwrap()
            .total,
        1
    );
    assert!(get_visible(&db, "qq:account:group:team", &remembered).is_err());
    assert_eq!(
        retrieve(&db, "qq:account:c2c:alice", "蓝色鲸鱼", 10, 100)
            .unwrap()
            .len(),
        1
    );
    assert!(retrieve(&db, "qq:account:c2c:alice", "蓝色鲸鱼", 10, 1)
        .unwrap()
        .is_empty());
}

#[test]
fn chinese_fts_and_short_search_apply_acl_before_pagination() {
    let mut db = database();
    conversation(&db, "alice-chat", "alice");
    conversation(&db, "bob-chat", "bob");
    let alice = create(
        &mut db,
        "admin",
        &private("苹果派是蓝色鲸鱼的最爱", "alice-chat"),
    )
    .unwrap();
    for _ in 0..3 {
        create(
            &mut db,
            "admin",
            &private("苹果派是蓝色鲸鱼的隐藏资料", "bob-chat"),
        )
        .unwrap();
    }
    let shared = create(&mut db, "admin", &shared("蓝色鲸鱼共享说明")).unwrap();
    for term in ["苹果派", "苹果", "蓝色鲸鱼", "鲸"] {
        let page = visible_list(&db, "alice", &query(term)).unwrap();
        assert!(page
            .items
            .iter()
            .all(|v| v.id == alice.id || v.id == shared.id));
        assert!(page.total >= 1);
    }
    let mut options = query("苹果派");
    options.limit = 1;
    let result = visible_list(&db, "alice", &options).unwrap();
    assert_eq!(result.total, 1);
    assert_eq!(result.items[0].id, alice.id);
    for literal in [
        "\" OR content:*",
        "' UNION SELECT secret",
        "蓝色鲸鱼\" OR \"隐藏资料",
        "owner_token:*",
    ] {
        assert_eq!(
            visible_list(&db, "alice", &query(literal)).unwrap().total,
            0
        );
    }
    assert_eq!(admin_list(&db, &query("苹果派")).unwrap().total, 4);
    let mut filtered = query("蓝色鲸鱼");
    filtered.visibility = Some(Visibility::Private);
    filtered.source_conversation_id = Some("bob-chat".into());
    assert_eq!(visible_list(&db, "alice", &filtered).unwrap().total, 0);
}

#[test]
fn source_validation_content_bounds_and_publication_require_explicit_authority() {
    let mut db = database();
    conversation(&db, "alice-chat", "alice");
    conversation(&db, "bob-chat", "bob");
    db.execute("INSERT INTO messages(id,conversation_id,role,content,created_at) VALUES('source','bob-chat','user','source fixture',?1)", [now()]).unwrap();
    let mut input = private("需要记住的事实", "alice-chat");
    input.source_message_id = Some("source".into());
    assert_eq!(
        create(&mut db, "admin", &input).unwrap_err().code,
        "invalid_memory_source"
    );
    input.source_message_id = None;
    input.content = "鲸".repeat(1334);
    assert_eq!(
        create(&mut db, "admin", &input).unwrap_err().code,
        "invalid_memory_content"
    );
    input.content = "x".repeat(MAX_CONTENT_BYTES);
    assert!(create(&mut db, "admin", &input).is_ok());
    let mut not_confirmed = shared("未经确认的共享内容");
    not_confirmed.publish = false;
    assert_eq!(
        create(&mut db, "admin", &not_confirmed).unwrap_err().code,
        "publish_confirmation_required"
    );
    let tx = db.transaction().unwrap();
    assert_eq!(
        create_in(
            &tx,
            Access::Owner("bob"),
            &private("跨主体创建", "alice-chat")
        )
        .unwrap_err()
        .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        create_in(&tx, Access::Owner("alice"), &shared("自行公开"))
            .unwrap_err()
            .status,
        StatusCode::FORBIDDEN
    );
    let own = create_in(
        &tx,
        Access::Owner("alice"),
        &private("自己的私有事实", "alice-chat"),
    )
    .unwrap();
    assert_eq!(own.source_owner_id.as_deref(), Some("alice"));
    tx.commit().unwrap();
}

#[test]
fn corrections_preserve_versions_and_conflicts_or_acl_do_not_mutate() {
    let mut db = database();
    conversation(&db, "alice-chat", "alice");
    let item = create(&mut db, "admin", &private("旧的蓝色偏好", "alice-chat")).unwrap();
    let updated = update(&mut db, "admin", &item.id, 1, &change("新的绿色偏好")).unwrap();
    assert_eq!(updated.revision, 2);
    let conflict = update(&mut db, "admin", &item.id, 1, &change("覆盖竞争更新")).unwrap_err();
    assert_eq!(conflict.code, "memory_revision_conflict");
    assert_eq!(
        forget(&mut db, "admin", &item.id, 1).unwrap_err().status,
        StatusCode::CONFLICT
    );
    let versions: i64 = db
        .query_row(
            "SELECT count(*) FROM memory_versions WHERE memory_id=?1 AND content IS NOT NULL",
            [&item.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(versions, 2);
    let tx = db.transaction().unwrap();
    assert_eq!(
        update_in(&tx, Access::Owner("bob"), &item.id, 2, &change("越权"))
            .unwrap_err()
            .status,
        StatusCode::NOT_FOUND
    );
    let publish = UpdateMemory {
        content: updated.content.clone(),
        visibility: Some(Visibility::Shared),
        publish: true,
    };
    assert_eq!(
        update_in(&tx, Access::Owner("alice"), &item.id, 2, &publish)
            .unwrap_err()
            .status,
        StatusCode::FORBIDDEN
    );
    tx.commit().unwrap();
    let publish = UpdateMemory {
        content: updated.content,
        visibility: Some(Visibility::Shared),
        publish: true,
    };
    let shared_item = update(&mut db, "admin", &item.id, 2, &publish).unwrap();
    assert_eq!(
        get_visible(&db, "bob", &item.id).unwrap().visibility,
        Visibility::Shared
    );
    assert_eq!(
        visible_list(&db, "bob", &query("绿色偏好")).unwrap().total,
        1
    );
    let tx = db.transaction().unwrap();
    assert_eq!(
        forget_in(&tx, Access::Owner("alice"), &item.id, 3)
            .unwrap_err()
            .status,
        StatusCode::NOT_FOUND
    );
    tx.commit().unwrap();
    assert_eq!(shared_item.revision, 3);
    let approved: i64 = db.query_row("SELECT count(*) FROM audit_events WHERE action='memory.visibility.approve' AND resource=?1",[&item.id],|r|r.get(0)).unwrap();
    assert_eq!(approved, 1);
    assert_eq!(admin_list(&db, &query("旧的蓝色")).unwrap().total, 0);
}

#[test]
fn forgetting_atomically_clears_every_memory_body_and_retains_bodyless_tombstone() {
    let mut db = database();
    conversation(&db, "chat", "alice");
    let item = create(&mut db, "admin", &private("原始秘密鲸鱼资料", "chat")).unwrap();
    update(&mut db, "admin", &item.id, 1, &change("修正秘密鲸鱼资料")).unwrap();
    let forgotten = forget(&mut db, "admin", &item.id, 2).unwrap();
    assert_eq!(forgotten.revision, 3);
    assert_eq!(forgotten.deletion_sequence, 1);
    assert_eq!(forgotten.cleanup.conversation_history, "retained");
    assert_eq!(
        get(&db, &item.id).unwrap_err().status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        visible_list(&db, "alice", &query("秘密鲸鱼"))
            .unwrap()
            .total,
        0
    );
    assert!(export(&db).unwrap()["items"].as_array().unwrap().is_empty());
    let counts: (i64, i64, i64) = db.query_row(
        "SELECT (SELECT count(*) FROM memory_records WHERE content IS NOT NULL),(SELECT count(*) FROM memory_versions WHERE content IS NOT NULL),(SELECT count(*) FROM memory_fts)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).unwrap();
    assert_eq!(counts, (0, 0, 0));
    let (versions, kind): (i64, String) = db
        .query_row(
            "SELECT count(*),max(change_kind) FROM memory_versions WHERE memory_id=?1",
            [&item.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(versions, 3);
    assert_eq!(kind, "forgotten");
    assert_eq!(
        forget(&mut db, "admin", &item.id, 3).unwrap_err().status,
        StatusCode::NOT_FOUND
    );
}

#[test]
fn audit_failure_rolls_back_forgetting_fts_and_deletion_watermark() {
    let mut db = database();
    let item = create(&mut db, "admin", &shared("不可半途遗忘的事务资料")).unwrap();
    db.execute_batch("CREATE TRIGGER fixture_fail_audit BEFORE INSERT ON audit_events WHEN new.action='memory.forget' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert_eq!(
        forget(&mut db, "admin", &item.id, 1).unwrap_err().status,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(get(&db, &item.id).unwrap().revision, 1);
    assert_eq!(
        visible_list(&db, "alice", &query("事务资料"))
            .unwrap()
            .total,
        1
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM memory_deletion_watermarks", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    // A command can still commit its error reply without committing half of a
    // failed mutation; the memory service owns an inner savepoint.
    let tx = db.transaction().unwrap();
    assert!(forget_in(&tx, Access::Admin("admin"), &item.id, 1).is_err());
    tx.execute("INSERT INTO audit_events(actor,action,resource,created_at) VALUES('admin','fixture.error_reply','fixture',?1)",[now()]).unwrap();
    tx.commit().unwrap();
    assert_eq!(get(&db, &item.id).unwrap().revision, 1);
    assert_eq!(
        visible_list(&db, "alice", &query("事务资料"))
            .unwrap()
            .total,
        1
    );
}

#[test]
fn future_memory_schema_fails_closed_and_fts_never_crosses_assistant_identity() {
    let mut db = database();
    let old = create(&mut db, "admin", &shared("旧助手专有记忆")).unwrap();
    let next = uuid::Uuid::new_v4().to_string();
    db.execute(
        "UPDATE settings SET value=?1 WHERE key='assistant.identity.v1'",
        [&next],
    )
    .unwrap();
    let page = visible_list(&db, "alice", &query("专有记忆")).unwrap();
    assert_eq!(page.assistant_id, next);
    assert_eq!(page.total, 0);
    assert!(get(&db, &old.id).is_err());
    assert!(create_in(&db, Access::Admin("admin"), &shared("没有外层事务")).is_err());
    db.execute(
        "INSERT INTO memory_schema_migrations(version,applied_at) VALUES(2,?1)",
        [now()],
    )
    .unwrap();
    assert!(initialize(&mut db).is_err());
    assert_eq!(identity(&db).unwrap(), next);
    assert_eq!(
        db.query_row("SELECT count(*) FROM memory_records", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn private_quota_and_page_bounds_are_enforced_and_forgetting_frees_capacity() {
    let mut db = database();
    conversation(&db, "chat", "alice");
    let first = {
        let tx = db.transaction().unwrap();
        let mut first = String::new();
        for index in 0..MAX_PRIVATE_RECORDS {
            let item = create_in(
                &tx,
                Access::Owner("alice"),
                &private(&format!("有界记忆条目 {index}"), "chat"),
            )
            .unwrap();
            if index == 0 {
                first = item.id;
            }
        }
        tx.commit().unwrap();
        first
    };
    assert_eq!(
        create(&mut db, "admin", &private("超限", "chat"))
            .unwrap_err()
            .code,
        "memory_limit"
    );
    assert_eq!(
        admin_list(
            &db,
            &ListOptions {
                limit: 101,
                ..Default::default()
            }
        )
        .unwrap_err()
        .code,
        "invalid_pagination"
    );
    let page = admin_list(
        &db,
        &ListOptions {
            limit: 100,
            offset: 499,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(page.total, 500);
    assert_eq!(page.items.len(), 1);
    forget(&mut db, "admin", &first, 1).unwrap();
    assert!(create(&mut db, "admin", &private("空位恢复", "chat")).is_ok());
}

const ORIGIN: &str = "http://127.0.0.1:18419";

#[test]
fn empty_appstate_retains_reader_one_until_a_memory_or_deletion_watermark_is_used() {
    let temp = TempDir::new().unwrap();
    let options = Options {
        data_dir: temp.path().to_owned(),
        origin: ORIGIN.into(),
        allow_private_model: true,
        cookie_secure: false,
        web_dir: temp.path().join("web"),
    };
    let state = AppState::open(options.clone()).unwrap();
    let required = |state: &AppState| {
        state
            .db(|db| {
                Ok(
                    db.query_row("SELECT max(version) FROM dsh_schema_migrations", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                )
            })
            .unwrap()
    };
    assert_eq!(
        required(&state),
        1,
        "empty migration keeps the character-only rollback path"
    );
    let assistant = state.db(|db| Ok(identity(db))).unwrap().unwrap();
    drop(state);
    let state = AppState::open(options.clone()).unwrap();
    assert_eq!(required(&state), 1);
    assert_eq!(state.db(|db| Ok(identity(db))).unwrap().unwrap(), assistant);
    let item = state
        .db(|db| Ok(create(db, "admin", &shared("首次持久记忆"))))
        .unwrap()
        .unwrap();
    assert_eq!(required(&state), 2);
    state
        .db(|db| Ok(forget(db, "admin", &item.id, 1)))
        .unwrap()
        .unwrap();
    assert_eq!(
        required(&state),
        2,
        "forgetting does not lower the durable reader requirement"
    );
    drop(state);
    let state = AppState::open(options).unwrap();
    assert_eq!(required(&state), 2);
    assert_eq!(
        state
            .db(|db| Ok(admin_list(db, &Default::default())))
            .unwrap()
            .unwrap()
            .total,
        0
    );
}

#[test]
fn initialize_recovers_reader_requirement_from_tombstone_or_context_watermark() {
    let mut db = database();
    db.execute_batch("CREATE TABLE dsh_schema_migrations(version INTEGER PRIMARY KEY,created_at INTEGER NOT NULL); INSERT INTO dsh_schema_migrations VALUES(1,0);").unwrap();
    initialize(&mut db).unwrap();
    let item = create(&mut db, "admin", &shared("留下无正文墓碑")).unwrap();
    forget(&mut db, "admin", &item.id, 1).unwrap();
    db.execute("DELETE FROM dsh_schema_migrations WHERE version=2", [])
        .unwrap();
    initialize(&mut db).unwrap();
    assert_eq!(
        db.query_row("SELECT max(version) FROM dsh_schema_migrations", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        2
    );
    let mut empty = database();
    empty.execute_batch("CREATE TABLE dsh_schema_migrations(version INTEGER PRIMARY KEY,created_at INTEGER NOT NULL); INSERT INTO dsh_schema_migrations VALUES(1,0);").unwrap();
    conversation(&empty, "watermarked", "alice");
    let assistant = identity(&empty).unwrap();
    empty.execute("INSERT INTO memory_context_watermarks(conversation_id,assistant_id,through_run_rowid,updated_at) VALUES('watermarked',?1,0,?2)", params![assistant,now()]).unwrap();
    initialize(&mut empty).unwrap();
    assert_eq!(
        empty
            .query_row("SELECT max(version) FROM dsh_schema_migrations", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
        2
    );
}

async fn request(
    state: &AppState,
    method: &str,
    path: &str,
    body: Value,
    authenticated: bool,
    csrf: bool,
    revision: Option<&str>,
) -> (StatusCode, Value, Option<String>) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("Host", "127.0.0.1:18419")
        .header("Origin", ORIGIN)
        .header("Content-Type", "application/json");
    if authenticated {
        builder = builder.header("Cookie", format!("dsh_session={}", "a".repeat(64)));
    }
    if csrf {
        builder = builder.header("X-CSRF-Token", "memory-csrf");
    }
    if let Some(revision) = revision {
        builder = builder.header("If-Match", revision);
    }
    let response = api::router(state.clone())
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let etag = response
        .headers()
        .get("etag")
        .map(|v| v.to_str().unwrap().to_string());
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let value = serde_json::from_slice(&body)
        .unwrap_or_else(|_| json!({"text":String::from_utf8_lossy(&body)}));
    (status, value, etag)
}

#[tokio::test]
async fn admin_api_requires_authentication_csrf_explicit_publish_and_revision() {
    let temp = TempDir::new().unwrap();
    let state = AppState::open(Options {
        data_dir: temp.path().to_owned(),
        origin: ORIGIN.into(),
        allow_private_model: true,
        cookie_secure: false,
        web_dir: temp.path().join("web"),
    })
    .unwrap();
    state.db(|db| {
        db.execute("INSERT INTO admins(id,username,password_hash,created_at) VALUES('admin','admin','unused-fixture',?1)",[now()])?;
        db.execute("INSERT INTO sessions(token_hash,user_id,csrf,expires_at) VALUES(?1,'admin','memory-csrf',?2)",params![token_hash(&"a".repeat(64)),now()+3600])?;
        Ok(())
    }).unwrap();
    for path in ["/api/admin/v1/memory", "/api/admin/v1/memory/export"] {
        assert_eq!(
            request(&state, "GET", path, Value::Null, false, false, None)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
    let body = json!({"content":"管理员共享记忆","visibility":"shared","publish":true});
    assert_eq!(
        request(
            &state,
            "POST",
            "/api/admin/v1/memory",
            body.clone(),
            true,
            false,
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &state,
            "POST",
            "/api/admin/v1/memory",
            json!({"content":"未确认","visibility":"shared"}),
            true,
            true,
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (status, item, etag) = request(
        &state,
        "POST",
        "/api/admin/v1/memory",
        body,
        true,
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{item}");
    assert_eq!(etag.as_deref(), Some("\"1\""));
    let path = format!("/api/admin/v1/memory/{}", item["id"].as_str().unwrap());
    assert_eq!(
        request(
            &state,
            "PATCH",
            &path,
            json!({"content":"修正"}),
            true,
            true,
            None
        )
        .await
        .0,
        StatusCode::PRECONDITION_REQUIRED
    );
    assert_eq!(
        request(
            &state,
            "PATCH",
            &path,
            json!({"content":"修正"}),
            true,
            true,
            Some("W/\"1\"")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &state,
            "PATCH",
            &path,
            json!({"content":"修正"}),
            true,
            true,
            Some("\"1\"")
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, error, _) = request(
        &state,
        "DELETE",
        &path,
        Value::Null,
        true,
        true,
        Some("\"1\""),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "memory_revision_conflict");
    assert_eq!(
        request(
            &state,
            "DELETE",
            &path,
            Value::Null,
            true,
            true,
            Some("\"2\"")
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&state, "GET", &path, Value::Null, true, false, None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (_, export, _) = request(
        &state,
        "GET",
        "/api/admin/v1/memory/export",
        Value::Null,
        true,
        false,
        None,
    )
    .await;
    assert_eq!(export["schema"], "dsh.assistant-memory.v1");
    assert_eq!(export["scope"]["kind"], "assistant");
    assert!(export["items"].as_array().unwrap().is_empty());
}
