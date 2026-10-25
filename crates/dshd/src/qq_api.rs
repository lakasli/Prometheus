//! Authenticated operator access to a finite QQ API catalog. This is deliberately
//! separate from the model tool bridge: chat content cannot select an operation.
use crate::{
    auth::Admin,
    error::{ApiError, ApiResult},
    qq_media::{self, PlatformReply},
    state::{now, AppState},
};
use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub title: String,
    pub method: String,
    pub path: String,
    pub parameters: Vec<String>,
    pub query_fields: Vec<String>,
    #[serde(default)]
    pub required_query_fields: Vec<String>,
    #[serde(default)]
    pub query_defaults: BTreeMap<String, String>,
    pub body_fields: Vec<String>,
    pub required_body_fields: Vec<String>,
    pub category: String,
    pub read_only: bool,
    pub deprecated: bool,
    pub documentation: String,
    pub implemented: bool,
    pub live_status: String,
}
#[derive(Deserialize)]
struct Catalog {
    operations: Vec<Operation>,
}
pub fn operations() -> &'static [Operation] {
    static ITEMS: OnceLock<Vec<Operation>> = OnceLock::new();
    ITEMS.get_or_init(|| {
        serde_json::from_str::<Catalog>(include_str!("../qq-api-catalog.json"))
            .expect("compiled QQ API catalog")
            .operations
    })
}
pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS qq_api_calls(id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,account_id TEXT NOT NULL,operation_id TEXT NOT NULL,outcome TEXT NOT NULL,http_status INTEGER,business_code INTEGER,trace_id TEXT,created_at INTEGER NOT NULL); CREATE INDEX IF NOT EXISTS qq_api_calls_lookup ON qq_api_calls(owner_id,account_id,operation_id,created_at); CREATE TABLE IF NOT EXISTS qq_api_objects(id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,account_id TEXT NOT NULL,credential_ref TEXT NOT NULL,value_json TEXT NOT NULL,expires_at INTEGER NOT NULL);")?;
    Ok(())
}
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/v1/qq/api/catalog", get(catalog))
        .route("/api/admin/v1/qq/api/operations/{id}", post(execute))
        .route("/api/admin/v1/qq/api/read-probe", post(read_probe))
        .route("/api/admin/v1/qq/api/calls", get(calls))
        .route("/api/admin/v1/qq/api/presigned-put", post(presigned_put))
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
}

