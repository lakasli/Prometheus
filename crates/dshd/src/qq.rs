//! QQ official gateway and webhook transports share durable event deduplication.
//! Protocol references are recorded in docs/m0-m1-validation.md by the project.
use crate::{
    character,
    config::RuntimeConfig,
    error::{ApiError, ApiResult},
    state::{now, AppState},
    worker::ReplyTarget,
};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier};
use futures_util::{SinkExt, StreamExt};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, protocol::WebSocketConfig, Message,
};
use tokio_util::sync::CancellationToken;

const QQ_BASE: &str = "https://api.bot.qq.com";
const QQ_TOKEN_BASE: &str = "https://api.bot.qq.com";
const USER_AGENT: &str = "DSH/0.1 QQ-Gateway";
const MAX_GATEWAY_MESSAGE: usize = 128 * 1024;

#[path = "qq_incoming_media.rs"]
pub mod incoming_media;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClosePolicy {
    pub clear_token: bool,
    pub clear_session: bool,
    pub terminal: bool,
    pub retry_delay_seconds: Option<u64>,
}

/// Platform close codes determine recovery; the untrusted reason is never logged.
pub fn close_policy(code: u16) -> ClosePolicy {
    ClosePolicy {
        clear_token: matches!(code, 4004 | 4006 | 4007 | 4009 | 4900..=4913),
        clear_session: matches!(code, 4006 | 4007 | 4009 | 4900..=4913),
        terminal: matches!(code, 4914 | 4915),
        retry_delay_seconds: (code == 4008).then_some(60),
    }
}

/// Text and binary UTF-8 JSON frames have identical handling and size limits.
pub fn parse_gateway_message(message: &Message) -> Result<Option<Value>, &'static str> {
    let data = match message {
        Message::Text(text) => text.as_bytes(),
        Message::Binary(bytes) => bytes.as_ref(),
        _ => return Ok(None),
    };
    if data.len() > MAX_GATEWAY_MESSAGE {
        return Err("QQ事件超过128KiB限制");
    }
    serde_json::from_slice(data)
        .map(Some)
        .map_err(|_| "QQ事件JSON无效")
}

#[derive(Debug)]
struct GatewayFailure {
    message: String,
    recovery: Option<ClosePolicy>,
}
impl From<&str> for GatewayFailure {
    fn from(message: &str) -> Self {
        Self {
            message: message.into(),
            recovery: None,
        }
    }
}
impl From<String> for GatewayFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            recovery: None,
        }
    }
}
fn closed(code: u16) -> GatewayFailure {
    tracing::warn!(
        channel = "qq",
        stage = "websocket_close",
        close_code = code,
        "QQ gateway closed"
    );
    GatewayFailure {
        message: format!("QQ网关关闭，代码{code}"),
        recovery: Some(close_policy(code)),
    }
}
fn websocket_failure(
    error: &tokio_tungstenite::tungstenite::Error,
    stage: &'static str,
) -> GatewayFailure {
    use tokio_tungstenite::tungstenite::Error;
    let class = match error {
        Error::Http(response) => {
            let status = response.status().as_u16();
            tracing::warn!(
                channel = "qq",
                stage,
                http_status = status,
                class = "http_rejected",
                "QQ WebSocket operation failed"
            );
            return format!("QQ WebSocket {stage}失败，HTTP {status}").into();
        }
        Error::Tls(_) => "tls",
        Error::Io(_) => "io",
        Error::Capacity(_) => "capacity",
        Error::Protocol(_) => "protocol",
        Error::Utf8 => "utf8",
        Error::ConnectionClosed | Error::AlreadyClosed => "closed",
        _ => "other",
    };
    tracing::warn!(
        channel = "qq",
        stage,
        class,
        "QQ WebSocket operation failed"
    );
    format!("QQ WebSocket {stage}失败，分类{class}").into()
}
fn http_failure(error: &reqwest::Error, stage: &'static str) -> String {
    let class = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connection_or_tls"
    } else if error.is_decode() {
        "decode"
    } else {
        "network"
    };
    tracing::warn!(channel = "qq", stage, class, "QQ HTTP operation failed");
    format!("QQ {stage}请求失败，分类{class}")
}
fn business_code(value: &Value) -> Option<i64> {
    ["code", "err_code", "retcode"].iter().find_map(|key| {
        value[*key]
            .as_i64()
            .or_else(|| value[*key].as_str().and_then(|v| v.parse().ok()))
    })
}
fn known_event_type(payload: &Value) -> Option<&'static str> {
    match payload["t"].as_str()? {
        "READY" => Some("READY"),
        "RESUMED" => Some("RESUMED"),
        "C2C_MESSAGE_CREATE" => Some("C2C_MESSAGE_CREATE"),
        "GROUP_AT_MESSAGE_CREATE" => Some("GROUP_AT_MESSAGE_CREATE"),
        "AT_MESSAGE_CREATE" => Some("AT_MESSAGE_CREATE"),
        "GROUP_MESSAGE_CREATE" => Some("GROUP_MESSAGE_CREATE"),
        "DIRECT_MESSAGE_CREATE" => Some("DIRECT_MESSAGE_CREATE"),
        _ => None,
    }
}

