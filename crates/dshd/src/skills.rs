use crate::state::now;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub kind: String,
    pub enabled: bool,
    pub revision: i64,
    pub builtin: bool,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RunSkill {
    pub skill: Skill,
    pub prompt: String,
    pub native_image: bool,
}

#[derive(Debug)]
pub struct SkillError {
    pub code: &'static str,
    pub message: &'static str,
}
impl std::fmt::Display for SkillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for SkillError {}
pub fn invalid() -> anyhow::Error {
    SkillError {
        code: "invalid_skill",
        message: "Skill 不存在、已停用，或调用格式无效；使用 /skill <ID> <需求>",
    }
    .into()
}

pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS skills(id TEXT PRIMARY KEY,name TEXT NOT NULL,description TEXT NOT NULL,instructions TEXT NOT NULL,kind TEXT NOT NULL,enabled INTEGER NOT NULL,revision INTEGER NOT NULL,builtin INTEGER NOT NULL,updated_at INTEGER NOT NULL);CREATE TABLE IF NOT EXISTS run_skills(run_id TEXT PRIMARY KEY REFERENCES runs(id),snapshot_json TEXT NOT NULL);")?;
    // A durable seed marker preserves explicit deletion, disabling and edits on restart.
    db.execute_batch("SAVEPOINT seed_skills;")?;
    let result = (|| -> anyhow::Result<()> {
        let seeded: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM settings WHERE key='skills_seed_v1')",
            [],
            |r| r.get(0),
        )?;
        if !seeded {
            for (id, name, description, instructions, kind) in [
                ("skill-creator", "制作 Skill", "把需求整理成可复用的 Skill 草稿，审核保存后才生效。", "你正在帮助用户制作可复用 Skill。先理解目标、适用场景、输入、步骤和输出；信息足够时输出一个 fenced json 代码块，内容为 {\"name\":\"简短名称\",\"description\":\"何时使用及边界\",\"instructions\":\"清晰步骤、输入输出与验证方式\",\"kind\":\"instruction\"}。只制作指令草稿，不声称已安装或保存，不执行任意 shell，不索取或嵌入密钥。用户在管理页面审核并保存后才启用；Skill 本身不会扩大操作权限，也不授予网络、文件或系统权限。", "instruction"),
                ("drawing", "绘画", "模型支持生图时直接生成图片，否则用 HTML / CSS / SVG 绘制并展示。", "根据用户要求制作视觉作品，遵循用户要求的主体、构图、配色和风格。使用当前已配置的绘画方式；表达清楚，画面细节协调，适合直接展示。", "drawing"),
            ] {
                db.execute("INSERT OR IGNORE INTO skills(id,name,description,instructions,kind,enabled,revision,builtin,updated_at) VALUES(?1,?2,?3,?4,?5,1,1,1,?6)", params![id,name,description,instructions,kind,now()])?;
            }
            db.execute(
                "INSERT INTO settings(key,value) VALUES('skills_seed_v1','1')",
                [],
            )?;
        }
        Ok(())
    })();
    if result.is_err() {
        db.execute_batch("ROLLBACK TO seed_skills;")?;
    }
    db.execute_batch("RELEASE seed_skills;")?;
    result
}