async fn catalog(State(state): State<AppState>, admin: Admin) -> ApiResult<Json<Value>> {
    let (_, config) = state.current_config()?;
    let mut items = serde_json::to_value(operations()).map_err(|_| ApiError::internal())?;
    for item in items.as_array_mut().unwrap() {
        let id = item["id"].as_str().unwrap_or("");
        let latest = state.db(|db| Ok(db.query_row("SELECT outcome,http_status,business_code,created_at FROM qq_api_calls WHERE owner_id=?1 AND account_id=?2 AND operation_id=?3 ORDER BY rowid DESC LIMIT 1", params![admin.id,config.qq.app_id,id], |r| Ok(json!({"outcome":r.get::<_,String>(0)?,"http_status":r.get::<_,Option<i64>>(1)?,"business_code":r.get::<_,Option<i64>>(2)?,"at":r.get::<_,i64>(3)?}))).optional()?))?;
        item["last_call"] = latest.unwrap_or(Value::Null);
    }
    Ok(Json(
        json!({"source_site_version":"v1.42.0","checked_on":"2026-10-25","api_base":qq_media::QQ_API_BASE,"group_apis":false,"model_tool_access":false,"operation_count":operations().len(),"operations":items,"c2c_reply_limit":4,"c2c_reply_window_seconds":3600,"authentication":"server_stored_app_secret","presigned_put":true,"secrets_returned":false}),
    ))
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExecuteBody {
    pub parameters: BTreeMap<String, String>,
    pub query: BTreeMap<String, String>,
    pub body: Option<Value>,
    /// A dry run is always safe and never contacts QQ.
    pub dry_run: bool,
    /// Explicit operator intent, required for all external mutations.
    pub acknowledge_effects: bool,
}

pub fn build_request(
    op: &Operation,
    input: &ExecuteBody,
) -> ApiResult<(String, BTreeMap<String, String>)> {
    let expected: BTreeSet<_> = op.parameters.iter().map(String::as_str).collect();
    if input
        .parameters
        .keys()
        .any(|k| !expected.contains(k.as_str()))
    {
        return Err(ApiError::bad(
            "qq_parameters",
            "路径参数包含该接口未定义的字段",
        ));
    }
    let mut path = op.path.clone();
    for key in &op.parameters {
        let value = input
            .parameters
            .get(key)
            .ok_or_else(|| ApiError::bad("qq_parameters", format!("缺少路径参数{key}")))?;
        // Platform message IDs are opaque and may include ! and standard
        // base64 +/=. Encode them as one path segment instead of interpreting
        // a slash as a route delimiter. Pre-encoded paths and URLs are denied.
        if value.is_empty()
            || value.len() > 512
            || value.split('/').any(|part| matches!(part,"."|".."))
            || value.starts_with("//")
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-~!=+/".contains(&b))
        {
            return Err(ApiError::bad(
                "qq_parameters",
                format!("路径参数{key}格式无效"),
            ));
        }
        let mut segment=String::with_capacity(value.len());
        const HEX:&[u8;16]=b"0123456789ABCDEF";
        for byte in value.bytes(){
            if byte.is_ascii_alphanumeric()||b"_.-~".contains(&byte){segment.push(byte as char);}else{segment.push('%');segment.push(HEX[(byte>>4)as usize]as char);segment.push(HEX[(byte&15)as usize]as char);}
        }
        path = path.replace(&format!("{{{key}}}"), &segment);
    }
    for (key, value) in &input.query {
        if !op.query_fields.contains(key)
            || value.len() > 2048
            || value.chars().any(char::is_control)
        {
            return Err(ApiError::bad("qq_query", "查询参数不被该接口允许或超限"));
        }
        let maximum = if op.id == "get_v2_panels" || op.path.contains("/reactions/") {
            50
        } else {
            400
        };
        if key == "limit"
            && value
                .parse::<u32>()
                .map(|n| n == 0 || n > maximum)
                .unwrap_or(true)
        {
            return Err(ApiError::bad(
                "qq_query",
                format!("该接口分页数量须在1到{maximum}之间"),
            ));
        }
    }
    for required in &op.required_query_fields {
        if input
            .query
            .get(required)
            .is_none_or(|value| value.is_empty())
        {
            return Err(ApiError::bad(
                "qq_required_query",
                format!("缺少查询参数{required}"),
            ));
        }
    }
    if op.id == "get_v2_panels"
        && !input
            .query
            .get("scope")
            .is_some_and(|value| matches!(value.as_str(), "c2c" | "group" | "channel" | "dm"))
    {
        return Err(ApiError::bad(
            "qq_panel_scope",
            "指令面板scope须为c2c/group/channel/dm",
        ));
    }
    if input.body.as_ref().map(|b| !b.is_object()).unwrap_or(false) {
        return Err(ApiError::bad("qq_body", "请求体必须为JSON对象"));
    }
    if op.read_only && input.body.is_some() {
        return Err(ApiError::bad("qq_body", "只读接口不接受请求体"));
    }
    if let Some(body) = input.body.as_ref().and_then(Value::as_object) {
        if body.keys().any(|key| !op.body_fields.contains(key)) {
            return Err(ApiError::bad(
                "qq_body_fields",
                "请求体包含该接口未定义的字段",
            ));
        }
        if op
            .required_body_fields
            .iter()
            .any(|key| !body.contains_key(key))
        {
            return Err(ApiError::bad(
                "qq_body_required",
                "请求体缺少该接口必填字段",
            ));
        }
    } else if !op.required_body_fields.is_empty() {
        return Err(ApiError::bad(
            "qq_body_required",
            "该接口需要请求体及必填字段",
        ));
    }
    Ok((path, input.query.clone()))
}

async fn execute(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    Json(input): Json<ExecuteBody>,
) -> ApiResult<Json<Value>> {
    let op = operations()
        .iter()
        .find(|v| v.id == id)
        .ok_or_else(ApiError::not_found)?;
    execute_operation(&state, &admin, op, &input)
        .await
        .map(Json)
}

async fn execute_operation(
    state: &AppState,
    admin: &Admin,
    op: &Operation,
    input: &ExecuteBody,
) -> ApiResult<Value> {
    let (path, query) = build_request(op, input)?;
    if input.dry_run {
        return Ok(
            json!({"operation":op.id,"dry_run":true,"method":op.method,"path":path,"query":query,"body_fields":input.body.as_ref().and_then(Value::as_object).map(|v|v.keys().cloned().collect::<Vec<_>>()),"requires_effect_acknowledgement":!op.read_only,"deprecated":op.deprecated}),
        );
    }
    if !op.read_only && !input.acknowledge_effects {
        return Err(ApiError::bad(
            "qq_effects_not_acknowledged",
            "写入接口须明确勾选并提交本次操作确认",
        ));
    }
    if op.deprecated {
        return Err(ApiError::bad(
            "qq_deprecated",
            "该接口已被官方废弃，请使用替代接口",
        ));
    }
    let (_, config) = state.current_config()?;
    if !config.qq.enabled || config.qq.credential_ref.is_none() {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "qq_unavailable",
            "请先启用并配置QQ渠道",
        ));
    }
    let body = input
        .body
        .as_ref()
        .map(|b| {
            resolve_references(
                state,
                &admin.id,
                &config.qq.app_id,
                config.qq.credential_ref.as_deref().unwrap_or(""),
                b,
                0,
            )
        })
        .transpose()?;
    if let Some(body) = &body {
        validate_body(body, 0)?;
    }
    let method =
        reqwest::Method::from_bytes(op.method.as_bytes()).map_err(|_| ApiError::internal())?;
    let reply = qq_media::request(state, &config, method, &path, &query, body.as_ref()).await;
    match reply {
        Ok(reply) => {
            let outcome = if reply.accepted() {
                "platform_accepted"
            } else if reply.asynchronous() {
                "asynchronous_pending"
            } else if reply.uncertain() {
                "result_unknown"
            } else if matches!(reply.http_status, 401 | 403)
                || matches!(
                    reply.business_code,
                    Some(11253 | 11254 | 11264 | 11282 | 40034105)
                )
            {
                "platform_permission_denied"
            } else {
                "platform_rejected"
            };
            let call_id = record_call(
                state,
                &admin.id,
                &config.qq.app_id,
                &op.id,
                outcome,
                Some(&reply),
            )?;
            let object_ref = if contains_opaque(&reply.value, 0) {
                let id = uuid::Uuid::new_v4().to_string();
                state.db(|db|{db.execute("DELETE FROM qq_api_objects WHERE expires_at<?1",[now()])?;db.execute("INSERT INTO qq_api_objects(id,owner_id,account_id,credential_ref,value_json,expires_at) VALUES(?1,?2,?3,?4,?5,?6)",params![id,admin.id,config.qq.app_id,config.qq.credential_ref.as_deref().unwrap_or(""),serde_json::to_string(&reply.value)?,now()+3600])?;Ok(())})?;
                Some(id)
            } else {
                None
            };
            Ok(
                json!({"call_id":call_id,"operation":op.id,"outcome":outcome,"http_status":reply.http_status,"business_code":reply.business_code,"trace_id":reply.trace_id,"platform_accepted":reply.accepted(),"delivered_to_client":false,"response":redact(&reply.value,0),"object_ref":object_ref,"reference_syntax":{"$qq_ref":"object_ref","field":"file_info"}}),
            )
        }
        Err((unknown, _)) => {
            let outcome = if unknown {
                "result_unknown"
            } else {
                "transport_failed"
            };
            let call_id = record_call(state, &admin.id, &config.qq.app_id, &op.id, outcome, None)?;
            Ok(
                json!({"call_id":call_id,"operation":op.id,"outcome":outcome,"platform_accepted":false,"delivered_to_client":false,"automatic_retry":false}),
            )
        }
    }
}