fn key(secret: &str) -> Result<SigningKey, String> {
    if secret.is_empty() {
        return Err("QQ凭证不可用".into());
    }
    let source = secret.as_bytes();
    let mut seed = [0u8; 32];
    for (i, b) in seed.iter_mut().enumerate() {
        *b = source[i % source.len()];
    }
    Ok(SigningKey::from_bytes(&seed))
}
pub fn verify_signature(secret: &str, timestamp: &str, signature: &str, body: &[u8]) -> bool {
    let Ok(key) = key(secret) else { return false };
    let Ok(bytes) = hex::decode(signature) else {
        return false;
    };
    let Ok(signature) = Signature::from_slice(&bytes) else {
        return false;
    };
    let mut content = timestamp.as_bytes().to_vec();
    content.extend_from_slice(body);
    key.verifying_key().verify(&content, &signature).is_ok()
}

pub async fn webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let (_, config) = state.current_config()?;
    if !config.qq.enabled || config.qq.transport != "webhook" {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "channel_disabled",
            "QQ Webhook未启用",
        ));
    }
    let reference = config
        .qq
        .credential_ref
        .as_ref()
        .ok_or_else(|| ApiError::forbidden("QQ凭证未设置"))?;
    let secret = state.secret(reference, "qq_app_secret")?;
    if let Some(appid) = headers.get("x-bot-appid").and_then(|h| h.to_str().ok()) {
        if appid != config.qq.app_id {
            return Err(ApiError::forbidden("QQ应用不匹配"));
        }
    }
    let payload: Value =
        serde_json::from_slice(&body).map_err(|_| ApiError::bad("invalid_event", "无效QQ事件"))?;
    // The current official SDK accepts URL validation before ordinary event signatures.
    if payload["op"] == 13 {
        let plain = payload["d"]["plain_token"]
            .as_str()
            .ok_or_else(|| ApiError::bad("invalid_challenge", "无效验证请求"))?;
        let event_ts = payload["d"]["event_ts"]
            .as_str()
            .ok_or_else(|| ApiError::bad("invalid_challenge", "无效验证请求"))?;
        // Restrict the challenge alphabet so this endpoint cannot sign arbitrary event JSON.
        if !(8..=256).contains(&plain.len())
            || !plain
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-+/=".contains(&b))
            || event_ts.is_empty()
            || event_ts.len() > 20
            || !event_ts.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(ApiError::bad(
                "invalid_challenge",
                "验证请求格式或长度不被允许",
            ));
        }
        let signature = key(&secret)
            .map_err(|_| ApiError::internal())?
            .sign(format!("{event_ts}{plain}").as_bytes());
        return Ok(Json(
            json!({"plain_token":plain,"signature":hex::encode(signature.to_bytes())}),
        ));
    }
    let timestamp = headers
        .get("x-signature-timestamp")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let signature = headers
        .get("x-signature-ed25519")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let stamp = timestamp
        .parse::<i64>()
        .map_err(|_| ApiError::forbidden("QQ签名时间戳无效"))?;
    if now().abs_diff(stamp) > 300 || !verify_signature(&secret, timestamp, signature, &body) {
        return Err(ApiError::forbidden("QQ签名校验失败或事件已过期"));
    }
    if payload["op"] == 0 {
        incoming_media::receive(&state, &config, &payload)?;
    }
    Ok(Json(json!({"op":12,"d":0})))
}

pub fn ingest(state: &AppState, config: &RuntimeConfig, payload: &Value) -> ApiResult<()> {
    let received_at = now();
    ingest_prepared(
        state,
        config,
        payload,
        received_at,
        incoming_media::reply_deadline(payload, received_at),
        incoming_media::Prepared::Text,
        None,
    )
}

