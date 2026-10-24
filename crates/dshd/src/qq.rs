//! QQ official gateway and webhook transports share durable event deduplication.
//! Protocol references are recorded in docs/m0-m1-validation.md by the project.
use crate::{
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
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

const QQ_BASE: &str = "https://api.sgroup.qq.com";
const QQ_TOKEN_BASE: &str = "https://bots.qq.com";

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
        ingest(&state, &config, &payload)?;
    }
    Ok(Json(json!({"op":12,"d":0})))
}

pub fn ingest(state: &AppState, config: &RuntimeConfig, payload: &Value) -> ApiResult<()> {
    let event_type = payload["t"].as_str().unwrap_or("");
    if ![
        "C2C_MESSAGE_CREATE",
        "GROUP_AT_MESSAGE_CREATE",
        "AT_MESSAGE_CREATE",
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
    let Some(raw) = data["content"].as_str() else {
        return Ok(());
    };
    let content = strip_mentions(raw).trim().to_string();
    if content.is_empty() {
        return Ok(());
    }
    if content.len() > 16000 {
        return Ok(());
    }
    let (kind, target) = match event_type {
        "C2C_MESSAGE_CREATE" => ("c2c", data["author"]["user_openid"].as_str().unwrap_or("")),
        "GROUP_AT_MESSAGE_CREATE" => ("group", data["group_openid"].as_str().unwrap_or("")),
        _ => ("channel", data["channel_id"].as_str().unwrap_or("")),
    };
    if target.is_empty()
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
        deadline: now() + 120,
    };
    state.db(|db| {
        let tx=db.transaction()?;
        if tx.query_row("SELECT 1 FROM inbox WHERE event_key=?1",[&event_key],|r|r.get::<_,i64>(0)).optional()?.is_some(){return Ok(());}
        let existing=tx.query_row("SELECT id FROM conversations WHERE source_key=?1",[&source_key],|r|r.get::<_,String>(0)).optional()?;
        let id=existing.unwrap_or_else(||uuid::Uuid::new_v4().to_string());
        tx.execute("INSERT OR IGNORE INTO conversations(id,owner_id,title,source_key,created_at) VALUES(?1,?2,?3,?4,?5)",params![id,owner,format!("QQ {kind}"),source_key,now()])?;
        let pending:i64=tx.query_row("SELECT count(*) FROM runs WHERE status='queued'",[],|r|r.get(0))?;
        let per_conversation:i64=tx.query_row("SELECT count(*) FROM runs WHERE conversation_id=?1 AND status IN ('queued','running')",[&id],|r|r.get(0))?;
        anyhow::ensure!(pending<500&&per_conversation<10,"queue full");
        let existing_run=tx.query_row("SELECT id FROM runs WHERE conversation_id=?1 AND client_message_id=?2",params![id,message_id],|r|r.get::<_,String>(0)).optional()?;
        let run=existing_run.unwrap_or_else(||uuid::Uuid::new_v4().to_string());
        // Inbox, Run and user message share one transaction. A crash cannot ACK a lost Run.
        tx.execute("INSERT OR IGNORE INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,reply_json,status,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,'queued',?8,?8)",params![run,id,owner,message_id,content,serde_json::to_string(config)?,serde_json::to_string(&reply)?,now()])?;
        tx.execute("INSERT OR IGNORE INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?1,?2,?3,'user',?4,?5)",params![uuid::Uuid::new_v4().to_string(),id,run,content,now()])?;
        tx.execute("INSERT INTO inbox(event_key,account_id,event_type,received_at,run_id) VALUES(?1,?2,?3,?4,?5)",params![event_key,account,event_type,now(),run])?;
        tx.commit()?;Ok(())
    })?;
    state.0.notify.notify_waiters();
    Ok(())
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
        .map_err(|_| "QQ鉴权网络请求失败")?;
    if !response.status().is_success() {
        return Err(format!("QQ鉴权返回HTTP {}", response.status().as_u16()));
    }
    let value: Value = response.json().await.map_err(|_| "QQ鉴权返回无效JSON")?;
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
        *value = status.into();
    }
}
pub async fn run_gateway(state: AppState, cancel: CancellationToken) {
    let mut failures = 0u32;
    let mut resume: Option<(String, i64, String)> = None;
    loop {
        if cancel.is_cancelled() {
            break;
        }
        let config = match state.current_config() {
            Ok((_, c)) => c,
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
        match outcome {
            Ok(()) => {
                failures = 0;
                continue;
            }
            Err(error) => {
                if let Ok(mut last) = state.0.qq_last_error.lock() {
                    *last = Some(error);
                }
                connection(&state, "backoff");
                failures = (failures + 1).min(6);
            }
        }
        tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs((1u64<<failures).min(60)))=>{}}
    }
    connection(&state, "stopped");
}