fn record_call(
    state: &AppState,
    owner: &str,
    account: &str,
    operation: &str,
    outcome: &str,
    reply: Option<&PlatformReply>,
) -> ApiResult<String> {
    let id = uuid::Uuid::new_v4().to_string();
    state.db(|db|{db.execute("INSERT INTO qq_api_calls(id,owner_id,account_id,operation_id,outcome,http_status,business_code,trace_id,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![id,owner,account,operation,outcome,reply.map(|r|r.http_status),reply.and_then(|r|r.business_code),reply.and_then(|r|r.trace_id.as_deref()),now()])?;Ok(())})?;
    Ok(id)
}

fn validate_body(value: &Value, depth: usize) -> ApiResult<()> {
    if depth > 16 {
        return Err(ApiError::bad("qq_body", "请求体嵌套过深"));
    }
    match value {
        Value::Object(o) => {
            if o.len() > 100 {
                return Err(ApiError::bad("qq_body", "请求体字段过多"));
            }
            for (k, v) in o {
                if k.len() > 128 {
                    return Err(ApiError::bad("qq_body", "字段名称过长"));
                }
                validate_body(v, depth + 1)?;
            }
        }
        Value::Array(a) => {
            if a.len() > 400 {
                return Err(ApiError::bad("qq_body", "请求体数组过长"));
            }
            for v in a {
                validate_body(v, depth + 1)?;
            }
        }
        Value::String(s) if s.len() > 7 * 1024 * 1024 => {
            return Err(ApiError::bad("qq_body", "请求字段超过上限"))
        }
        _ => {}
    }
    Ok(())
}

fn resolve_references(
    state: &AppState,
    owner: &str,
    account: &str,
    credential: &str,
    value: &Value,
    depth: usize,
) -> ApiResult<Value> {
    if depth > 16 {
        return Err(ApiError::bad("qq_body", "请求体嵌套过深"));
    }
    match value {
        Value::Object(o) if o.contains_key("$qq_ref") => {
            if o.len() != 2 {
                return Err(ApiError::bad(
                    "qq_reference",
                    "不透明引用只能包含$qq_ref和field",
                ));
            }
            let id = o["$qq_ref"]
                .as_str()
                .ok_or_else(|| ApiError::bad("qq_reference", "引用ID无效"))?;
            let field = o
                .get("field")
                .and_then(Value::as_str)
                .filter(|s| s.len() <= 200)
                .ok_or_else(|| ApiError::bad("qq_reference", "引用字段无效"))?;
            let raw=state.db(|db|Ok(db.query_row("SELECT value_json FROM qq_api_objects WHERE id=?1 AND owner_id=?2 AND account_id=?3 AND credential_ref=?4 AND expires_at>?5",params![id,owner,account,credential,now()],|r|r.get::<_,String>(0)).optional()?))?.ok_or_else(ApiError::not_found)?;
            let data: Value = serde_json::from_str(&raw).map_err(|_| ApiError::internal())?;
            select_field(&data, field)
                .cloned()
                .ok_or_else(|| ApiError::bad("qq_reference", "引用字段不存在"))
        }
        Value::Object(o) => {
            let mut out = serde_json::Map::new();
            for (k, v) in o {
                out.insert(
                    k.clone(),
                    resolve_references(state, owner, account, credential, v, depth + 1)?,
                );
            }
            Ok(Value::Object(out))
        }
        Value::Array(a) => Ok(Value::Array(
            a.iter()
                .map(|v| resolve_references(state, owner, account, credential, v, depth + 1))
                .collect::<ApiResult<_>>()?,
        )),
        _ => Ok(value.clone()),
    }
}
fn select_field<'a>(value: &'a Value, field: &str) -> Option<&'a Value> {
    let mut v = value;
    for part in field.split('.') {
        v = if let Value::Array(a) = v {
            a.get(part.parse::<usize>().ok()?)?
        } else {
            v.get(part)?
        };
    }
    Some(v)
}
fn opaque_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "access_token"
            | "clientsecret"
            | "client_secret"
            | "token"
            | "secret"
            | "file_info"
            | "presigned_url"
            | "raw_url"
            | "preview_url"
            | "upload_id"
            | "url_link"
    )
}
fn contains_opaque(value: &Value, depth: usize) -> bool {
    if depth > 20 {
        return true;
    }
    match value {
        Value::Object(o) => o
            .iter()
            .any(|(k, v)| opaque_key(k) || contains_opaque(v, depth + 1)),
        Value::Array(a) => a.iter().any(|v| contains_opaque(v, depth + 1)),
        Value::String(s) => {
            s.starts_with("qqbot-media://") || s.starts_with("https://") && s.contains('?')
        }
        _ => false,
    }
}
fn redact(value: &Value, depth: usize) -> Value {
    if depth > 20 {
        return json!("[depth limit]");
    }
    match value {
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        if opaque_key(k) {
                            json!("[server-held reference]")
                        } else {
                            redact(v, depth + 1)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(|v| redact(v, depth + 1)).collect()),
        Value::String(s)
            if s.starts_with("qqbot-media://") || s.starts_with("https://") && s.contains('?') =>
        {
            json!("[server-held reference]")
        }
        _ => value.clone(),
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ProbeBody {
    parameters: BTreeMap<String, String>,
}
async fn read_probe(
    State(state): State<AppState>,
    admin: Admin,
    Json(body): Json<ProbeBody>,
) -> ApiResult<Json<Value>> {
    let mut results = Vec::new();
    for op in operations() {
        if !op.read_only {
            results.push(json!({"operation":op.id,"outcome":"not_executed_write"}));
            continue;
        }
        if op.deprecated {
            results.push(json!({"operation":op.id,"outcome":"deprecated_not_executed"}));
            continue;
        }
        let missing: Vec<_> = op
            .parameters
            .iter()
            .filter(|p| !body.parameters.contains_key(*p))
            .collect();
        if !missing.is_empty() {
            results.push(json!({"operation":op.id,"outcome":"requires_resource","missing_parameters":missing}));
            continue;
        }
        let parameters = op
            .parameters
            .iter()
            .map(|p| (p.clone(), body.parameters[p].clone()))
            .collect();
        let input = ExecuteBody {
            parameters,
            query: op.query_defaults.clone(),
            ..Default::default()
        };
        results.push(execute_operation(&state, &admin, op, &input).await?);
        // Avoid a burst of API calls and respect account-wide platform limits.
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
    }
    Ok(Json(
        json!({"results":results,"mutations_performed":false,"client_delivery_tested":false}),
    ))
}

async fn calls(State(state): State<AppState>, admin: Admin) -> ApiResult<Json<Value>> {
    let (_, config) = state.current_config()?;
    let items=state.db(|db|{let mut q=db.prepare("SELECT id,operation_id,outcome,http_status,business_code,trace_id,created_at FROM qq_api_calls WHERE owner_id=?1 AND account_id=?2 ORDER BY rowid DESC LIMIT 100")?;let rs=q.query_map(params![admin.id,config.qq.app_id],|r|Ok(json!({"id":r.get::<_,String>(0)?,"operation":r.get::<_,String>(1)?,"outcome":r.get::<_,String>(2)?,"http_status":r.get::<_,Option<i64>>(3)?,"business_code":r.get::<_,Option<i64>>(4)?,"trace_id":r.get::<_,Option<String>>(5)?,"at":r.get::<_,i64>(6)?})))?;Ok(rs.collect::<rusqlite::Result<Vec<_>>>()?)})?;
    Ok(Json(json!({"calls":items})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PutBody {
    object_ref: String,
    field: String,
    file_data: String,
    content_type: Option<String>,
}
async fn presigned_put(
    State(state): State<AppState>,
    admin: Admin,
    Json(input): Json<PutBody>,
) -> ApiResult<Json<Value>> {
    if input.file_data.len() > 7 * 1024 * 1024 {
        return Err(ApiError::bad("qq_upload_limit", "单次分片不得超过5MiB"));
    }
    let bytes = STANDARD
        .decode(&input.file_data)
        .map_err(|_| ApiError::bad("qq_file_data", "文件数据不是有效base64"))?;
    if bytes.is_empty() || bytes.len() > 5 * 1024 * 1024 {
        return Err(ApiError::bad("qq_upload_limit", "单次分片须为1字节到5MiB"));
    }
    let (_, config) = state.current_config()?;
    let credential = config
        .qq
        .credential_ref
        .as_deref()
        .ok_or_else(|| ApiError::bad("qq_unconfigured", "QQ未配置"))?;
    let raw=state.db(|db|Ok(db.query_row("SELECT value_json FROM qq_api_objects WHERE id=?1 AND owner_id=?2 AND account_id=?3 AND credential_ref=?4 AND expires_at>?5",params![input.object_ref,admin.id,config.qq.app_id,credential,now()],|r|r.get::<_,String>(0)).optional()?))?.ok_or_else(ApiError::not_found)?;
    let value: Value = serde_json::from_str(&raw).map_err(|_| ApiError::internal())?;
    if !(input.field == "presigned_url"
        || input.field.starts_with("parts.") && input.field.ends_with(".presigned_url"))
    {
        return Err(ApiError::bad(
            "qq_reference",
            "只能使用平台响应中的预签名上传字段",
        ));
    }
    let url = select_field(&value, &input.field)
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad("qq_reference", "预签名上传字段不存在"))?;
    let mime = input.content_type.as_deref();
    if mime
        .map(|s| {
            !matches!(
                s,
                "image/png"
                    | "image/jpeg"
                    | "image/gif"
                    | "image/webp"
                    | "application/octet-stream"
                    | "video/mp4"
                    | "audio/wav"
                    | "audio/mpeg"
                    | "audio/ogg"
            )
        })
        .unwrap_or(false)
    {
        return Err(ApiError::bad("qq_content_type", "上传内容类型不被允许"));
    }
    qq_media::put_presigned(url, &bytes, mime)
        .await
        .map_err(|(_, e)| {
            ApiError::new(StatusCode::BAD_GATEWAY, "qq_presigned_upload_failed", e)
        })?;
    Ok(Json(
        json!({"uploaded":true,"bytes":bytes.len(),"credentials_returned":false}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_is_finite_unique_and_excludes_groups() {
        let mut seen = BTreeSet::new();
        assert!(operations().len() >= 77);
        for op in operations() {
            assert!(seen.insert(op.id.clone()));
            assert!(!op.path.contains("/groups/"));
            assert!(op.path.starts_with('/'));
            assert_eq!(op.read_only, op.method == "GET");
            assert!(op.documentation.starts_with("https://bot.q.qq.com/wiki/"));
        }
    }
    #[test]
    fn path_injection_and_extra_query_are_rejected() {
        let op = operations()
            .iter()
            .find(|o| o.id == "get_channels_channel_id")
            .unwrap();
        let mut input = ExecuteBody {
            parameters: BTreeMap::from([("channel_id".into(), "123".into())]),
            ..Default::default()
        };
        assert_eq!(build_request(op, &input).unwrap().0, "/channels/123");
        input
            .parameters
            .insert("channel_id".into(), "123/../../users/@me".into());
        assert!(build_request(op, &input).is_err());
        input
            .parameters
            .insert("channel_id".into(), "123%2fmessages".into());
        assert!(build_request(op, &input).is_err());
        input.parameters.insert("channel_id".into(), "123".into());
        input
            .query
            .insert("url".into(), "https://attacker.invalid".into());
        assert!(build_request(op, &input).is_err());
    }

    #[test]
    fn panel_listing_requires_an_official_scope_and_its_documented_page_limit() {
        let op = operations()
            .iter()
            .find(|o| o.id == "get_v2_panels")
            .unwrap();
        let mut input = ExecuteBody::default();
        assert!(build_request(op, &input).is_err());
        input.query = op.query_defaults.clone();
        assert_eq!(build_request(op, &input).unwrap().1["scope"], "c2c");
        input.query.insert("scope".into(), "private".into());
        assert!(build_request(op, &input).is_err());
        input.query.insert("scope".into(), "c2c".into());
        input.query.insert("limit".into(), "51".into());
        assert!(build_request(op, &input).is_err());
        input.query.insert("limit".into(), "50".into());
        assert!(build_request(op, &input).is_ok());
    }
    #[test]
    fn opaque_media_credentials_are_redacted_recursively() {
        let value = json!({"file_info":"sensitive","parts":[{"presigned_url":"https://cos.qq.com/a?secret=1"}],"id":"public-id","url":"qqbot-media://image/v1/token"});
        let redacted = redact(&value, 0).to_string();
        assert!(!redacted.contains("sensitive"));
        assert!(!redacted.contains("secret=1"));
        assert!(!redacted.contains("/v1/token"));
        assert!(redacted.contains("public-id"));
    }
    #[test]
    fn every_operation_rejects_unlisted_body_fields() {
        for op in operations() {
            let parameters = op
                .parameters
                .iter()
                .map(|key| (key.clone(), "Synthetic_1".into()))
                .collect();
            let mut fields: serde_json::Map<String, Value> = op
                .required_body_fields
                .iter()
                .map(|key| (key.clone(), json!("synthetic")))
                .collect();
            fields.insert("unlisted_shell_command".into(), json!("never execute"));
            let input = ExecuteBody {
                parameters,
                body: Some(Value::Object(fields)),
                ..Default::default()
            };
            assert!(
                build_request(op, &input).is_err(),
                "{} must reject fields outside its schema",
                op.id
            );
        }
    }
    #[test]
    fn opaque_platform_references_require_owner_account_credential_and_lifetime() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = AppState::open(crate::state::Options {
            data_dir: dir.path().to_path_buf(),
            origin: "http://localhost".into(),
            allow_private_model: false,
            cookie_secure: false,
            web_dir: dir.path().to_path_buf(),
        })
        .unwrap();
        state.db(|db|{db.execute("INSERT INTO qq_api_objects(id,owner_id,account_id,credential_ref,value_json,expires_at) VALUES('private-id','owner-a','account-a','credential-a',?1,?2)",params![json!({"file_info":"opaque-synthetic-token"}).to_string(),now()+60])?;Ok(())}).unwrap();
        let reference = json!({"$qq_ref":"private-id","field":"file_info"});
        assert_eq!(
            resolve_references(
                &state,
                "owner-a",
                "account-a",
                "credential-a",
                &reference,
                0
            )
            .unwrap(),
            json!("opaque-synthetic-token")
        );
        assert!(resolve_references(
            &state,
            "owner-b",
            "account-a",
            "credential-a",
            &reference,
            0
        )
        .is_err());
        assert!(resolve_references(
            &state,
            "owner-a",
            "account-b",
            "credential-a",
            &reference,
            0
        )
        .is_err());
        assert!(resolve_references(
            &state,
            "owner-a",
            "account-a",
            "credential-b",
            &reference,
            0
        )
        .is_err());
        state
            .db(|db| {
                db.execute("UPDATE qq_api_objects SET expires_at=?1", [now() - 1])?;
                Ok(())
            })
            .unwrap();
        assert!(resolve_references(
            &state,
            "owner-a",
            "account-a",
            "credential-a",
            &reference,
            0
        )
        .is_err());
    }
}