pub fn ingest_prepared(
    state: &AppState,
    config: &RuntimeConfig,
    payload: &Value,
    received_at: i64,
    deadline: i64,
    prepared: incoming_media::Prepared,
    fence: Option<&incoming_media::Fence>,
) -> ApiResult<()> {
    let event_type = payload["t"].as_str().unwrap_or("");
    if ![
        "C2C_MESSAGE_CREATE",
        "GROUP_AT_MESSAGE_CREATE",
        "AT_MESSAGE_CREATE",
        "DIRECT_MESSAGE_CREATE",
    ]
    .contains(&event_type)
    {
        return Ok(());
    }
    let data = &payload["d"];
    if data["author"]["bot"].as_bool() == Some(true) {
        return Ok(());
    }
    let message_id = data["id"]
        .as_str()
        .ok_or_else(|| ApiError::bad("invalid_event", "QQ消息缺少ID"))?;
    let event_id = payload["id"].as_str().unwrap_or(message_id);
    let has_media = prepared.has_media();
    let media_notice = prepared.notice();
    let media_code = if let incoming_media::Prepared::Unavailable { code, .. } = &prepared {
        Some(*code)
    } else {
        None
    };
    let raw = data["content"].as_str().unwrap_or("");
    let mut content = strip_mentions(raw).trim().to_string();
    if content.is_empty() && has_media {
        content = match &prepared {
            incoming_media::Prepared::Image(_) => "请客观描述这张图片。",
            incoming_media::Prepared::Video(_) => "我发送了一个视频附件。",
            _ => "[收到不支持或不可用的媒体消息]",
        }
        .into();
    }
    if let incoming_media::Prepared::Video(video) = &prepared {
        content.push_str(&format!("\n[宿主附件说明：已接收一个MP4视频，时长{}毫秒，容器尺寸{}×{}，可按当前会话白名单原样回发；本阶段不读取视频帧或音轨，只能依据用户文字说明回答，不得声称看到了画面或听到了声音。]",video.duration_ms,video.width,video.height));
    }
    if content.is_empty() {
        return Ok(());
    }
    if content.len() > 16000 {
        return Ok(());
    }
    let (kind, target) = match event_type {
        "C2C_MESSAGE_CREATE" => ("c2c", data["author"]["user_openid"].as_str().unwrap_or("")),
        "GROUP_AT_MESSAGE_CREATE" => ("group", data["group_openid"].as_str().unwrap_or("")),
        "DIRECT_MESSAGE_CREATE" => ("dms", data["guild_id"].as_str().unwrap_or("")),
        _ => ("channel", data["channel_id"].as_str().unwrap_or("")),
    };
    if target.is_empty()
        || target.len() > 200
        || !target
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(ApiError::bad("invalid_event", "QQ目标标识无效"));
    }
    let account = &config.qq.app_id;
    let event_key = format!("qq:{account}:{event_type}:{event_id}");
    let source_key = format!("qq:{account}:{kind}:{target}");
    let owner = format!("qq:{account}:{kind}:{target}");
    let reply = ReplyTarget {
        kind: kind.into(),
        target: target.into(),
        message_id: message_id.into(),
        account_id: account.clone(),
        credential_ref: config
            .qq
            .credential_ref
            .clone()
            .ok_or_else(|| ApiError::bad("qq_unconfigured", "QQ凭证未设置"))?,
        // This deadline was frozen at durable receipt, before media download.
        deadline,
    };
    state.db(|db| {
        let tx=db.transaction()?;
        if let Some(fence)=fence{incoming_media::verify_fence_in(&tx,fence)?;}
        if tx.query_row("SELECT 1 FROM inbox WHERE event_key=?1",[&event_key],|r|r.get::<_,i64>(0)).optional()?.is_some(){if let Some(fence)=fence{incoming_media::finish_in(&tx,fence,"completed",media_code)?;}tx.commit()?;return Ok(());}
        // A retransmission after a persona switch still belongs to the original Run.
        if let Some(run)=tx.query_row("SELECT id FROM runs WHERE owner_id=?1 AND client_message_id=?2 ORDER BY rowid LIMIT 1",params![owner,message_id],|r|r.get::<_,String>(0)).optional()? {
            tx.execute("INSERT INTO inbox(event_key,account_id,event_type,received_at,run_id) VALUES(?1,?2,?3,?4,?5)",params![event_key,account,event_type,received_at,run])?;
            if let Some(fence)=fence{incoming_media::finish_in(&tx,fence,"completed",media_code)?;}
            tx.commit()?;return Ok(());
        }
        let existing=tx.query_row("SELECT id FROM conversations WHERE source_key=?1 AND owner_id=?2",params![source_key,owner],|r|r.get::<_,String>(0)).optional()?;
        let fresh=existing.is_none();
        let mut id=existing.unwrap_or_else(||uuid::Uuid::new_v4().to_string());
        let (config_revision,active_config)=character::current_config_in(&tx)?;
        if fence.is_some(){anyhow::ensure!(active_config.qq.enabled && active_config.qq.app_id==config.qq.app_id && active_config.qq.credential_ref==config.qq.credential_ref,"QQ receive account changed");}
        tx.execute("INSERT OR IGNORE INTO conversations(id,owner_id,title,source_key,created_at) VALUES(?1,?2,?3,?4,?5)",params![id,owner,format!("QQ {kind}"),source_key,now()])?;
        let command=if has_media{None}else{persona_command(&content)};
        let memory_command=!has_media && crate::memory_commands::is_command(&content);
        let has_binding=tx.query_row("SELECT 1 FROM conversation_character_bindings WHERE conversation_id=?1",[&id],|r|r.get::<_,i64>(0)).optional()?.is_some();
        let has_model_work=tx.query_row("SELECT 1 FROM runs WHERE conversation_id=?1 AND run_kind IN ('model','skill_text','skill_image') LIMIT 1",[&id],|r|r.get::<_,i64>(0)).optional()?.is_some();
        if command.is_none() && !memory_command && (fresh || (!has_binding && !has_model_work)) {character::bind_new_conversation(&tx,&id,None,&active_config)?;}
        let run=uuid::Uuid::new_v4().to_string();
        let command_response=if let Some(notice)=media_notice{Some(notice.to_string())}else if let Some(command)=command {
            if kind!="c2c" {Some("本助手仅使用一个性格，请在 Web 助手与模型中编辑。".to_string())}
            else {let(next,response)=handle_persona_command(&tx,&id,&source_key,&owner,command,&active_config)?;id=next;Some(response)}
        }else{None};
        let pending:i64=tx.query_row("SELECT count(*) FROM runs WHERE status='queued'",[],|r|r.get(0))?;
        let per_conversation:i64=tx.query_row("SELECT count(*) FROM runs WHERE conversation_id=?1 AND status IN ('queued','running')",[&id],|r|r.get(0))?;
        anyhow::ensure!(pending<500&&per_conversation<10,"queue full");
        // Local control replies must stay usable when a preferred model mapping expires.
        let snapshot=if command_response.is_some()||memory_command{None}else{character::freeze_for_run(&tx,&id,config_revision,&active_config,"用户")?};
        let status=if command_response.is_some(){"completed"}else{"queued"};
        let selected=if command_response.is_some()||memory_command||has_media {None}else{crate::skills::select(&tx,&content,None,false,false)?};
        let run_kind=if command_response.is_some(){if has_media{"media_notice"}else{"character_command"}}else if memory_command{"memory_command"}else if selected.is_some(){"skill_text"}else{"model"};
        tx.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,reply_json,status,response,created_at,updated_at,character_snapshot_json,character_digest,compiler_version,run_kind) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10,?11,?12,?13,?14)",params![run,id,owner,message_id,content,serde_json::to_string(&active_config)?,serde_json::to_string(&reply)?,status,command_response,now(),snapshot.as_ref().map(serde_json::to_string).transpose()?,snapshot.as_ref().map(|s|&s.projection_digest),snapshot.as_ref().map(|s|&s.compiler_version),run_kind])?;
        tx.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?1,?2,?3,'user',?4,?5)",params![uuid::Uuid::new_v4().to_string(),id,run,content,now()])?;
        match prepared {
        incoming_media::Prepared::Image(image)=>{
            let pending:i64=tx.query_row("SELECT count(*) FROM conversation_images WHERE image IS NOT NULL",[],|r|r.get(0))?;
            anyhow::ensure!(pending<20,"conversation image queue full");
            crate::media_assets::store_png_in(&tx,&owner,&id,&image)?;
            let source:String=tx.query_row("SELECT id FROM messages WHERE run_id=?1 AND role='user'",[&run],|r|r.get(0))?;
            tx.execute("INSERT INTO conversation_images(run_id,conversation_id,owner_id,source_message_id,width,height,image_sha256,image,status,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'queued',?9)",params![run,id,owner,source,image.width,image.height,image.sha256,image.bytes,received_at])?;
            tx.execute("INSERT OR IGNORE INTO dsh_schema_migrations(version,created_at) VALUES(8,?1)",[now()])?;
        },
        incoming_media::Prepared::Video(video)=>{crate::media_assets::store_video_in(&tx,&owner,&id,&video)?;},
        _=>{},
        }
        if let Some(selected)=selected {crate::skills::store(&tx,&run,&selected)?;}
        if let Some(response)=command_response {
            let pending:i64=tx.query_row("SELECT count(*) FROM outbox WHERE status IN ('queued','sending')",[],|r|r.get(0))?;
            anyhow::ensure!(pending<200,"reply queue full");
            tx.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?1,?2,?3,'assistant',?4,?5)",params![uuid::Uuid::new_v4().to_string(),id,run,response,now()])?;
            tx.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,deadline,created_at) VALUES(?1,?2,?3,?4,?5,?6,'queued',?7,?8)",params![uuid::Uuid::new_v4().to_string(),run,reply.account_id,reply.credential_ref,serde_json::to_string(&reply)?,response,reply.deadline,now()])?;
        }
        tx.execute("INSERT INTO inbox(event_key,account_id,event_type,received_at,run_id) VALUES(?1,?2,?3,?4,?5)",params![event_key,account,event_type,received_at,run])?;
        if let Some(fence)=fence{incoming_media::finish_in(&tx,fence,"completed",media_code)?;}
        tx.commit()?;Ok(())
    })?;
    state.0.notify.notify_waiters();
    Ok(())
}

