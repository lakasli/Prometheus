use crate::{config::RuntimeConfig, error::ApiError, state::now};
use axum::http::StatusCode;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub const COMPILER_VERSION: &str = "dsh-character-v1";
pub const DEFAULT_KEY: &str = "character.default.v1";
pub type CharacterResult<T> = Result<T, CharacterError>;
#[derive(Debug)]
pub struct CharacterError {
    pub code: &'static str,
    pub message: String,
    pub status: StatusCode,
}
impl CharacterError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            status: StatusCode::UNPROCESSABLE_ENTITY,
        }
    }
    pub fn conflict() -> Self {
        Self {
            code: "character_conflict",
            message: "角色卡已更新，请重新载入后合并".into(),
            status: StatusCode::CONFLICT,
        }
    }
    pub fn missing() -> Self {
        Self {
            code: "character_not_found",
            message: "角色卡或版本不存在".into(),
            status: StatusCode::NOT_FOUND,
        }
    }
    pub fn into_api(self) -> ApiError {
        ApiError::new(self.status, self.code, self.message)
    }
}
impl std::fmt::Display for CharacterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for CharacterError {}
impl From<rusqlite::Error> for CharacterError {
    fn from(_: rusqlite::Error) -> Self {
        Self {
            code: "character_storage_error",
            message: "角色卡存储操作失败".into(),
            status: StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}
impl From<serde_json::Error> for CharacterError {
    fn from(_: serde_json::Error) -> Self {
        Self::new("invalid_character_card", "角色卡JSON无效")
    }
}
impl From<CharacterError> for ApiError {
    fn from(value: CharacterError) -> Self {
        value.into_api()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CardRef {
    pub card_id: String,
    pub revision: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StaticSnapshot {
    pub card_id: String,
    pub revision: i64,
    pub name: String,
    pub card_digest: String,
    pub projection_digest: String,
    pub compiler_version: String,
    pub system_prompt: String,
    pub config_revision: i64,
    pub module_digest: String,
    pub is_test: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft_etag: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Draft {
    pub card_id: String,
    pub draft_id: String,
    pub draft_etag: String,
    pub base_revision: i64,
    pub card: Value,
}

pub fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
pub fn canonical(card: &Value) -> CharacterResult<String> {
    Ok(serde_json::to_string(card)?)
}
pub fn card_digest(card: &Value) -> CharacterResult<String> {
    Ok(digest(canonical(card)?.as_bytes()))
}
pub fn current_config_in(db: &Connection) -> anyhow::Result<(i64, RuntimeConfig)> {
    let (rev,text):(i64,String)=db.query_row("SELECT c.revision,c.config_json FROM config_revisions c JOIN settings s ON s.key='active_revision' AND c.revision=CAST(s.value AS INTEGER)",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    Ok((rev, serde_json::from_str(&text)?))
}
pub fn migrate(db: &mut Connection) -> anyhow::Result<()> {
    let tx = db.transaction()?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS dsh_schema_migrations(version INTEGER PRIMARY KEY,created_at INTEGER NOT NULL);")?;
    let version: i64 = tx.query_row(
        "SELECT coalesce(max(version),0) FROM dsh_schema_migrations",
        [],
        |r| r.get(0),
    )?;
    anyhow::ensure!(version <= 3, "database schema is newer than this binary");
    tx.execute_batch("CREATE TABLE IF NOT EXISTS dsh_schema_migrations(version INTEGER PRIMARY KEY,created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS character_cards(id TEXT PRIMARY KEY,created_by TEXT NOT NULL,created_at INTEGER NOT NULL,archived_at INTEGER,qq_selectable INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS character_card_revisions(card_id TEXT NOT NULL REFERENCES character_cards(id),revision INTEGER NOT NULL,canonical_json TEXT NOT NULL,digest TEXT NOT NULL,created_at INTEGER NOT NULL,PRIMARY KEY(card_id,revision));
CREATE TABLE IF NOT EXISTS character_card_drafts(id TEXT PRIMARY KEY,card_id TEXT NOT NULL REFERENCES character_cards(id),owner_id TEXT NOT NULL,base_revision INTEGER NOT NULL,etag TEXT NOT NULL,card_json TEXT NOT NULL,chat_binding_digest TEXT,created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS character_module_bindings(card_id TEXT NOT NULL,revision INTEGER NOT NULL,module TEXT NOT NULL,model_digest TEXT NOT NULL,PRIMARY KEY(card_id,revision,module),FOREIGN KEY(card_id,revision) REFERENCES character_card_revisions(card_id,revision));
CREATE TABLE IF NOT EXISTS conversation_character_bindings(conversation_id TEXT PRIMARY KEY REFERENCES conversations(id),card_id TEXT NOT NULL REFERENCES character_cards(id),revision INTEGER NOT NULL,draft_card_json TEXT,draft_etag TEXT,chat_binding_digest TEXT);
CREATE TABLE IF NOT EXISTS character_imports(id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,input_digest TEXT NOT NULL,card_json TEXT NOT NULL,report_json TEXT NOT NULL,expires_at INTEGER NOT NULL,confirmed_json TEXT);
CREATE TABLE IF NOT EXISTS character_publish_keys(owner_id TEXT NOT NULL,key TEXT NOT NULL,request_digest TEXT NOT NULL,result_json TEXT NOT NULL,PRIMARY KEY(owner_id,key));
CREATE TABLE IF NOT EXISTS run_prompt_snapshots(run_id TEXT PRIMARY KEY REFERENCES runs(id),messages_json TEXT NOT NULL,digest TEXT NOT NULL,compiler_version TEXT NOT NULL,created_at INTEGER NOT NULL);
CREATE TRIGGER IF NOT EXISTS character_revision_no_update BEFORE UPDATE ON character_card_revisions BEGIN SELECT RAISE(ABORT,'immutable character revision'); END;
CREATE TRIGGER IF NOT EXISTS character_revision_no_delete BEFORE DELETE ON character_card_revisions BEGIN SELECT RAISE(ABORT,'immutable character revision'); END;")?;
    for (column, ty) in [
        ("character_snapshot_json", "TEXT"),
        ("character_digest", "TEXT"),
        ("compiler_version", "TEXT"),
        ("run_kind", "TEXT NOT NULL DEFAULT 'model'"),
    ] {
        let exists = {
            let mut stmt = tx.prepare("PRAGMA table_info(runs)")?;
            let columns = stmt
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?;
            columns.iter().any(|name| name == column)
        };
        if !exists {
            tx.execute_batch(&format!("ALTER TABLE runs ADD COLUMN {column} {ty}"))?;
        }
    }
    tx.execute(
        "INSERT OR IGNORE INTO dsh_schema_migrations(version,created_at) VALUES(1,?1)",
        [now()],
    )?;
    tx.commit()?;
    Ok(())
}
pub fn template(name: &str, prompt: &str) -> Value {
    json!({"spec":"chara_card_v3","spec_version":"3.0","data":{"name":name,"description":"","personality":"","scenario":"","first_mes":"","mes_example":"","creator_notes":"","system_prompt":prompt,"post_history_instructions":"","alternate_greetings":[],"group_only_greetings":[],"tags":[],"creator":"","character_version":"1.0.0","extensions":{"dsh":{"schema_version":1,"modules":{"chat":{"mode":"inherit"},"vision":{"mode":"disabled"},"speech":{"mode":"disabled"}}}}}})
}
fn issue(errors: &mut Vec<Value>, field: &str, message: &str) {
    errors.push(json!({"field":field,"message":message}));
}
fn keys(value: &Value, allowed: &[&str], path: &str, errors: &mut Vec<Value>) {
    if let Some(object) = value.as_object() {
        for key in object.keys() {
            if !allowed.contains(&key.as_str()) {
                issue(errors, &format!("{path}.{key}"), "不支持的字段");
            }
        }
    } else {
        issue(errors, path, "必须是JSON对象");
    }
}
pub fn validate(card: &Value) -> Vec<Value> {
    let mut errors = vec![];
    keys(card, &["spec", "spec_version", "data"], "$", &mut errors);
    if card["spec"] != "chara_card_v3" || card["spec_version"] != "3.0" {
        issue(&mut errors, "spec", "仅支持Character Card V3 3.0");
    }
    let data = &card["data"];
    let strings = [
        ("name", 64),
        ("description", 8000),
        ("personality", 4000),
        ("scenario", 4000),
        ("first_mes", 2000),
        ("mes_example", 4000),
        ("creator_notes", 4000),
        ("system_prompt", 8000),
        ("post_history_instructions", 4000),
        ("creator", 128),
        ("character_version", 64),
    ];
    let mut allowed: Vec<&str> = strings.iter().map(|(name, _)| *name).collect();
    allowed.extend([
        "nickname",
        "alternate_greetings",
        "group_only_greetings",
        "tags",
        "extensions",
    ]);
    keys(data, &allowed, "data", &mut errors);
    for (field, max) in strings {
        match data[field].as_str() {
            Some(text)
                if text.chars().count() <= max
                    && (!matches!(field, "name" | "character_version")
                        || !text.trim().is_empty()) => {}
            _ => issue(
                &mut errors,
                &format!("data.{field}"),
                "文本为空、类型不正确或超过字段上限",
            ),
        }
    }
    if let Some(value) = data.get("nickname") {
        if value.as_str().is_none_or(|s| s.chars().count() > 64) {
            issue(&mut errors, "data.nickname", "昵称必须是64字符内文本");
        }
    }
    for (field, max, len, unique) in [
        ("alternate_greetings", 8, 2000, false),
        ("group_only_greetings", 8, 2000, false),
        ("tags", 16, 64, true),
    ] {
        match data[field].as_array() {
            Some(items) if items.len() <= max => {
                let mut seen = HashSet::new();
                for item in items {
                    if !item.as_str().is_some_and(|s| {
                        s.chars().count() <= len && (!unique || (!s.is_empty() && seen.insert(s)))
                    }) {
                        issue(
                            &mut errors,
                            &format!("data.{field}"),
                            "数组元素无效、超长或重复",
                        );
                    }
                }
            }
            _ => issue(
                &mut errors,
                &format!("data.{field}"),
                "数组类型不正确或数量超限",
            ),
        }
    }
    let ext = &data["extensions"];
    keys(ext, &["dsh"], "data.extensions", &mut errors);
    let dsh = &ext["dsh"];
    keys(
        dsh,
        &["schema_version", "modules"],
        "data.extensions.dsh",
        &mut errors,
    );
    if dsh["schema_version"].as_i64() != Some(1) {
        issue(
            &mut errors,
            "data.extensions.dsh.schema_version",
            "仅支持DSH角色卡版本1",
        );
    }
    let modules = &dsh["modules"];
    keys(
        modules,
        &["chat", "vision", "speech"],
        "data.extensions.dsh.modules",
        &mut errors,
    );
    for module in ["chat", "vision", "speech"] {
        let pref = &modules[module];
        let path = format!("data.extensions.dsh.modules.{module}");
        match pref["mode"].as_str() {
            Some("inherit") => keys(pref, &["mode"], &path, &mut errors),
            Some("disabled") if module != "chat" => keys(pref, &["mode"], &path, &mut errors),
            Some("prefer") => {
                let mut fields = vec!["mode", "provider_hint", "model_hint"];
                if module == "speech" {
                    fields.push("voice_hint");
                }
                keys(pref, &fields, &path, &mut errors);
                for (field, max) in [("provider_hint", 96), ("model_hint", 200)] {
                    if !pref[field]
                        .as_str()
                        .is_some_and(|s| !s.trim().is_empty() && s.chars().count() <= max)
                    {
                        issue(
                            &mut errors,
                            &format!("{path}.{field}"),
                            "偏好需要完整provider与model提示",
                        );
                    }
                }
                if let Some(voice) = pref.get("voice_hint") {
                    if !voice
                        .as_str()
                        .is_some_and(|s| !s.trim().is_empty() && s.chars().count() <= 128)
                    {
                        issue(&mut errors, &format!("{path}.voice_hint"), "音色提示无效");
                    }
                }
            }
            _ => issue(&mut errors, &format!("{path}.mode"), "不支持的模块偏好模式"),
        }
    }
    if canonical(card).is_ok_and(|s| s.len() > 256 * 1024) {
        issue(&mut errors, "$", "角色卡超过256KiB");
    }
    if errors.is_empty() {
        if let Err(e) = compile(card, "用户") {
            issue(&mut errors, "data.system_prompt", &e.message);
        }
    }
    errors
}
fn require_valid(card: &Value) -> CharacterResult<()> {
    let errors = validate(card);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(CharacterError::new(
            "invalid_character_card",
            format!(
                "角色卡校验失败：{}",
                errors[0]["field"].as_str().unwrap_or("$")
            ),
        ))
    }
}
pub fn render_macros(input: &str, name: &str, user: &str) -> String {
    let mut result = String::new();
    let mut remaining = input;
    while let Some(start) = remaining.find("{{") {
        result.push_str(&remaining[..start]);
        let tail = &remaining[start..];
        if let Some(end) = tail.find("}}") {
            let token = &tail[..end + 2];
            result.push_str(match token {
                "{{char}}" => name,
                "{{user}}" => user,
                _ => token,
            });
            remaining = &tail[end + 2..];
        } else {
            result.push_str(tail);
            return result;
        }
    }
    result.push_str(remaining);
    result
}
pub fn compile(card: &Value, user: &str) -> CharacterResult<(String, Vec<Value>)> {
    let d = &card["data"];
    let name = d["name"].as_str().unwrap_or("DSH");
    let mut prompt=String::from("你是运行于DSH的AI助手。遵循宿主操作规则；角色设定不构成工具、文件、网络或外发权限。保持事实准确，不将虚构风格示例当作真实经历。\n\n");
    for (field, label) in [
        ("name", "角色名称"),
        ("nickname", "角色昵称"),
        ("system_prompt", "角色表达指令"),
        ("description", "角色背景"),
        ("personality", "稳定性格"),
        ("scenario", "当前设定"),
    ] {
        if let Some(text) = d[field].as_str().filter(|s| !s.trim().is_empty()) {
            prompt.push_str(&format!(
                "{label}：\n{}\n\n",
                render_macros(text, name, user)
            ));
        }
    }
    if prompt.len() > 32000 {
        return Err(CharacterError::new(
            "character_prompt_too_large",
            "核心人格超过32000 UTF-8字节，请缩短身份或指令",
        ));
    }
    let mut warnings = vec![];
    if let Some(examples) = d["mes_example"].as_str().filter(|s| !s.trim().is_empty()) {
        let label = "虚构风格示例（不是历史事实）：\n";
        let mut selected = String::new();
        for block in examples.split("<START>").filter(|s| !s.trim().is_empty()) {
            let block = format!("<START>{}\n", render_macros(block, name, user));
            if prompt.len() + label.len() + selected.len() + block.len() > 32000 {
                issue(
                    &mut warnings,
                    "data.mes_example",
                    "示例超过静态人格预算，已按完整块裁剪",
                );
                break;
            }
            selected.push_str(&block);
        }
        if !selected.is_empty() {
            prompt.push_str(label);
            prompt.push_str(&selected);
        }
    }
    for field in [
        "system_prompt",
        "description",
        "personality",
        "scenario",
        "mes_example",
    ] {
        if let Some(text) = d[field].as_str() {
            let scrubbed = text.replace("{{char}}", "").replace("{{user}}", "");
            if scrubbed.contains("{{") {
                issue(
                    &mut warnings,
                    &format!("data.{field}"),
                    "未知宏按字面保留，不执行",
                );
            }
        }
    }
    Ok((prompt, warnings))
}
pub fn model_digest(config: &RuntimeConfig) -> CharacterResult<String> {
    card_digest(&serde_json::to_value(&config.model)?)
}
pub fn binding_errors(
    card: &Value,
    config: &RuntimeConfig,
    chat_binding: Option<&str>,
) -> Vec<Value> {
    let mut errors = vec![];
    let modules = &card["data"]["extensions"]["dsh"]["modules"];
    for module in ["vision", "speech"] {
        if modules[module]["mode"] != "disabled" {
            issue(
                &mut errors,
                &format!("data.extensions.dsh.modules.{module}"),
                "当前实例尚未提供此模块，请选择disabled",
            );
        }
    }
    if modules["chat"]["mode"] == "prefer" && model_digest(config).ok().as_deref() != chat_binding {
        issue(
            &mut errors,
            "data.extensions.dsh.modules.chat",
            "请显式映射到当前模型，或选择inherit",
        );
    }
    errors
}
pub fn create(db: &Connection, owner: &str, card: Value) -> CharacterResult<Draft> {
    require_valid(&card)?;
    let id = uuid::Uuid::new_v4().to_string();
    db.execute(
        "INSERT INTO character_cards(id,created_by,created_at) VALUES(?1,?2,?3)",
        params![id, owner, now()],
    )?;
    create_draft_value(db, &id, owner, 0, card)
}
fn create_draft_value(
    db: &Connection,
    id: &str,
    owner: &str,
    base: i64,
    card: Value,
) -> CharacterResult<Draft> {
    let draft = Draft {
        card_id: id.into(),
        draft_id: uuid::Uuid::new_v4().to_string(),
        draft_etag: uuid::Uuid::new_v4().to_string(),
        base_revision: base,
        card,
    };
    db.execute("INSERT INTO character_card_drafts(id,card_id,owner_id,base_revision,etag,card_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![draft.draft_id,id,owner,base,draft.draft_etag,canonical(&draft.card)?,now()])?;
    Ok(draft)
}
pub fn get_draft(
    db: &Connection,
    card_id: &str,
    draft_id: &str,
    owner: &str,
) -> CharacterResult<Draft> {
    let row:Option<(String,i64,String)>=db.query_row("SELECT etag,base_revision,card_json FROM character_card_drafts WHERE id=?1 AND card_id=?2 AND owner_id=?3",params![draft_id,card_id,owner],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let (etag, base, text) = row.ok_or_else(CharacterError::missing)?;
    Ok(Draft {
        card_id: card_id.into(),
        draft_id: draft_id.into(),
        draft_etag: etag,
        base_revision: base,
        card: serde_json::from_str(&text)?,
    })
}
pub fn from_revision(
    db: &Connection,
    id: &str,
    revision: i64,
    owner: &str,
) -> CharacterResult<Draft> {
    ensure_active(db, id)?;
    let card = revision_card(
        db,
        &CardRef {
            card_id: id.into(),
            revision,
        },
        true,
    )?;
    let latest: i64 = db.query_row(
        "SELECT coalesce(max(revision),0) FROM character_card_revisions WHERE card_id=?1",
        [id],
        |r| r.get(0),
    )?;
    create_draft_value(db, id, owner, latest, card)
}
pub fn update_draft(
    db: &Connection,
    id: &str,
    draft_id: &str,
    owner: &str,
    expected: &str,
    card: Value,
) -> CharacterResult<Draft> {
    require_valid(&card)?;
    let old = get_draft(db, id, draft_id, owner)?;
    if old.draft_etag != expected {
        return Err(CharacterError::conflict());
    }
    ensure_active(db, id)?;
    let etag = uuid::Uuid::new_v4().to_string();
    let changed=db.execute("UPDATE character_card_drafts SET card_json=?1,etag=?2,chat_binding_digest=NULL WHERE id=?3 AND etag=?4",params![canonical(&card)?,etag,draft_id,expected])?;
    if changed != 1 {
        return Err(CharacterError::conflict());
    }
    get_draft(db, id, draft_id, owner)
}
pub fn map_draft_model(
    db: &Connection,
    id: &str,
    draft_id: &str,
    owner: &str,
    expected: &str,
    config: &RuntimeConfig,
) -> CharacterResult<Draft> {
    ensure_active(db, id)?;
    let draft = get_draft(db, id, draft_id, owner)?;
    if draft.draft_etag != expected {
        return Err(CharacterError::conflict());
    }
    let etag = uuid::Uuid::new_v4().to_string();
    db.execute(
        "UPDATE character_card_drafts SET chat_binding_digest=?1,etag=?2 WHERE id=?3",
        params![model_digest(config)?, etag, draft_id],
    )?;
    get_draft(db, id, draft_id, owner)
}
pub fn draft_binding(db: &Connection, draft_id: &str) -> CharacterResult<Option<String>> {
    Ok(db.query_row(
        "SELECT chat_binding_digest FROM character_card_drafts WHERE id=?1",
        [draft_id],
        |r| r.get(0),
    )?)
}
fn ensure_active(db: &Connection, id: &str) -> CharacterResult<()> {
    let archived: Option<Option<i64>> = db
        .query_row(
            "SELECT archived_at FROM character_cards WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    match archived {
        Some(None) => Ok(()),
        Some(Some(_)) => Err(CharacterError::new(
            "character_archived",
            "角色卡已归档，不能建立新绑定",
        )),
        None => Err(CharacterError::missing()),
    }
}
pub fn revision_card(
    db: &Connection,
    reference: &CardRef,
    allow_archived: bool,
) -> CharacterResult<Value> {
    if !allow_archived {
        ensure_active(db, &reference.card_id)?;
    }
    let text: Option<String> = db
        .query_row(
            "SELECT canonical_json FROM character_card_revisions WHERE card_id=?1 AND revision=?2",
            params![reference.card_id, reference.revision],
            |r| r.get(0),
        )
        .optional()?;
    Ok(serde_json::from_str(
        &text.ok_or_else(CharacterError::missing)?,
    )?)
}
pub fn publish(
    db: &Connection,
    id: &str,
    draft_id: &str,
    owner: &str,
    expected: &str,
    key: &str,
    config: &RuntimeConfig,
) -> CharacterResult<Value> {
    if key.is_empty() || key.len() > 128 {
        return Err(CharacterError::new(
            "idempotency_key_required",
            "发布必须提供128字节内Idempotency-Key",
        ));
    }
    let request = digest(format!("{id}:{draft_id}:{expected}").as_bytes());
    let prior:Option<(String,String)>=db.query_row("SELECT request_digest,result_json FROM character_publish_keys WHERE owner_id=?1 AND key=?2",params![owner,key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((stored, result)) = prior {
        if stored != request {
            return Err(CharacterError::conflict());
        }
        return Ok(serde_json::from_str(&result)?);
    }
    ensure_active(db, id)?;
    let draft = get_draft(db, id, draft_id, owner)?;
    if draft.draft_etag != expected {
        return Err(CharacterError::conflict());
    }
    require_valid(&draft.card)?;
    let binding = draft_binding(db, draft_id)?;
    if !binding_errors(&draft.card, config, binding.as_deref()).is_empty() {
        return Err(CharacterError::new(
            "module_unmapped",
            "模块未映射或当前实例不支持，请先校验卡片",
        ));
    }
    let latest: i64 = db.query_row(
        "SELECT coalesce(max(revision),0) FROM character_card_revisions WHERE card_id=?1",
        [id],
        |r| r.get(0),
    )?;
    if latest != draft.base_revision {
        return Err(CharacterError::conflict());
    }
    let revision = latest + 1;
    let digest = card_digest(&draft.card)?;
    let etag = uuid::Uuid::new_v4().to_string();
    db.execute("INSERT INTO character_card_revisions(card_id,revision,canonical_json,digest,created_at) VALUES(?1,?2,?3,?4,?5)",params![id,revision,canonical(&draft.card)?,digest,now()])?;
    if let Some(binding) = binding {
        db.execute("INSERT INTO character_module_bindings(card_id,revision,module,model_digest) VALUES(?1,?2,'chat',?3)",params![id,revision,binding])?;
    }
    db.execute(
        "UPDATE character_card_drafts SET base_revision=?1,etag=?2 WHERE id=?3",
        params![revision, etag, draft_id],
    )?;
    let result = json!({"card_id":id,"revision":revision,"name":draft.card["data"]["name"],"digest":digest,"draft_etag":etag});
    db.execute("INSERT INTO character_publish_keys(owner_id,key,request_digest,result_json) VALUES(?1,?2,?3,?4)",params![owner,key,request,result.to_string()])?;
    Ok(result)
}
pub fn default_reference(db: &Connection) -> CharacterResult<Option<CardRef>> {
    let text: Option<String> = db
        .query_row(
            "SELECT value FROM settings WHERE key=?1",
            [DEFAULT_KEY],
            |r| r.get(0),
        )
        .optional()?;
    text.map(|t| serde_json::from_str(&t).map_err(Into::into))
        .transpose()
}
pub fn default_state(db: &Connection) -> CharacterResult<Value> {
    let reference = default_reference(db)?;
    let etag = if let Some(r) = &reference {
        digest(serde_json::to_string(r)?.as_bytes())
    } else {
        "none".into()
    };
    let view = reference
        .map(|r| {
            let card = revision_card(db, &r, true)?;
            Ok::<_, CharacterError>(
                json!({"card_id":r.card_id,"revision":r.revision,"name":card["data"]["name"]}),
            )
        })
        .transpose()?;
    Ok(json!({"default":view,"default_etag":etag}))
}
fn mark_feature(db: &Connection) -> CharacterResult<()> {
    db.execute("INSERT INTO settings(key,value) VALUES('feature.character-cards.v1','1') ON CONFLICT(key) DO UPDATE SET value='1'",[])?;
    Ok(())
}
pub fn set_default(
    db: &Connection,
    reference: &CardRef,
    expected: &str,
    config: &RuntimeConfig,
) -> CharacterResult<Value> {
    if default_state(db)?["default_etag"] != expected {
        return Err(CharacterError::conflict());
    }
    let card = revision_card(db, reference, false)?;
    check_published_binding(db, reference, &card, config)?;
    db.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![DEFAULT_KEY,serde_json::to_string(reference)?])?;
    mark_feature(db)?;
    default_state(db)
}
fn check_published_binding(
    db: &Connection,
    reference: &CardRef,
    card: &Value,
    config: &RuntimeConfig,
) -> CharacterResult<()> {
    let binding:Option<String>=db.query_row("SELECT model_digest FROM character_module_bindings WHERE card_id=?1 AND revision=?2 AND module='chat'",params![reference.card_id,reference.revision],|r|r.get(0)).optional()?;
    if binding_errors(card, config, binding.as_deref()).is_empty() {
        Ok(())
    } else {
        Err(CharacterError::new(
            "module_unmapped",
            "当前模型映射已失效或模块不受支持",
        ))
    }
}
pub fn bind_new_conversation(
    db: &Connection,
    conversation_id: &str,
    selected: Option<&CardRef>,
    config: &RuntimeConfig,
) -> CharacterResult<Option<CardRef>> {
    let reference = selected
        .cloned()
        .map(Some)
        .unwrap_or(default_reference(db)?);
    if let Some(reference) = &reference {
        let card = revision_card(db, reference, false)?;
        check_published_binding(db, reference, &card, config)?;
        db.execute("INSERT INTO conversation_character_bindings(conversation_id,card_id,revision) VALUES(?1,?2,?3)",params![conversation_id,reference.card_id,reference.revision])?;
        mark_feature(db)?;
    }
    Ok(reference)
}
pub fn bind_test_conversation(
    db: &Connection,
    conversation_id: &str,
    card_id: &str,
    draft_id: &str,
    owner: &str,
    expected: &str,
    config: &RuntimeConfig,
) -> CharacterResult<()> {
    ensure_active(db, card_id)?;
    let draft = get_draft(db, card_id, draft_id, owner)?;
    if draft.draft_etag != expected {
        return Err(CharacterError::conflict());
    }
    let binding = draft_binding(db, draft_id)?;
    if !binding_errors(&draft.card, config, binding.as_deref()).is_empty() {
        return Err(CharacterError::new(
            "module_unmapped",
            "测试前请完成模块映射",
        ));
    }
    db.execute("INSERT INTO conversation_character_bindings(conversation_id,card_id,revision,draft_card_json,draft_etag,chat_binding_digest) VALUES(?1,?2,0,?3,?4,?5)",params![conversation_id,card_id,canonical(&draft.card)?,expected,binding])?;
    mark_feature(db)?;
    Ok(())
}
pub fn freeze_for_run(
    db: &Connection,
    conversation_id: &str,
    config_revision: i64,
    config: &RuntimeConfig,
    user_name: &str,
) -> CharacterResult<Option<StaticSnapshot>> {
    type BindingRow = (String, i64, Option<String>, Option<String>, Option<String>);
    let row:Option<BindingRow>=db.query_row("SELECT card_id,revision,draft_card_json,draft_etag,chat_binding_digest FROM conversation_character_bindings WHERE conversation_id=?1",[conversation_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let Some((card_id, revision, draft, draft_etag, binding)) = row else {
        return Ok(None);
    };
    let reference = CardRef {
        card_id: card_id.clone(),
        revision,
    };
    let is_test = draft.is_some();
    let card = if let Some(text) = draft {
        let card: Value = serde_json::from_str(&text)?;
        if !binding_errors(&card, config, binding.as_deref()).is_empty() {
            return Err(CharacterError::new("module_unmapped", "测试模型映射已失效"));
        }
        card
    } else {
        let card = revision_card(db, &reference, true)?;
        check_published_binding(db, &reference, &card, config)?;
        card
    };
    let (system_prompt, _) = compile(&card, user_name)?;
    let module_digest = model_digest(config)?;
    let projection_digest = card_digest(&json!({
        "compiler_version": COMPILER_VERSION,
        "module_digest": module_digest,
        "system_prompt": system_prompt,
        "is_test": is_test,
        "draft_etag": draft_etag
    }))?;
    Ok(Some(StaticSnapshot {
        card_id,
        revision,
        name: card["data"]["name"].as_str().unwrap_or("DSH").into(),
        card_digest: card_digest(&card)?,
        projection_digest,
        compiler_version: COMPILER_VERSION.into(),
        system_prompt,
        config_revision,
        module_digest,
        is_test,
        draft_etag,
    }))
}
pub fn connection_character(db: &Connection, conversation_id: &str) -> CharacterResult<Value> {
    let row:Option<(String,i64,Option<String>,Option<String>)>=db.query_row("SELECT card_id,revision,draft_card_json,draft_etag FROM conversation_character_bindings WHERE conversation_id=?1",[conversation_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let Some((id, revision, draft, etag)) = row else {
        return Ok(Value::Null);
    };
    let test = draft.is_some();
    let card = if let Some(text) = draft {
        serde_json::from_str(&text)?
    } else {
        revision_card(
            db,
            &CardRef {
                card_id: id.clone(),
                revision,
            },
            true,
        )?
    };
    Ok(
        json!({"card_id":id,"revision":revision,"name":card["data"]["name"],"greeting":card["data"]["first_mes"],"digest":card_digest(&card)?,"is_test":test,"draft_etag":etag}),
    )
}
pub fn list(db: &Connection, owner: &str) -> CharacterResult<Value> {
    let mut stmt=db.prepare("SELECT id,archived_at,coalesce((SELECT max(revision) FROM character_card_revisions WHERE card_id=character_cards.id),0),qq_selectable FROM character_cards ORDER BY created_at DESC,id LIMIT 100")?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, bool>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut items = vec![];
    for (id, archived, latest, qq_selectable) in rows {
        let draft_id:Option<String>=db.query_row("SELECT id FROM character_card_drafts WHERE card_id=?1 AND owner_id=?2 ORDER BY created_at DESC,rowid DESC LIMIT 1",params![id,owner],|r|r.get(0)).optional()?;
        let draft = draft_id
            .map(|d| get_draft(db, &id, &d, owner))
            .transpose()?;
        let name = if latest > 0 {
            revision_card(
                db,
                &CardRef {
                    card_id: id.clone(),
                    revision: latest,
                },
                true,
            )?["data"]["name"]
                .clone()
        } else {
            draft
                .as_ref()
                .map(|d| d.card["data"]["name"].clone())
                .unwrap_or(json!("角色草稿"))
        };
        items.push(json!({"id":id,"name":name,"archived_at":archived,"latest_revision":latest,"qq_selectable":qq_selectable,"draft":draft}));
    }
    let mut result = default_state(db)?;
    result["items"] = json!(items);
    Ok(result)
}
pub fn revisions(db: &Connection, id: &str) -> CharacterResult<Value> {
    let mut stmt=db.prepare("SELECT revision,digest,canonical_json,created_at FROM character_card_revisions WHERE card_id=?1 ORDER BY revision DESC LIMIT 100")?;
    let rows = stmt
        .query_map([id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut items = vec![];
    for (revision, digest, text, created) in rows {
        let card: Value = serde_json::from_str(&text)?;
        items.push(json!({"card_id":id,"revision":revision,"digest":digest,"name":card["data"]["name"],"created_at":created}));
    }
    Ok(json!({"items":items}))
}
pub fn archive(db: &Connection, id: &str, expected: &str) -> CharacterResult<Value> {
    ensure_active(db, id)?;
    let latest: i64 = db.query_row(
        "SELECT coalesce(max(revision),0) FROM character_card_revisions WHERE card_id=?1",
        [id],
        |r| r.get(0),
    )?;
    if latest.to_string() != expected {
        return Err(CharacterError::conflict());
    }
    if default_reference(db)?.is_some_and(|r| r.card_id == id) {
        return Err(CharacterError::new(
            "character_is_default",
            "请先将新会话默认切换到其他角色再归档",
        ));
    }
    db.execute(
        "UPDATE character_cards SET archived_at=?1 WHERE id=?2",
        params![now(), id],
    )?;
    Ok(json!({"ok":true}))
}

pub fn set_qq_access(
    db: &Connection,
    id: &str,
    expected: &str,
    enabled: bool,
) -> CharacterResult<Value> {
    ensure_active(db, id)?;
    let latest: i64 = db.query_row(
        "SELECT coalesce(max(revision),0) FROM character_card_revisions WHERE card_id=?1",
        [id],
        |r| r.get(0),
    )?;
    if latest.to_string() != expected {
        return Err(CharacterError::conflict());
    }
    db.execute(
        "UPDATE character_cards SET qq_selectable=?1 WHERE id=?2",
        params![enabled, id],
    )?;
    Ok(json!({"card_id":id,"qq_selectable":enabled}))
}
pub fn list_published(db: &Connection) -> CharacterResult<Vec<Value>> {
    let mut stmt=db.prepare("SELECT c.id,r.revision,r.canonical_json FROM character_cards c JOIN character_card_revisions r ON r.card_id=c.id WHERE c.archived_at IS NULL AND c.qq_selectable=1 AND r.revision=(SELECT max(revision) FROM character_card_revisions WHERE card_id=c.id) ORDER BY c.created_at DESC,c.id LIMIT 20")?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut items = vec![];
    for (id, revision, text) in rows {
        let card: Value = serde_json::from_str(&text)?;
        items.push(json!({"card_id":id,"revision":revision,"name":card["data"]["name"]}));
    }
    Ok(items)
}
pub fn find_published(db: &Connection, selector: &str) -> CharacterResult<CardRef> {
    let mut stmt=db.prepare("SELECT c.id,r.revision,r.canonical_json FROM character_cards c JOIN character_card_revisions r ON r.card_id=c.id WHERE c.archived_at IS NULL AND c.qq_selectable=1 AND r.revision=(SELECT max(revision) FROM character_card_revisions WHERE card_id=c.id)")?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut matches = vec![];
    for (id, revision, text) in rows {
        let card: Value = serde_json::from_str(&text)?;
        if id == selector {
            return Ok(CardRef {
                card_id: id,
                revision,
            });
        }
        if card["data"]["name"] == selector {
            matches.push(CardRef {
                card_id: id,
                revision,
            });
        }
    }
    if matches.len() == 1 {
        Ok(matches.remove(0))
    } else if matches.is_empty() {
        Err(CharacterError::missing())
    } else {
        Err(CharacterError::new(
            "ambiguous_character",
            "有多个同名角色，请使用角色ID",
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn database() -> Connection {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        db.execute_batch(include_str!("../schema.sql")).unwrap();
        migrate(&mut db).unwrap();
        db
    }
    fn conversation(db: &Connection, id: &str) {
        db.execute(
            "INSERT INTO conversations(id,owner_id,title,created_at) VALUES(?1,'admin','test',0)",
            [id],
        )
        .unwrap();
    }
    fn published(db: &Connection, name: &str, key: &str) -> (Draft, CardRef) {
        let draft = create(db, "admin", template(name, "说话简短")).unwrap();
        let value = publish(
            db,
            &draft.card_id,
            &draft.draft_id,
            "admin",
            &draft.draft_etag,
            key,
            &RuntimeConfig::default(),
        )
        .unwrap();
        (
            draft,
            CardRef {
                card_id: value["card_id"].as_str().unwrap().into(),
                revision: value["revision"].as_i64().unwrap(),
            },
        )
    }
    #[test]
    fn schema_rejects_unknown_fields_and_limits() {
        let mut card = template("卡", "");
        assert!(validate(&card).is_empty());
        card["data"]["extensions"]["dsh"]["secret"] = json!("not accepted");
        assert!(!validate(&card).is_empty());
        card = template("卡", &"字".repeat(8001));
        assert!(!validate(&card).is_empty());
    }
    #[test]
    fn publish_is_immutable_and_retry_is_idempotent() {
        let db = database();
        let (draft, r) = published(&db, "卡", "key");
        let retry = publish(
            &db,
            &draft.card_id,
            &draft.draft_id,
            "admin",
            &draft.draft_etag,
            "key",
            &RuntimeConfig::default(),
        )
        .unwrap();
        assert_eq!(retry["revision"], 1);
        assert!(db
            .execute(
                "UPDATE character_card_revisions SET digest='x' WHERE card_id=?1",
                [&r.card_id]
            )
            .is_err());
        let current = get_draft(&db, &draft.card_id, &draft.draft_id, "admin").unwrap();
        assert!(publish(
            &db,
            &draft.card_id,
            &draft.draft_id,
            "admin",
            &current.draft_etag,
            "key",
            &RuntimeConfig::default()
        )
        .is_err());
    }
    #[test]
    fn parallel_drafts_cannot_overwrite_published_head() {
        let db = database();
        let (_, r) = published(&db, "卡", "key");
        let a = from_revision(&db, &r.card_id, 1, "admin").unwrap();
        let b = from_revision(&db, &r.card_id, 1, "admin").unwrap();
        publish(
            &db,
            &a.card_id,
            &a.draft_id,
            "admin",
            &a.draft_etag,
            "second",
            &RuntimeConfig::default(),
        )
        .unwrap();
        assert_eq!(
            publish(
                &db,
                &b.card_id,
                &b.draft_id,
                "admin",
                &b.draft_etag,
                "third",
                &RuntimeConfig::default()
            )
            .unwrap_err()
            .status,
            StatusCode::CONFLICT
        );
    }
    #[test]
    fn default_does_not_change_existing_bindings_or_legacy() {
        let db = database();
        let (_, r) = published(&db, "旧角色", "old");
        let (_, next) = published(&db, "新角色", "new");
        let config = RuntimeConfig::default();
        conversation(&db, "legacy");
        conversation(&db, "bound");
        set_default(&db, &r, "none", &config).unwrap();
        bind_new_conversation(&db, "bound", None, &config).unwrap();
        let etag = default_state(&db).unwrap()["default_etag"]
            .as_str()
            .unwrap()
            .to_string();
        set_default(&db, &next, &etag, &config).unwrap();
        assert!(freeze_for_run(&db, "legacy", 1, &config, "用户")
            .unwrap()
            .is_none());
        assert_eq!(
            freeze_for_run(&db, "bound", 1, &config, "用户")
                .unwrap()
                .unwrap()
                .name,
            "旧角色"
        );
    }
    #[test]
    fn draft_test_is_frozen_and_export_has_only_card_fields() {
        let db = database();
        let draft = create(&db, "admin", template("测试卡", "原始人格")).unwrap();
        conversation(&db, "test");
        bind_test_conversation(
            &db,
            "test",
            &draft.card_id,
            &draft.draft_id,
            "admin",
            &draft.draft_etag,
            &RuntimeConfig::default(),
        )
        .unwrap();
        let mut updated = draft.card.clone();
        updated["data"]["system_prompt"] = json!("另一个人格");
        update_draft(
            &db,
            &draft.card_id,
            &draft.draft_id,
            "admin",
            &draft.draft_etag,
            updated,
        )
        .unwrap();
        let snapshot = freeze_for_run(&db, "test", 1, &RuntimeConfig::default(), "用户")
            .unwrap()
            .unwrap();
        assert!(
            snapshot.is_test
                && snapshot.system_prompt.contains("原始人格")
                && !snapshot.system_prompt.contains("另一个人格")
        );
        assert!(!draft.card.to_string().contains("credential_ref"));
    }
    #[test]
    fn macros_are_single_pass_and_unknown_macros_remain_literal() {
        assert_eq!(
            render_macros("{{char}} {{user}} {{file}}", "{{user}}", "甲"),
            "{{user}} 甲 {{file}}"
        );
    }
    #[test]
    fn archived_drafts_cannot_create_test_bindings_or_model_mappings() {
        let db = database();
        let draft = create(&db, "admin", template("归档卡", "")).unwrap();
        archive(&db, &draft.card_id, "0").unwrap();
        assert_eq!(
            from_revision(&db, &draft.card_id, 1, "admin")
                .unwrap_err()
                .code,
            "character_archived"
        );
        conversation(&db, "test");
        assert_eq!(
            bind_test_conversation(
                &db,
                "test",
                &draft.card_id,
                &draft.draft_id,
                "admin",
                &draft.draft_etag,
                &RuntimeConfig::default()
            )
            .unwrap_err()
            .code,
            "character_archived"
        );
        assert_eq!(
            map_draft_model(
                &db,
                &draft.card_id,
                &draft.draft_id,
                "admin",
                &draft.draft_etag,
                &RuntimeConfig::default()
            )
            .unwrap_err()
            .code,
            "character_archived"
        );
    }
    #[test]
    fn projection_digest_includes_rendered_macros_and_module_mapping() {
        let db = database();
        let draft = create(&db, "admin", template("卡", "称呼用户为{{user}}")).unwrap();
        let result = publish(
            &db,
            &draft.card_id,
            &draft.draft_id,
            "admin",
            &draft.draft_etag,
            "key",
            &RuntimeConfig::default(),
        )
        .unwrap();
        let reference = CardRef {
            card_id: draft.card_id,
            revision: result["revision"].as_i64().unwrap(),
        };
        conversation(&db, "bound");
        bind_new_conversation(&db, "bound", Some(&reference), &RuntimeConfig::default()).unwrap();
        let a = freeze_for_run(&db, "bound", 1, &RuntimeConfig::default(), "甲")
            .unwrap()
            .unwrap();
        let b = freeze_for_run(&db, "bound", 1, &RuntimeConfig::default(), "乙")
            .unwrap()
            .unwrap();
        assert_eq!(a.card_digest, b.card_digest);
        assert_ne!(a.projection_digest, b.projection_digest);
        let mut model = RuntimeConfig::default();
        model.model.model = "other-model".into();
        let c = freeze_for_run(&db, "bound", 2, &model, "甲")
            .unwrap()
            .unwrap();
        assert_ne!(a.projection_digest, c.projection_digest);
    }
    #[test]
    fn qq_roles_require_explicit_sharing() {
        let db = database();
        let (_, r) = published(&db, "公开卡", "key");
        assert!(list_published(&db).unwrap().is_empty());
        assert!(find_published(&db, "公开卡").is_err());
        set_qq_access(&db, &r.card_id, "1", true).unwrap();
        assert_eq!(list_published(&db).unwrap().len(), 1);
        assert_eq!(find_published(&db, "公开卡").unwrap(), r);
    }
}