async fn gateway_session(
    state: &AppState,
    config: &RuntimeConfig,
    cancel: &CancellationToken,
    identity: &str,
    resume: &mut Option<(String, i64, String)>,
) -> Result<(), String> {
    let token = access_token(state, config).await?;
    let response = client()?
        .get(format!("{QQ_BASE}/gateway"))
        .header("Authorization", format!("QQBot {token}"))
        .send()
        .await
        .map_err(|_| "QQ网关查询失败")?;
    if !response.status().is_success() {
        return Err("QQ网关查询被拒绝".into());
    }
    let value: Value = response.json().await.map_err(|_| "QQ网关响应无效")?;
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
    let (socket, _) = tokio::time::timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async(gateway),
    )
    .await
    .map_err(|_| "QQ连接超时")?
    .map_err(|_| "QQ连接失败")?;
    let (mut writer, mut reader) = socket.split();
    let hello = tokio::time::timeout(Duration::from_secs(15), reader.next())
        .await
        .map_err(|_| "QQ握手超时")?
        .ok_or("QQ握手连接关闭")?
        .map_err(|_| "QQ握手失败")?;
    let hello: Value = serde_json::from_str(hello.to_text().map_err(|_| "QQ握手非文本")?)
        .map_err(|_| "QQ握手JSON无效")?;
    if hello["op"] != 10 {
        return Err("QQ握手opcode无效".into());
    }
    let interval = hello["d"]["heartbeat_interval"]
        .as_u64()
        .unwrap_or(45000)
        .clamp(1000, 120000);
    let identify = if let Some((session, seq, _)) = resume {
        json!({"op":6,"d":{"token":format!("QQBot {token}"),"session_id":session,"seq":seq}})
    } else {
        json!({"op":2,"d":{"token":format!("QQBot {token}"),"intents":(1u64<<25)|(1u64<<30),"shard":[0,1],"properties":{"$os":"linux","$browser":"dsh","$device":"raspberry-pi"}}})
    };
    writer
        .send(Message::Text(identify.to_string().into()))
        .await
        .map_err(|_| "QQ鉴权发送失败")?;
    let mut heartbeat = tokio::time::interval(Duration::from_millis(interval));
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
                let incoming=incoming.ok_or("QQ连接关闭")?.map_err(|_|"QQ连接读取失败")?;
                match incoming {
                    Message::Text(text)=>{
                        if text.len()>128*1024{return Err("QQ事件超过限制".into());}
                        let payload:Value=serde_json::from_str(&text).map_err(|_|"QQ事件JSON无效")?;
                        match payload["op"].as_i64().unwrap_or(-1) {
                            0=>{
                                if payload["t"]=="READY" {
                                    let session=payload["d"]["session_id"].as_str().ok_or("QQ缺少session_id")?.to_string();
                                    *resume=Some((session,payload["s"].as_i64().unwrap_or(0),identity.to_string()));connection(state,"connected");
                                } else if payload["t"]=="RESUMED" {connection(state,"connected");}
                                if payload["t"]=="READY"||payload["t"]=="RESUMED" {if let Ok(mut last)=state.0.qq_last_error.lock(){*last=None;}}
                                ingest(state,config,&payload).map_err(|_|"QQ事件入库失败")?;
                                // Only advance the resume sequence after durable event handling succeeded.
                                if let Some(seq)=payload["s"].as_i64(){sequence=Some(seq);if let Some(r)=resume.as_mut(){r.1=seq;}}
                            },
                            7=>return Err("QQ请求重连".into()),
                            9=>{*resume=None;return Err("QQ会话失效".into());},
                            11=>awaiting_ack=false,
                            _=>{}
                        }
                    },
                    Message::Ping(bytes)=>{writer.send(Message::Pong(bytes)).await.map_err(|_|"QQ ping失败")?;},
                    Message::Close(_)=>return Err("QQ连接关闭".into()),
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

async fn send_qq(
    state: &AppState,
    target: &ReplyTarget,
    content: &str,
) -> Result<String, (bool, String)> {
    if target.deadline < now() {
        return Err((false, "回复截止时间已过".into()));
    }
    let (_, config) = state
        .current_config()
        .map_err(|_| (false, "无法核验QQ当前配置".into()))?;
    if !config.qq.enabled
        || config.qq.app_id != target.account_id
        || config.qq.credential_ref.as_deref() != Some(target.credential_ref.as_str())
    {
        return Err((false, "QQ账号配置已变更，拒绝替换目标账号或凭证发送".into()));
    }
    let token = access_token(state, &config).await.map_err(|e| (false, e))?;
    let path = match target.kind.as_str() {
        "c2c" => format!("/v2/users/{}/messages", target.target),
        "group" => format!("/v2/groups/{}/messages", target.target),
        "channel" => format!("/channels/{}/messages", target.target),
        _ => return Err((false, "未知QQ目标类型".into())),
    };
    // The protocol has platform-specific size limits; keep M1 text bounded.
    let content: String = content.chars().take(1500).collect();
    let body = if target.kind == "channel" {
        json!({"content":content,"msg_id":target.message_id})
    } else {
        json!({"content":content,"msg_type":0,"msg_id":target.message_id,"msg_seq":1})
    };
    let response = client()
        .map_err(|e| (false, e))?
        .post(format!("{QQ_BASE}{path}"))
        .header("Authorization", format!("QQBot {token}"))
        .header("X-Union-Appid", &target.account_id)
        .json(&body)
        .send()
        .await
        .map_err(|_| (true, "QQ发送网络中断，结果未知，未自动重发".into()))?;
    if !response.status().is_success() {
        return Err((
            false,
            format!("QQ发送返回HTTP {}", response.status().as_u16()),
        ));
    }
    let value: Value = response
        .json()
        .await
        .map_err(|_| (true, "QQ发送响应无效，结果未知".into()))?;
    value["id"]
        .as_str()
        .or_else(|| value["message"]["id"].as_str())
        .map(|s| s.to_string())
        .ok_or((true, "QQ响应无消息ID，结果未知".into()))
}