fn persona_command(content: &str) -> Option<&str> {
    for prefix in ["/角色", "/persona"] {
        if let Some(rest) = content.strip_prefix(prefix) {
            if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                return Some(rest.trim());
            }
        }
    }
    None
}

fn handle_persona_command(
    tx: &rusqlite::Transaction<'_>,
    conversation: &str,
    _source_key: &str,
    _owner: &str,
    _command: &str,
    config: &RuntimeConfig,
) -> anyhow::Result<(String, String)> {
    let current = character::personality_state(tx, config)?;
    Ok((
        conversation.into(),
        format!(
            "当前助手：{}。本助手仅有一个性格，不再支持角色切换；请在 Web 助手与模型中编辑性格。",
            current["personality"]["name"].as_str().unwrap_or("DSH")
        ),
    ))
}

fn strip_mentions(input: &str) -> String {
    let mut rest = input;
    let mut output = String::new();
    while let Some(index) = rest.find("<@") {
        output.push_str(&rest[..index]);
        let after = &rest[index..];
        if let Some(end) = after.find('>') {
            rest = &after[end + 1..];
        } else {
            output.push_str(after);
            return output;
        }
    }
    output.push_str(rest);
    output
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(12))
        .connect_timeout(Duration::from_secs(8))
        .build()
        .map_err(|_| "QQ客户端初始化失败".into())
}
pub async fn access_token(state: &AppState, config: &RuntimeConfig) -> Result<String, String> {
    let reference = config
        .qq
        .credential_ref
        .as_ref()
        .ok_or("QQ AppSecret未设置")?;
    let identity = format!("{}:{reference}", config.qq.app_id);
    let mut cached = state.0.qq_token.lock().await;
    if let Some((token, expires, key)) = &*cached {
        if key == &identity && *expires > now() + 60 {
            return Ok(token.clone());
        }
    }
    let secret = state
        .secret(reference, "qq_app_secret")
        .map_err(|_| "QQ凭证不可用")?;
    let response = client()?
        .post(format!("{QQ_TOKEN_BASE}/app/getAppAccessToken"))
        .json(&json!({"appId":config.qq.app_id,"clientSecret":secret}))
        .send()
        .await
        .map_err(|error| http_failure(&error, "token"))?;
    tracing::info!(
        channel = "qq",
        stage = "token",
        http_status = response.status().as_u16(),
        "QQ token HTTP response"
    );
    if !response.status().is_success() {
        return Err(format!("QQ鉴权返回HTTP {}", response.status().as_u16()));
    }
    let value: Value = response.json().await.map_err(|_| "QQ鉴权返回无效JSON")?;
    if let Some(code) = business_code(&value) {
        tracing::info!(
            channel = "qq",
            stage = "token",
            business_code = code,
            "QQ token business status"
        );
    }
    let token = value["access_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("QQ鉴权失败；检查AppID、AppSecret和IP白名单")?
        .to_string();
    let ttl = value["expires_in"]
        .as_i64()
        .or_else(|| value["expires_in"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(300)
        .clamp(60, 7200);
    *cached = Some((token.clone(), now() + ttl, identity));
    Ok(token)
}

fn connection(state: &AppState, status: &str) {
    if let Ok(mut value) = state.0.qq_connection.lock() {
        if value.as_str() != status {
            tracing::info!(
                channel = "qq",
                connection_state = status,
                "QQ connection state changed"
            );
            *value = status.into();
        }
    }
}
pub async fn run_gateway(state: AppState, cancel: CancellationToken) {
    let mut failures = 0u32;
    let mut resume: Option<(String, i64, String)> = None;
    loop {
        if cancel.is_cancelled() {
            break;
        }
        let (revision, config) = match state.current_config() {
            Ok(value) => value,
            Err(_) => {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        if !config.qq.enabled || config.qq.transport != "websocket" {
            connection(
                &state,
                if config.qq.enabled {
                    "webhook"
                } else {
                    "disabled"
                },
            );
            tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(1))=>{}};
            continue;
        }
        let identity = format!(
            "{}:{}",
            config.qq.app_id,
            config.qq.credential_ref.as_deref().unwrap_or("")
        );
        if resume.as_ref().map(|r| r.2.as_str()) != Some(identity.as_str()) {
            resume = None;
        }
        connection(&state, "connecting");
        let outcome = gateway_session(&state, &config, &cancel, &identity, &mut resume).await;
        if cancel.is_cancelled() {
            break;
        }
        let mut retry_delay = None;
        match outcome {
            Ok(()) => {
                failures = 0;
                continue;
            }
            Err(error) => {
                // Only generated classifications and numeric statuses enter diagnostics.
                tracing::warn!(channel="qq",error=%error.message,"QQ gateway attempt failed");
                if let Ok(mut last) = state.0.qq_last_error.lock() {
                    *last = Some(error.message);
                }
                if let Some(policy) = error.recovery {
                    if policy.clear_token {
                        *state.0.qq_token.lock().await = None;
                    }
                    if policy.clear_session {
                        resume = None;
                    }
                    retry_delay = policy.retry_delay_seconds;
                    if policy.terminal {
                        connection(&state, "blocked");
                        // A terminal platform close requires an operator/configuration change.
                        loop {
                            tokio::select! {_=cancel.cancelled()=>return,_=tokio::time::sleep(Duration::from_secs(1))=>{}}
                            if state
                                .current_config()
                                .map(|(current, _)| current != revision)
                                .unwrap_or(false)
                            {
                                break;
                            }
                        }
                        failures = 0;
                        resume = None;
                        continue;
                    }
                }
                connection(&state, "backoff");
                failures = (failures + 1).min(6);
            }
        }
        let delay = retry_delay.unwrap_or_else(|| (1u64 << failures).min(60));
        tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(delay))=>{}}
    }
    connection(&state, "stopped");
}

async fn gateway_session(
    state: &AppState,
    config: &RuntimeConfig,
    cancel: &CancellationToken,
    identity: &str,
    resume: &mut Option<(String, i64, String)>,
) -> Result<(), GatewayFailure> {
    let token = access_token(state, config).await?;
    let response = client()?
        .get(format!("{QQ_BASE}/gateway"))
        .header("Authorization", format!("QQBot {token}"))
        .send()
        .await
        .map_err(|error| http_failure(&error, "gateway"))?;
    tracing::info!(
        channel = "qq",
        stage = "gateway",
        http_status = response.status().as_u16(),
        "QQ gateway HTTP response"
    );
    if !response.status().is_success() {
        return Err(format!("QQ网关查询被拒绝，HTTP {}", response.status().as_u16()).into());
    }
    let value: Value = response.json().await.map_err(|_| "QQ网关响应无效")?;
    if let Some(code) = business_code(&value) {
        tracing::info!(
            channel = "qq",
            stage = "gateway",
            business_code = code,
            "QQ gateway business status"
        );
    }
    let gateway = value["url"].as_str().ok_or("QQ网关未返回连接地址")?;
    let parsed = url::Url::parse(gateway).map_err(|_| "QQ网关地址无效")?;
    let hostname = parsed.host_str().unwrap_or("");
    if parsed.scheme() != "wss"
        || !(hostname == "api.bot.qq.com"
            || hostname.ends_with(".bot.qq.com")
            || hostname.ends_with(".sgroup.qq.com"))
    {
        return Err("QQ网关主机不被允许".into());
    }
    let mut request = gateway
        .into_client_request()
        .map_err(|_| GatewayFailure::from("QQ WebSocket升级请求无效"))?;
    request.headers_mut().insert(
        "user-agent",
        USER_AGENT
            .parse()
            .map_err(|_| GatewayFailure::from("QQ客户端标识无效"))?,
    );
    let socket_config = WebSocketConfig::default()
        .max_message_size(Some(MAX_GATEWAY_MESSAGE))
        .max_frame_size(Some(MAX_GATEWAY_MESSAGE));
    let (socket, upgrade) = tokio::time::timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async_with_config(request, Some(socket_config), false),
    )
    .await
    .map_err(|_| "QQ连接超时")?
    .map_err(|error| websocket_failure(&error, "upgrade"))?;
    tracing::info!(
        channel = "qq",
        stage = "websocket_upgrade",
        http_status = upgrade.status().as_u16(),
        "QQ WebSocket upgraded"
    );
    let (mut writer, mut reader) = socket.split();
    let hello = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let frame = reader
                .next()
                .await
                .ok_or_else(|| closed(1006))?
                .map_err(|error| websocket_failure(&error, "hello"))?;
            match frame {
                Message::Ping(bytes) => {
                    writer
                        .send(Message::Pong(bytes))
                        .await
                        .map_err(|error| websocket_failure(&error, "hello_pong"))?;
                }
                Message::Close(frame) => {
                    return Err(closed(frame.map(|f| u16::from(f.code)).unwrap_or(1006)))
                }
                data => {
                    if let Some(payload) = parse_gateway_message(&data)? {
                        return Ok(payload);
                    }
                }
            }
        }
    })
    .await
    .map_err(|_| "QQ握手超时")??;
    if hello["op"] != 10 {
        return Err("QQ握手opcode无效".into());
    }
    let interval = hello["d"]["heartbeat_interval"]
        .as_u64()
        .unwrap_or(45000)
        .clamp(1000, 120000);
    tracing::info!(
        channel = "qq",
        stage = "hello",
        heartbeat_interval_ms = interval,
        "QQ HELLO received"
    );
    let identify = if let Some((session, seq, _)) = resume {
        json!({"op":6,"d":{"token":format!("QQBot {token}"),"session_id":session,"seq":seq}})
    } else {
        json!({"op":2,"d":{"token":format!("QQBot {token}"),"intents":(1u64<<25)|(1u64<<30),"shard":[0,1],"properties":{"$os":"linux","$browser":"dsh","$device":"raspberry-pi"}}})
    };
    writer
        .send(Message::Text(identify.to_string().into()))
        .await
        .map_err(|error| websocket_failure(&error, "identify"))?;
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_millis(interval),
        Duration::from_millis(interval),
    );
    let mut config_check = tokio::time::interval(Duration::from_secs(1));
    let mut sequence: Option<i64> = resume.as_ref().map(|r| r.1);
    let mut awaiting_ack = false;
    let token_started = now();
    loop {
        tokio::select! {
            _=cancel.cancelled()=>{let _=writer.send(Message::Close(None)).await;return Ok(());},
            _=config_check.tick()=>{
                let (_,current)=state.current_config().map_err(|_|"无法读取QQ配置")?;
                if !current.qq.enabled||current.qq.transport!="websocket"||current.qq.app_id!=config.qq.app_id||current.qq.credential_ref!=config.qq.credential_ref {let _=writer.send(Message::Close(None)).await;*resume=None;return Ok(());}
                // Refresh access before it can expire. Reconnect resumes the same durable event stream.
                if now()-token_started>6500{return Err("QQ凭证更新重连".into());}
            },
            _=heartbeat.tick()=>{
                if awaiting_ack{return Err("QQ心跳未确认".into());}
                writer.send(Message::Text(json!({"op":1,"d":sequence}).to_string().into())).await.map_err(|_|"QQ心跳发送失败")?;awaiting_ack=true;
            },
            incoming=reader.next()=>{
                let incoming=incoming.ok_or_else(||closed(1006))?.map_err(|error|websocket_failure(&error,"read"))?;
                match incoming {
                    data @ (Message::Text(_) | Message::Binary(_))=>{
                        let payload=parse_gateway_message(&data)?.ok_or("QQ事件不是JSON数据帧")?;
                        if let Some(event_type)=known_event_type(&payload){tracing::info!(channel="qq",event_type,"QQ gateway event received");}
                        match payload["op"].as_i64().unwrap_or(-1) {
                            0=>{
                                if payload["t"]=="READY" {
                                    let session=payload["d"]["session_id"].as_str().ok_or("QQ缺少session_id")?.to_string();
                                    *resume=Some((session,payload["s"].as_i64().unwrap_or(0),identity.to_string()));connection(state,"connected");
                                } else if payload["t"]=="RESUMED" {connection(state,"connected");}
                                if payload["t"]=="READY"||payload["t"]=="RESUMED" {if let Ok(mut last)=state.0.qq_last_error.lock(){*last=None;}}
                                incoming_media::receive(state,config,&payload).map_err(|_|"QQ事件入库失败")?;
                                // Only advance the resume sequence after durable event handling succeeded.
                                if let Some(seq)=payload["s"].as_i64(){sequence=Some(seq);if let Some(r)=resume.as_mut(){r.1=seq;}}
                            },
                            7=>return Err("QQ请求重连".into()),
                            9=>{
                                let resumable=payload["d"].as_bool().unwrap_or(false);
                                tracing::warn!(channel="qq",opcode=9,resumable,"QQ session invalidated");
                                return Err(GatewayFailure{message:"QQ会话失效，opcode9".into(),recovery:Some(ClosePolicy{clear_token:!resumable,clear_session:!resumable,terminal:false,retry_delay_seconds:Some(3)})});
                            },
                            11=>awaiting_ack=false,
                            _=>{}
                        }
                    },
                    Message::Ping(bytes)=>{writer.send(Message::Pong(bytes)).await.map_err(|error|websocket_failure(&error,"pong"))?;},
                    Message::Close(frame)=>return Err(closed(frame.map(|f|u16::from(f.code)).unwrap_or(1006))),
                    _=>{}
                }
            }
        }
    }
}