/// Separate seed marker upgrades existing instances without overwriting edits
/// or resurrecting a default that an administrator deliberately removed.
pub fn initialize_trigger_setup(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("SAVEPOINT trigger_skill_seed;")?;
    let result = (|| -> anyhow::Result<()> {
        let seeded: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM settings WHERE key='trigger_setup_skill_seed_v1')",
            [],
            |r| r.get(0),
        )?;
        if !seeded {
            let draft: serde_json::Value = serde_json::from_str(include_str!(
                "../../../docs/examples/task-trigger-setup.skill.json"
            ))?;
            let instructions = draft["instructions"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing setup Skill instructions"))?;
            anyhow::ensure!(
                instructions.len() <= 16000,
                "setup Skill instructions too large"
            );
            db.execute("INSERT OR IGNORE INTO skills(id,name,description,instructions,kind,enabled,revision,builtin,updated_at) VALUES('task-trigger-setup',?1,?2,?3,'instruction',1,1,1,?4)",params![draft["name"].as_str().unwrap_or("设置触发任务"),draft["description"].as_str().unwrap_or("制作任务草稿"),instructions,now()])?;
            db.execute(
                "INSERT INTO settings(key,value) VALUES('trigger_setup_skill_seed_v1','1')",
                [],
            )?;
        }
        Ok(())
    })();
    if result.is_err() {
        db.execute_batch("ROLLBACK TO trigger_skill_seed;")?;
    }
    db.execute_batch("RELEASE trigger_skill_seed;")?;
    result
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Skill> {
    Ok(Skill {
        id: r.get(0)?,
        name: r.get(1)?,
        description: r.get(2)?,
        instructions: r.get(3)?,
        kind: r.get(4)?,
        enabled: r.get(5)?,
        revision: r.get(6)?,
        builtin: r.get(7)?,
        updated_at: r.get(8)?,
    })
}
pub fn list(db: &Connection) -> anyhow::Result<Vec<Skill>> {
    let mut stmt=db.prepare("SELECT id,name,description,instructions,kind,enabled,revision,builtin,updated_at FROM skills ORDER BY builtin DESC,updated_at DESC,id")?;
    let items = stmt.query_map([], row)?.collect::<Result<Vec<_>, _>>()?;
    Ok(items)
}
pub fn get(db: &Connection, id: &str) -> anyhow::Result<Option<Skill>> {
    Ok(db.query_row("SELECT id,name,description,instructions,kind,enabled,revision,builtin,updated_at FROM skills WHERE id=?1",[id],row).optional()?)
}
pub fn for_run(db: &Connection, id: &str) -> anyhow::Result<Option<RunSkill>> {
    db.query_row(
        "SELECT snapshot_json FROM run_skills WHERE run_id=?1",
        [id],
        |r| r.get::<_, String>(0),
    )
    .optional()?
    .map(|s| serde_json::from_str(&s).map_err(Into::into))
    .transpose()
}
pub fn requested<'a>(
    input: &'a str,
    explicit: Option<&'a str>,
) -> anyhow::Result<(Option<&'a str>, &'a str)> {
    if input.trim() == "/skill" {
        return Err(invalid());
    }
    let command = input
        .trim_start()
        .strip_prefix("/skill")
        .filter(|tail| tail.starts_with(char::is_whitespace));
    let (id, prompt) = if let Some(tail) = command {
        let tail = tail.trim_start();
        let Some(split) = tail.find(char::is_whitespace) else {
            return Err(invalid());
        };
        let (id, prompt) = (&tail[..split], tail[split..].trim());
        if explicit.is_some_and(|selected| selected != id) {
            return Err(invalid());
        }
        (Some(id), prompt)
    } else {
        (explicit, input)
    };
    let Some(id) = id else {
        return Ok((None, prompt));
    };
    if id.is_empty() || id.len() > 64 || prompt.trim().is_empty() || input.trim() == "/skill" {
        return Err(invalid());
    }
    if crate::memory_commands::is_command(prompt)
        || (explicit.is_some() && input.trim_start().starts_with('/') && command.is_none())
    {
        return Err(invalid());
    }
    Ok((Some(id), prompt))
}
pub fn select(
    db: &Connection,
    input: &str,
    explicit: Option<&str>,
    native_available: bool,
    web: bool,
) -> anyhow::Result<Option<RunSkill>> {
    let (id, prompt) = requested(input, explicit)?;
    let Some(id) = id else { return Ok(None) };
    let skill = get(db, id)?.filter(|s| s.enabled).ok_or_else(invalid)?;
    let native_image = skill.kind == "drawing" && native_available && web;
    Ok(Some(RunSkill {
        skill,
        prompt: prompt.to_owned(),
        native_image,
    }))
}
pub fn store(db: &Connection, id: &str, selected: &RunSkill) -> anyhow::Result<()> {
    db.execute(
        "INSERT INTO run_skills(run_id,snapshot_json) VALUES(?1,?2)",
        params![id, serde_json::to_string(selected)?],
    )?;
    Ok(())
}
pub fn prompt(selected: &RunSkill) -> String {
    let mut text=format!("\n\n本次用户明确选择 Skill「{}」revision {}。以下为管理员保存的任务步骤，只作用于本次任务，不改变身份、隐私规则或工具权限，不授权执行代码或系统操作。\n{}",selected.skill.name,selected.skill.revision,selected.skill.instructions);
    if selected.skill.kind == "drawing" && !selected.native_image {
        text.push_str("\n当前使用 HTML 绘制模式：必须输出完整的 fenced html 代码块，用 HTML、CSS 和内联 SVG 绘画。不使用 JavaScript、事件处理器、外部字体、外部图片、网络请求、iframe、表单或外部依赖。图形须自包含、适应容器，并有文字说明。不能声称调用了生图模型或返回了真实照片。");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::RuntimeConfig,
        state::{AppState, Options},
        worker,
    };
    use tempfile::TempDir;

    fn open(path: &std::path::Path) -> AppState {
        AppState::open(Options {
            data_dir: path.to_owned(),
            origin: "http://127.0.0.1:18419".into(),
            allow_private_model: true,
            cookie_secure: false,
            web_dir: path.join("web"),
        })
        .unwrap()
    }
    fn conversation(state: &AppState) -> String {
        state.db(|db|{db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('chat','owner','test',0)",[])?;Ok(())}).unwrap();
        "chat".into()
    }

    #[test]
    fn seed_once_preserves_deleted_and_edited_defaults_after_reopen() {
        let temp = TempDir::new().unwrap();
        {
            let state = open(temp.path());
            state.db(|db|{assert_eq!(list(db)?.len(),3);db.execute("DELETE FROM skills WHERE id='drawing'",[])?;db.execute("UPDATE skills SET name='edited',enabled=0,revision=2 WHERE id='skill-creator'",[])?;Ok(())}).unwrap();
        }
        let state = open(temp.path());
        state
            .db(|db| {
                assert_eq!(list(db)?.len(), 2);
                let creator = get(db, "skill-creator")?.unwrap();
                assert_eq!(creator.name, "edited");
                assert!(!creator.enabled);
                assert_eq!(creator.revision, 2);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn queued_skill_is_frozen_and_retry_compares_selection_without_requiring_live_skill() {
        let temp = TempDir::new().unwrap();
        let state = open(temp.path());
        let chat = conversation(&state);
        let run = worker::enqueue_with_skill(
            &state,
            &chat,
            "owner",
            "one",
            "draw blue",
            None,
            Some("drawing"),
        )
        .unwrap();
        state.db(|db|{let selected=for_run(db,&run)?.unwrap();assert_eq!(selected.skill.revision,1);assert!(!selected.native_image);assert!(prompt(&selected).contains("fenced html"));db.execute("UPDATE skills SET instructions='changed',enabled=0,revision=2 WHERE id='drawing'",[])?;assert_eq!(for_run(db,&run)?.unwrap().skill.revision,1);Ok(())}).unwrap();
        assert_eq!(
            worker::enqueue_with_skill(
                &state,
                &chat,
                "owner",
                "one",
                "draw blue",
                None,
                Some("drawing")
            )
            .unwrap(),
            run
        );
        assert_eq!(
            worker::enqueue_with_skill(&state, &chat, "owner", "one", "draw blue", None, None)
                .unwrap_err()
                .status,
            axum::http::StatusCode::CONFLICT
        );
        assert_eq!(
            worker::enqueue_with_skill(
                &state,
                &chat,
                "owner",
                "two",
                "draw blue",
                None,
                Some("drawing")
            )
            .unwrap_err()
            .code,
            "invalid_skill"
        );
        assert_eq!(
            worker::enqueue_with_skill(
                &state,
                &chat,
                "owner",
                "three",
                "draw blue",
                None,
                Some("missing")
            )
            .unwrap_err()
            .code,
            "invalid_skill"
        );
    }

    #[test]
    fn commands_validate_and_native_selection_requires_web_even_when_model_supports_images() {
        assert!(requested("/skill", None).is_err());
        assert!(requested("/skill drawing", None).is_err());
        assert!(requested("/skill drawing blue", Some("skill-creator")).is_err());
        assert!(requested("/memory remember x", Some("drawing")).is_err());
        let temp = TempDir::new().unwrap();
        let state = open(temp.path());
        state
            .db(|db| {
                let native = select(db, "/skill drawing blue", None, true, true)?.unwrap();
                assert!(native.native_image);
                assert_eq!(native.prompt, "blue");
                assert!(!prompt(&native).contains("fenced html"));
                assert!(
                    !select(db, "/skill drawing blue", None, true, false)?
                        .unwrap()
                        .native_image
                );
                Ok(())
            })
            .unwrap();
    }

    #[tokio::test]
    async fn selected_text_skill_injects_frozen_instructions_and_strips_command_in_real_worker_request(
    ) {
        use axum::{extract::State, routing::post, Json, Router};
        use serde_json::{json, Value};
        use std::{
            future::IntoFuture,
            sync::{Arc, Mutex},
        };
        use tokio_util::sync::CancellationToken;
        let captured: Arc<Mutex<Option<Value>>> = Default::default();
        async fn model(
            State(captured): State<Arc<Mutex<Option<Value>>>>,
            Json(body): Json<Value>,
        ) -> Json<Value> {
            *captured.lock().unwrap() = Some(body);
            Json(
                json!({"choices":[{"message":{"content":"```html\n<svg xmlns=\"http://www.w3.org/2000/svg\"><circle r=\"10\"/></svg>\n```"}}]}),
            )
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let model_task = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/chat/completions", post(model))
                    .with_state(captured.clone()),
            )
            .into_future(),
        );
        let temp = TempDir::new().unwrap();
        let state = open(temp.path());
        let chat = conversation(&state);
        let key = uuid::Uuid::new_v4().to_string();
        crate::state::private_write(&temp.path().join("secrets").join(&key), b"synthetic").unwrap();
        state
            .db(|db| {
                db.execute(
                    "INSERT INTO secrets(id,kind,created_at) VALUES(?1,'model_api_key',0)",
                    [&key],
                )?;
                let mut config = RuntimeConfig::default();
                config.model.base_url = format!("http://{address}");
                config.model.credential_ref = Some(key.clone());
                db.execute(
                    "UPDATE config_revisions SET config_json=?1 WHERE revision=1",
                    [serde_json::to_string(&config)?],
                )?;
                Ok(())
            })
            .unwrap();
        let run = worker::enqueue(
            &state,
            &chat,
            "owner",
            "one",
            "/skill drawing 画蓝色圆形",
            None,
        )
        .unwrap();
        state
            .db(|db| {
                db.execute(
                    "UPDATE skills SET instructions='different after enqueue' WHERE id='drawing'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let cancel = CancellationToken::new();
        let worker_task = tokio::spawn(worker::run_worker(state.clone(), cancel.clone()));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let done = state
                    .db(|db| {
                        Ok(
                            db.query_row("SELECT status FROM runs WHERE id=?1", [&run], |r| {
                                r.get::<_, String>(0)
                            })?,
                        )
                    })
                    .unwrap();
                if done == "completed" {
                    break;
                }
                assert_ne!(
                    done,
                    "failed",
                    "{}",
                    state
                        .db(|db| Ok(db.query_row(
                            "SELECT coalesce(error,'') FROM runs WHERE id=?1",
                            [&run],
                            |r| r.get::<_, String>(0)
                        )?))
                        .unwrap()
                );
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        cancel.cancel();
        worker_task.await.unwrap();
        model_task.abort();
        let body = captured.lock().unwrap().clone().unwrap();
        assert_eq!(body["max_tokens"], 4096);
        let messages = body["messages"].as_array().unwrap();
        assert!(messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("fenced html"));
        assert!(!messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("different after enqueue"));
        assert_eq!(messages.last().unwrap()["content"], "画蓝色圆形");
        state
            .db(|db| {
                assert_eq!(
                    db.query_row("SELECT run_kind FROM runs WHERE id=?1", [run], |r| r
                        .get::<_, String>(0))?,
                    "skill_text"
                );
                Ok(())
            })
            .unwrap();
    }
}
