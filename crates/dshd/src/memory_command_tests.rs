use super::execute;
use crate::{
    character,
    config::RuntimeConfig,
    memory::{self, CreateMemory, ListOptions, Visibility},
    qq,
    state::{now, AppState, Options},
    worker,
};
use rusqlite::params;
use serde_json::{json, Value};
use tempfile::TempDir;

fn open() -> (TempDir, AppState) {
    let temporary = TempDir::new().unwrap();
    let state = AppState::open(Options {
        data_dir: temporary.path().to_owned(),
        origin: "http://127.0.0.1:18419".into(),
        allow_private_model: false,
        cookie_secure: false,
        web_dir: temporary.path().join("web"),
    })
    .unwrap();
    assert!(state
        .current_config()
        .unwrap()
        .1
        .model
        .credential_ref
        .is_none());
    (temporary, state)
}

fn conversation(state: &AppState, owner: &str) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    state
        .db(|db| {
            db.execute(
                "INSERT INTO conversations(id,owner_id,title,created_at) VALUES(?1,?2,'memory command fixture',?3)",
                params![id, owner, now()],
            )?;
            Ok(())
        })
        .unwrap();
    id
}

fn mark_running(state: &AppState, run: &str) {
    state
        .db(|db| {
            db.execute(
                "UPDATE runs SET status='running' WHERE id=?1 AND status='queued'",
                [run],
            )?;
            Ok(())
        })
        .unwrap();
}

fn command(state: &AppState, conversation: &str, owner: &str, text: &str) -> (String, String) {
    let run = worker::enqueue(
        state,
        conversation,
        owner,
        &uuid::Uuid::new_v4().to_string(),
        text,
        None,
    )
    .unwrap();
    mark_running(state, &run);
    assert!(execute(state, &run).unwrap());
    let response = state
        .db(|db| {
            Ok(
                db.query_row("SELECT response FROM runs WHERE id=?1", [&run], |r| {
                    r.get(0)
                })?,
            )
        })
        .unwrap();
    (run, response)
}