pub async fn run_outbox(state: AppState, cancel: CancellationToken) {
    loop {
        if cancel.is_cancelled() {
            break;
        }
        let next=state.db(|db|{
            let tx=db.transaction()?;
            let item=tx.query_row("SELECT id,target_json,content FROM outbox WHERE status='queued' ORDER BY created_at LIMIT 1",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional()?;
            if let Some((id,_,_))=&item{tx.execute("UPDATE outbox SET status='sending' WHERE id=?1 AND status='queued'",[id])?;}
            tx.commit()?;Ok(item)
        });
        let Ok(Some((id, target, content))) = next else {
            tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_millis(500))=>{}};
            continue;
        };
        let target: ReplyTarget = match serde_json::from_str(&target) {
            Ok(t) => t,
            Err(_) => {
                let _ = finish_delivery(&state, &id, "failed", None, Some("无效目标记录"));
                continue;
            }
        };
        let result = send_qq(&state, &target, &content).await;
        match result {
            Ok(message_id) => {
                let _ = finish_delivery(&state, &id, "sent", Some(&message_id), None);
            }
            Err((unknown, error)) => {
                let _ = finish_delivery(
                    &state,
                    &id,
                    if unknown { "unknown" } else { "failed" },
                    None,
                    Some(&error),
                );
            }
        }
        // Account-wide outbound pacing; unknown results are never retried automatically.
        tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_millis(500))=>{}}
    }
}
fn finish_delivery(
    state: &AppState,
    id: &str,
    status: &str,
    message: Option<&str>,
    error: Option<&str>,
) -> ApiResult<()> {
    state.db(|db| {
        db.execute(
            "UPDATE outbox SET status=?1,platform_message_id=?2,error=?3 WHERE id=?4",
            params![status, message, error, id],
        )?;
        Ok(())
    })
}

