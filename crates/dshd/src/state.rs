use crate::config::RuntimeConfig;
use crate::error::{ApiError, ApiResult};
use rand::{rngs::OsRng, RngCore};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Notify;

#[derive(Clone)]
pub struct Options {
    pub data_dir: PathBuf,
    pub origin: String,
    pub allow_private_model: bool,
    pub cookie_secure: bool,
    pub web_dir: PathBuf,
}

#[derive(Clone)]
pub struct AppState(pub Arc<Inner>);
pub struct Inner {
    pub db: Mutex<Connection>,
    pub options: Options,
    pub started: Instant,
    pub notify: Notify,
    pub stopping: std::sync::atomic::AtomicBool,
    pub auth_attempts: Mutex<Vec<Instant>>,
    pub qq_token: tokio::sync::Mutex<Option<(String, i64, String)>>,
    pub qq_connection: Mutex<String>,
    pub qq_last_error: Mutex<Option<String>>,
    pub checks: tokio::sync::Semaphore,
    pub summary_slots: tokio::sync::Semaphore,
    pub weixin_signal: tokio::sync::watch::Sender<u64>,
    pub wasm_runtime: Mutex<crate::wasm_workbench::RuntimeStatus>,
    pub trigger_clock: Mutex<crate::trigger_scheduler::ClockHealth>,
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub fn random_token() -> String {
    let mut b = [0u8; 32];
    OsRng.fill_bytes(&mut b);
    hex::encode(b)
}
pub fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}
pub fn private_dir(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
pub fn private_write(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}

pub fn create_bootstrap_token(data_dir: &Path) -> anyhow::Result<PathBuf> {
    private_dir(data_dir)?;
    let path = data_dir.join("bootstrap-token.json");
    // This command is an explicit local administrative rotation. Token values never enter logs.
    let temp = data_dir.join(format!(".bootstrap-{}.tmp", uuid::Uuid::new_v4()));
    private_write(
        &temp,
        serde_json::to_string(&json!({"token":random_token(),"expires_at":now()+900}))?.as_bytes(),
    )?;
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    std::fs::rename(&temp, &path)?;
    Ok(path)
}

impl AppState {
    pub fn open(options: Options) -> anyhow::Result<Self> {
        private_dir(&options.data_dir)?;
        private_dir(&options.data_dir.join("secrets"))?;
        let mut db = Connection::open(options.data_dir.join("dsh.db"))?;
        db.busy_timeout(Duration::from_secs(3))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;",
        )?;
        db.execute_batch(include_str!("../schema.sql"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                options.data_dir.join("dsh.db"),
                std::fs::Permissions::from_mode(0o600),
            )?;
        }
        let count: i64 = db.query_row("SELECT count(*) FROM config_revisions", [], |r| r.get(0))?;
        if count == 0 {
            db.execute(
                "INSERT INTO config_revisions(revision,config_json,created_at) VALUES(1,?1,?2)",
                params![serde_json::to_string(&RuntimeConfig::default())?, now()],
            )?;
            db.execute(
                "INSERT INTO settings(key,value) VALUES('active_revision','1')",
                [],
            )?;
        }
        crate::character::migrate(&mut db)?;
        crate::memory::initialize(&mut db)?;
        crate::conversation_summary::initialize(&db)?;
        crate::weixin_store::initialize(&db)?;
        crate::weixin_login::initialize(&db)?;
        crate::memory_candidates::initialize(&db)?;
        crate::image_analysis::initialize(&db)?;
        crate::conversation_images::initialize(&db)?;
        crate::wasm_workbench::initialize(&db)?;
        crate::worker::initialize(&db)?;
        crate::chat_reply::initialize(&db)?;
        crate::media_assets::initialize(&db)?;
        crate::speech::initialize(&db)?;
        crate::qq_api::initialize(&db)?;
        crate::qq::incoming_media::initialize(&db)?;
        crate::qq::text_batch::initialize(&db)?;
        crate::plugin_factory::initialize(&db)?;
        crate::skills::initialize(&db)?;
        // Queued items survive restart. An interrupted external request is not blindly replayed.
        db.execute("UPDATE runs SET status='interrupted',error='服务重启时任务仍在执行；结果未确认，请人工重试',updated_at=?1 WHERE status='running'",[now()])?;
        db.execute("UPDATE outbox SET status='unknown',error='服务重启时发送结果未知' WHERE status='sending'",[])?;
        crate::trigger_calendar::initialize(&db)?;
        crate::trigger_store::initialize(&db)?;
        crate::trigger_retention::initialize(&db)?;
        crate::skills::initialize_trigger_setup(&db)?;
        crate::diary::initialize(&mut db)?;
        crate::diary_model::initialize(&db)?;
        crate::relationships::initialize(&db)?;
        crate::relationships_model::initialize(&db)?;
        crate::schedule::initialize(&db)?;
        crate::schedule_model::initialize(&db)?;
        crate::conversation_identity::initialize(&db)?;
        crate::turn_control::ensure_schema(&db)?;
        crate::delivery_context::ensure_schema(&db)?;
        crate::diary::initialize_delivery_capture(&db)?;
        crate::conversation_state::initialize(&db)?;
        crate::conversation_commitments::ensure_schema(&db)?;
        crate::context_budget::ensure_schema(&db)?;
        crate::rest_runtime::initialize_all(&db)?;
        crate::runtime_logs::initialize(&db)?;
        Ok(Self(Arc::new(Inner {
            db: Mutex::new(db),
            options,
            started: Instant::now(),
            notify: Notify::new(),
            stopping: std::sync::atomic::AtomicBool::new(false),
            auth_attempts: Mutex::new(Vec::new()),
            qq_token: tokio::sync::Mutex::new(None),
            qq_connection: Mutex::new("disabled".into()),
            qq_last_error: Mutex::new(None),
            checks: tokio::sync::Semaphore::new(2),
            summary_slots: tokio::sync::Semaphore::new(1),
            weixin_signal: tokio::sync::watch::channel(0u64).0,
            wasm_runtime: Mutex::new(crate::wasm_workbench::RuntimeStatus::default()),
            trigger_clock: Mutex::new(crate::trigger_scheduler::ClockHealth::default()),
        })))
    }

    pub fn db<T>(&self, f: impl FnOnce(&mut Connection) -> anyhow::Result<T>) -> ApiResult<T> {
        let mut db = self.0.db.lock().map_err(|_| ApiError::internal())?;
        f(&mut db).map_err(|error| {
            if let Some(source) =
                error.downcast_ref::<crate::conversation_context::SourceUnavailable>()
            {
                ApiError::new(
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "context_source_unavailable",
                    source.to_string(),
                )
            } else if let Some(character) = error.downcast_ref::<crate::character::CharacterError>()
            {
                ApiError::new(character.status, character.code, character.message.clone())
            } else if let Some(skill) = error.downcast_ref::<crate::skills::SkillError>() {
                ApiError::bad(skill.code, skill.message)
            } else if let Some(trigger) = error.downcast_ref::<crate::trigger_store::TriggerError>()
            {
                ApiError::new(trigger.status, trigger.code, trigger.message.clone())
            } else {
                ApiError::internal()
            }
        })
    }
    pub fn current_config(&self) -> ApiResult<(i64, RuntimeConfig)> {
        self.db(|db| {
            let (revision,text):(i64,String)=db.query_row("SELECT c.revision,c.config_json FROM config_revisions c JOIN settings s ON s.key='active_revision' AND c.revision=CAST(s.value AS INTEGER)",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
            Ok((revision,serde_json::from_str(&text)?))
        })
    }
    pub fn secret(&self, id: &str, kind: &str) -> ApiResult<String> {
        let exists = self.db(|db| {
            Ok(db
                .query_row(
                    "SELECT 1 FROM secrets WHERE id=?1 AND kind=?2",
                    params![id, kind],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?
                .is_some())
        })?;
        if !exists || uuid::Uuid::parse_str(id).is_err() {
            return Err(ApiError::bad(
                "credential_unavailable",
                "所选凭证不存在或类型不匹配",
            ));
        }
        std::fs::read_to_string(self.0.options.data_dir.join("secrets").join(id))
            .map_err(|_| ApiError::bad("credential_unavailable", "凭证文件不可用"))
    }
    pub fn check_config_secrets(&self, c: &RuntimeConfig) -> ApiResult<()> {
        if let Some(id) = &c.model.credential_ref {
            self.secret(id, "model_api_key")?;
        }
        if let Some(id) = &c.qq.credential_ref {
            self.secret(id, "qq_app_secret")?;
        }
        Ok(())
    }
    pub fn audit(&self, actor: &str, action: &str, resource: &str) -> ApiResult<()> {
        self.db(|db| {
            db.execute(
                "INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,?2,?3,?4)",
                params![actor, action, resource, now()],
            )?;
            Ok(())
        })
    }
    pub fn read_json(&self, sql: &str, bind: &str) -> ApiResult<Value> {
        self.db(|db| {
            let s: String = db.query_row(sql, [bind], |r| r.get(0))?;
            Ok(serde_json::from_str(&s)?)
        })
    }
}