fn scalar(state: &AppState, sql: &str) -> i64 {
    state
        .db(|db| Ok(db.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
}

fn qq_config() -> RuntimeConfig {
    let mut config = RuntimeConfig::default();
    config.qq.app_id = "12345".into();
    config.qq.credential_ref = Some("unused-test-credential-reference".into());
    config
}

fn qq_event(event: &str, message: &str, user: &str, content: &str) -> Value {
    json!({"op":0,"id":event,"t":"C2C_MESSAGE_CREATE","d":{
        "id":message,"content":content,"author":{"user_openid":user}
    }})
}

#[test]
fn enqueue_and_execute_without_model_are_durable_idempotent_and_traceable() {
    let (_temporary, state) = open();
    let chat = conversation(&state, "web-admin");
    let text = "/memory remember 助手的这份来源私有记忆无需模型密钥";
    let first = worker::enqueue(
        &state,
        &chat,
        "web-admin",
        "same-client-message",
        text,
        None,
    )
    .unwrap();
    let duplicate = worker::enqueue(
        &state,
        &chat,
        "web-admin",
        "same-client-message",
        text,
        None,
    )
    .unwrap();
    assert_eq!(first, duplicate);
    assert!(worker::enqueue(
        &state,
        &chat,
        "web-admin",
        "same-client-message",
        "/memory remember 不同正文",
        None
    )
    .is_err());
    assert_eq!(scalar(&state, "SELECT count(*) FROM runs"), 1);
    mark_running(&state, &first);
    assert!(execute(&state, &first).unwrap());
    assert!(execute(&state, &first).unwrap());
    assert_eq!(
        scalar(
            &state,
            "SELECT count(*) FROM memory_records WHERE deleted_at IS NULL"
        ),
        1
    );
    assert_eq!(scalar(&state, "SELECT count(*) FROM memory_versions"), 1);
    assert_eq!(
        scalar(
            &state,
            "SELECT count(*) FROM messages WHERE role='assistant'"
        ),
        1
    );
    assert_eq!(
        scalar(&state, "SELECT count(*) FROM run_prompt_snapshots"),
        0
    );
    assert_eq!(scalar(&state, "SELECT count(*) FROM runs WHERE run_kind='memory_command' AND status='completed' AND character_snapshot_json IS NULL"), 1);
    state
        .db(|db| {
            let page = memory::visible_list(db, "web-admin", &ListOptions::default()).unwrap();
            let item = &page.items[0];
            assert_eq!(item.visibility, Visibility::Private);
            assert_eq!(item.source_owner_id.as_deref(), Some("web-admin"));
            assert_eq!(item.source_conversation_id.as_deref(), Some(chat.as_str()));
            let source: (String, String) = db.query_row(
                "SELECT run_id,role FROM messages WHERE id=?1",
                [item.source_message_id.as_ref().unwrap()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(source, (first, "user".into()));
            assert_eq!(
                memory::visible_list(db, "another-owner", &ListOptions::default())
                    .unwrap()
                    .total,
                0
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn command_reads_shared_memory_but_cannot_publish_change_or_forget_it() {
    let (_temporary, state) = open();
    let chat = conversation(&state, "alice");
    let shared = state
        .db(|db| {
            Ok(memory::create(
                db,
                "instance-admin",
                &CreateMemory {
                    content: "助手有一段明确批准的共享经历".into(),
                    visibility: Visibility::Shared,
                    source_conversation_id: None,
                    source_message_id: None,
                    publish: true,
                },
            ))
        })
        .unwrap()
        .unwrap();
    let (_, shown) = command(
        &state,
        &chat,
        "alice",
        &format!("/memory show {}", shared.id),
    );
    assert!(shown.contains(&shared.content));
    let (_, listed) = command(&state, &chat, "alice", "/memory list 共享经历");
    assert!(listed.contains(&shared.id));
    let (_, denied) = command(
        &state,
        &chat,
        "alice",
        &format!("/memory correct {}@1 越权覆盖", shared.id),
    );
    assert!(denied.contains("资源不存在"));
    let (_, denied) = command(
        &state,
        &chat,
        "alice",
        &format!("/memory forget {}@1", shared.id),
    );
    assert!(denied.contains("资源不存在"));
    command(
        &state,
        &chat,
        "alice",
        &format!("/memory publish {}@1", shared.id),
    );
    let (_, remembered) = command(
        &state,
        &chat,
        "alice",
        "/memory remember --shared 这只是字面内容，不能成为发布参数",
    );
    assert!(remembered.contains("已记住"));
    state
        .db(|db| {
            let actual = memory::get(db, &shared.id).unwrap();
            assert_eq!(actual.revision, 1);
            assert_eq!(actual.content, shared.content);
            assert_eq!(actual.visibility, Visibility::Shared);
            let private = memory::admin_list(
                db,
                &ListOptions {
                    visibility: Some(Visibility::Private),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(private.items.len(), 1);
            assert!(private.items[0].content.starts_with("--shared"));
            Ok(())
        })
        .unwrap();
    assert_eq!(
        scalar(
            &state,
            "SELECT count(*) FROM audit_events WHERE action='memory.visibility.approve'"
        ),
        1
    );
}

#[test]
fn source_private_commands_reject_other_owners_and_honor_revision_conflicts() {
    let (_temporary, state) = open();
    let alice = conversation(&state, "alice");
    let bob = conversation(&state, "bob");
    command(
        &state,
        &alice,
        "alice",
        "/memory remember 只有Alice当前来源域可见的鲸鱼秘密",
    );
    let item = state
        .db(|db| {
            Ok(memory::visible_list(db, "alice", &Default::default())
                .unwrap()
                .items
                .remove(0))
        })
        .unwrap();
    for text in [
        format!("/memory show {}", item.id),
        format!("/memory correct {}@1 他人覆盖", item.id),
        format!("/memory forget {}@1", item.id),
    ] {
        let (_, response) = command(&state, &bob, "bob", &text);
        assert!(response.contains("资源不存在"));
        assert!(!response.contains(&item.content));
    }
    let (_, response) = command(&state, &bob, "bob", "/memory list 鲸鱼秘密");
    assert!(!response.contains(&item.id));
    let (_, response) = command(
        &state,
        &alice,
        "alice",
        &format!("/memory correct {}@1 更新后的来源私有记忆", item.id),
    );
    assert!(response.contains(&format!("{}@2", item.id)));
    let (_, response) = command(
        &state,
        &alice,
        "alice",
        &format!("/memory forget {}@1", item.id),
    );
    assert!(response.contains("记忆已被修改"));
    state
        .db(|db| {
            assert_eq!(memory::get(db, &item.id).unwrap().revision, 2);
            Ok(())
        })
        .unwrap();
    let (_, response) = command(
        &state,
        &alice,
        "alice",
        &format!("/memory forget {}@2", item.id),
    );
    assert!(response.contains("已从记忆库移除"));
    assert_eq!(
        scalar(
            &state,
            "SELECT count(*) FROM memory_records WHERE deleted_at IS NULL"
        ),
        0
    );
    assert_eq!(
        scalar(
            &state,
            "SELECT count(*) FROM memory_versions WHERE content IS NOT NULL"
        ),
        0
    );
}

#[test]
fn qq_ingest_replay_executes_once_and_uses_private_source_domain_without_model_history() {
    let (_temporary, state) = open();
    let config = qq_config();
    let event = qq_event(
        "incoming-event",
        "platform-message",
        "alice",
        "/memory remember QQ私聊里的独立助手记忆",
    );
    qq::ingest(&state, &config, &event).unwrap();
    qq::ingest(&state, &config, &event).unwrap();
    assert_eq!(scalar(&state, "SELECT count(*) FROM inbox"), 1);
    let run: String = state
        .db(|db| {
            Ok(db.query_row(
                "SELECT id FROM runs WHERE client_message_id='platform-message'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    mark_running(&state, &run);
    assert!(execute(&state, &run).unwrap());
    assert!(execute(&state, &run).unwrap());
    qq::ingest(&state, &config, &event).unwrap();
    // A transport replay can have another event identifier but the same platform message.
    qq::ingest(
        &state,
        &config,
        &qq_event(
            "replayed-event",
            "platform-message",
            "alice",
            "/memory remember QQ私聊里的独立助手记忆",
        ),
    )
    .unwrap();
    assert_eq!(scalar(&state, "SELECT count(*) FROM runs"), 1);
    assert_eq!(scalar(&state, "SELECT count(*) FROM memory_records"), 1);
    assert_eq!(
        scalar(&state, "SELECT count(*) FROM outbox WHERE status='queued'"),
        1
    );
    assert_eq!(
        scalar(
            &state,
            "SELECT count(*) FROM messages WHERE role='assistant'"
        ),
        1
    );
    assert_eq!(
        scalar(&state, "SELECT count(*) FROM run_prompt_snapshots"),
        0
    );
    assert_eq!(
        scalar(
            &state,
            "SELECT count(*) FROM messages m JOIN runs r ON r.id=m.run_id WHERE r.run_kind='model'"
        ),
        0
    );
    state
        .db(|db| {
            let page = memory::visible_list(db, "qq:12345:c2c:alice", &Default::default()).unwrap();
            assert_eq!(page.total, 1);
            let item = &page.items[0];
            assert_eq!(item.source_owner_id.as_deref(), Some("qq:12345:c2c:alice"));
            assert_eq!(item.visibility, Visibility::Private);
            let source_run: String = db.query_row(
                "SELECT run_id FROM messages WHERE id=?1",
                [item.source_message_id.as_ref().unwrap()],
                |r| r.get(0),
            )?;
            assert_eq!(source_run, run);
            for other in [
                "qq:12345:c2c:bob",
                "qq:12345:group:alice",
                "qq:67890:c2c:alice",
            ] {
                assert_eq!(
                    memory::visible_list(db, other, &Default::default())
                        .unwrap()
                        .total,
                    0
                );
            }
            let target: String =
                db.query_row("SELECT target_json FROM outbox", [], |r| r.get(0))?;
            let target: Value = serde_json::from_str(&target)?;
            assert_eq!(target["target"], "alice");
            assert_eq!(target["account_id"], "12345");
            Ok(())
        })
        .unwrap();
}

fn expire_preferred_model(state: &AppState) {
    state
        .db(|db| {
            let tx = db.transaction()?;
            let (_, config) = character::current_config_in(&tx)?;
            let mut card = character::template("过期映射测试角色", "简短自然回复。");
            card["data"]["extensions"]["dsh"]["modules"]["chat"] =
                json!({"mode":"prefer","provider_hint":"deepseek","model_hint":config.model.model});
            let draft = character::create(&tx, "fixture-admin", card)?;
            let mapped = character::map_draft_model(
                &tx,
                &draft.card_id,
                &draft.draft_id,
                "fixture-admin",
                &draft.draft_etag,
                &config,
            )?;
            let published = character::publish(
                &tx,
                &draft.card_id,
                &draft.draft_id,
                "fixture-admin",
                &mapped.draft_etag,
                "memory-prefer-fixture",
                &config,
            )?;
            character::set_default(
                &tx,
                &character::CardRef {
                    card_id: draft.card_id,
                    revision: published["revision"].as_i64().unwrap(),
                },
                "none",
                &config,
            )?;
            let mut changed = config;
            changed.model.model = "changed-model-with-expired-character-binding".into();
            tx.execute(
                "UPDATE config_revisions SET config_json=?1 WHERE revision=1",
                [serde_json::to_string(&changed)?],
            )?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn expired_persona_model_preferences_do_not_block_web_or_fresh_qq_memory_commands() {
    let (_temporary, state) = open();
    let chat = conversation(&state, "web-owner");
    expire_preferred_model(&state);
    // A normal new persona binding fails, establishing the fixture really expired.
    assert!(state
        .db(|db| {
            let tx = db.transaction()?;
            let (_, config) = character::current_config_in(&tx)?;
            character::bind_new_conversation(&tx, &chat, None, &config)?;
            tx.commit()?;
            Ok(())
        })
        .is_err());
    let (_, response) = command(
        &state,
        &chat,
        "web-owner",
        "/memory remember 控制命令不依赖模型偏好",
    );
    assert!(response.contains("已记住"));
    let config = qq_config();
    qq::ingest(
        &state,
        &config,
        &qq_event(
            "control",
            "control",
            "new_qq_user",
            "/memory remember QQ首次控制消息也不依赖角色绑定",
        ),
    )
    .unwrap();
    let run: String = state
        .db(|db| {
            Ok(db.query_row(
                "SELECT id FROM runs WHERE client_message_id='control'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    mark_running(&state, &run);
    assert!(execute(&state, &run).unwrap());
    assert!(qq::ingest(
        &state,
        &config,
        &qq_event(
            "ordinary",
            "ordinary",
            "new_qq_user",
            "普通聊天仍应明确拒绝过期模型绑定"
        )
    )
    .is_err());
    assert_eq!(
        scalar(
            &state,
            "SELECT count(*) FROM memory_records WHERE deleted_at IS NULL"
        ),
        2
    );
    assert_eq!(scalar(&state,"SELECT count(*) FROM runs WHERE run_kind='memory_command' AND status='completed' AND character_snapshot_json IS NULL"),2);
}

#[test]
fn action_errors_commit_only_error_reply_and_reply_failure_rolls_back_memory_effects() {
    let (_temporary, state) = open();
    let chat = conversation(&state, "alice");
    state.db(|db| {
        db.execute_batch("CREATE TRIGGER fail_memory_audit BEFORE INSERT ON audit_events WHEN new.action='memory.create' BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;
        Ok(())
    }).unwrap();
    let (_, response) = command(
        &state,
        &chat,
        "alice",
        "/memory remember 审计故障不留下半条记忆",
    );
    assert!(response.contains("内部操作失败"));
    assert_eq!(scalar(&state, "SELECT count(*) FROM memory_records"), 0);
    assert_eq!(scalar(&state, "SELECT count(*) FROM memory_versions"), 0);
    assert_eq!(scalar(&state, "SELECT count(*) FROM memory_fts"), 0);
    assert_eq!(
        scalar(&state, "SELECT max(version) FROM dsh_schema_migrations"),
        1
    );
    state.db(|db| {
        db.execute_batch("DROP TRIGGER fail_memory_audit; CREATE TRIGGER fail_command_reply BEFORE INSERT ON messages WHEN new.role='assistant' BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;
        Ok(())
    }).unwrap();
    let run = worker::enqueue(
        &state,
        &chat,
        "alice",
        "retry-after-save-failure",
        "/memory remember 回复保存失败也不能先提交记忆",
        None,
    )
    .unwrap();
    mark_running(&state, &run);
    assert!(execute(&state, &run).is_err());
    assert_eq!(scalar(&state, "SELECT count(*) FROM memory_records"), 0);
    assert_eq!(scalar(&state, "SELECT count(*) FROM memory_versions"), 0);
    assert_eq!(
        scalar(&state, "SELECT max(version) FROM dsh_schema_migrations"),
        1
    );
    state
        .db(|db| {
            let (status, response): (String, Option<String>) = db.query_row(
                "SELECT status,response FROM runs WHERE id=?1",
                [&run],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(status, "running");
            assert!(response.is_none());
            db.execute_batch("DROP TRIGGER fail_command_reply")?;
            Ok(())
        })
        .unwrap();
    assert!(execute(&state, &run).unwrap());
    assert!(execute(&state, &run).unwrap());
    assert_eq!(scalar(&state, "SELECT count(*) FROM memory_records"), 1);
    assert_eq!(scalar(&state, "SELECT count(*) FROM memory_versions"), 1);
    assert_eq!(
        scalar(
            &state,
            "SELECT count(*) FROM messages WHERE role='assistant'"
        ),
        2
    );
}

fn ingest_and_execute(state: &AppState, config: &RuntimeConfig, event: &Value) -> String {
    qq::ingest(state, config, event).unwrap();
    let message = event["d"]["id"].as_str().unwrap();
    let run: String = state
        .db(|db| {
            Ok(db.query_row(
                "SELECT id FROM runs WHERE client_message_id=?1",
                [message],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    mark_running(state, &run);
    assert!(execute(state, &run).unwrap());
    state
        .db(|db| {
            Ok(
                db.query_row("SELECT response FROM runs WHERE id=?1", [&run], |r| {
                    r.get(0)
                })?,
            )
        })
        .unwrap()
}

#[test]
fn qq_groups_and_channels_can_read_but_cannot_create_correct_or_forget_memories() {
    let (_temporary, state) = open();
    let config = qq_config();
    let shared = state
        .db(|db| {
            Ok(memory::create(
                db,
                "instance-admin",
                &CreateMemory {
                    content: "经管理员批准的群聊与频道可读共享资料".into(),
                    visibility: Visibility::Shared,
                    source_conversation_id: None,
                    source_message_id: None,
                    publish: true,
                },
            ))
        })
        .unwrap()
        .unwrap();
    for (kind, event_type, field, target) in [
        (
            "group",
            "GROUP_AT_MESSAGE_CREATE",
            "group_openid",
            "team_01",
        ),
        ("channel", "AT_MESSAGE_CREATE", "channel_id", "channel_01"),
    ] {
        let event = |suffix: &str, content: &str| {
            let id = format!("{kind}-{suffix}");
            let mut data = json!({"id":id,"content":content,"author":{"id":"member_01","member_openid":"member_01"}});
            data[field] = json!(target);
            json!({"op":0,"id":id,"t":event_type,"d":data})
        };
        let before = scalar(&state, "SELECT count(*) FROM memory_records");
        let response = ingest_and_execute(
            &state,
            &config,
            &event(
                "remember",
                "/memory remember 群成员不能代表其他成员写长期事实",
            ),
        );
        assert!(response.contains("实例管理员"));
        assert!(!response.contains("已记住"));
        assert_eq!(
            scalar(&state, "SELECT count(*) FROM memory_records"),
            before
        );
        let owner = format!("qq:12345:{kind}:{target}");
        let source: String = state
            .db(|db| {
                Ok(db.query_row(
                    "SELECT id FROM conversations WHERE owner_id=?1",
                    [&owner],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        // Domain read access does not let group participants modify records that
        // were explicitly created for that domain by the instance administrator.
        let domain = state
            .db(|db| {
                Ok(memory::create(
                    db,
                    "instance-admin",
                    &CreateMemory {
                        content: format!("管理员维护的{kind}域资料"),
                        visibility: Visibility::Private,
                        source_conversation_id: Some(source),
                        source_message_id: None,
                        publish: false,
                    },
                ))
            })
            .unwrap()
            .unwrap();
        for (suffix, text) in [
            (
                "correct",
                format!("/memory correct {}@1 群成员覆盖", domain.id),
            ),
            ("forget", format!("/memory forget {}@1", domain.id)),
        ] {
            let response = ingest_and_execute(&state, &config, &event(suffix, &text));
            assert!(response.contains("实例管理员"));
        }
        let response = ingest_and_execute(
            &state,
            &config,
            &event("show", &format!("/memory show {}", shared.id)),
        );
        assert!(response.contains(&shared.content));
        let response = ingest_and_execute(&state, &config, &event("list", "/memory list 域资料"));
        assert!(response.contains(&domain.id));
        state
            .db(|db| {
                let actual = memory::get(db, &domain.id).unwrap();
                assert_eq!(actual.revision, 1);
                assert_eq!(actual.content, domain.content);
                Ok(())
            })
            .unwrap();
    }
    assert_eq!(scalar(&state, "SELECT count(*) FROM memory_records"), 3);
    assert_eq!(scalar(&state, "SELECT count(*) FROM memory_versions"), 3);
    assert_eq!(
        scalar(&state, "SELECT count(*) FROM outbox WHERE status='queued'"),
        10
    );
}

#[test]
fn draft_character_test_conversations_cannot_write_long_term_assistant_memory() {
    let (_temporary, state) = open();
    let chat = conversation(&state, "alice");
    state
        .db(|db| {
            let tx = db.transaction()?;
            let (_, config) = character::current_config_in(&tx)?;
            let draft = character::create(
                &tx,
                "alice",
                character::template("临时草稿测试角色", "本次只是独立角色草稿测试。"),
            )?;
            character::bind_test_conversation(
                &tx,
                &chat,
                &draft.card_id,
                &draft.draft_id,
                "alice",
                &draft.draft_etag,
                &config,
            )?;
            assert_eq!(
                character::connection_character(&tx, &chat)?["is_test"],
                true
            );
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    let (_, response) = command(
        &state,
        &chat,
        "alice",
        "/memory remember 不应污染助手长期记忆的草稿实验内容",
    );
    assert!(!response.contains("已记住"));
    assert!(response.contains("草稿") || response.contains("测试"));
    assert_eq!(scalar(&state, "SELECT count(*) FROM memory_records"), 0);
    assert_eq!(scalar(&state, "SELECT count(*) FROM memory_versions"), 0);
    assert_eq!(scalar(&state, "SELECT count(*) FROM memory_fts"), 0);
    let direct = state
        .db(|db| {
            Ok(memory::create(
                db,
                "instance-admin",
                &CreateMemory {
                    content: "不能借管理员显式来源路径绕过草稿限制".into(),
                    visibility: Visibility::Private,
                    source_conversation_id: Some(chat),
                    source_message_id: None,
                    publish: false,
                },
            ))
        })
        .unwrap();
    assert!(direct.is_err());
    assert_eq!(scalar(&state, "SELECT count(*) FROM memory_records"), 0);
}