pub async fn send_qq(
    state: &AppState,
    target: &ReplyTarget,
    content: &str,
) -> Result<String, (bool, String)> {
    send_qq_payload(state, target, &json!({"type":"text","text":content}), 1).await
}

/// Only host-validated text enters this path. Media bytes are resolved by the host,
/// never from a model-supplied path or URL.
pub async fn send_qq_payload(
    state: &AppState,
    target: &ReplyTarget,
    payload: &Value,
    seq: u32,
) -> Result<String, (bool, String)> {
    if payload["type"] != "text" {
        return Err((false, "QQ媒体回复需要宿主提供已授权的制品字节".into()));
    }
    let content = payload["text"].as_str().unwrap_or("");
    if content.trim().is_empty() || content.chars().count() > 1500 {
        return Err((false, "QQ文本回复须为1到1500字".into()));
    }
    let config = crate::qq_media::validate_target(state, target, seq)?;
    let body = if matches!(target.kind.as_str(), "channel" | "dms") {
        json!({"content":content,"msg_id":target.message_id})
    } else {
        json!({"content":content,"msg_type":0,"msg_id":target.message_id,"msg_seq":seq})
    };
    crate::qq_media::send_message(state, &config, target, &body).await
}

pub use crate::qq_media::send_media_bytes;
