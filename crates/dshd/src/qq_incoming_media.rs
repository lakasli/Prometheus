//! Durable QQ reception before media fetch. ACK/resume advancement follows the
//! receipt transaction, while a single bounded consumer preserves receive order.
//! Signed CDN URLs exist only in the private pending row and are erased on exit.
use crate::{
    config::{public_ip, RuntimeConfig},
    error::{ApiError, ApiResult},
    image_analysis::{self, CanonicalImage},
    state::{now, AppState},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const MAX_PENDING: i64 = 200;
const MAX_PENDING_BYTES: i64 = 32 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 128 * 1024;
const TYPES: [&str; 4] = [
    "C2C_MESSAGE_CREATE",
    "GROUP_AT_MESSAGE_CREATE",
    "AT_MESSAGE_CREATE",
    "DIRECT_MESSAGE_CREATE",
];

pub enum Prepared {
    Text,
    Image(CanonicalImage),
    Unavailable {
        code: &'static str,
        message: &'static str,
    },
}
impl Prepared {
    pub fn has_media(&self) -> bool {
        !matches!(self, Self::Text)
    }
    pub fn notice(&self) -> Option<&'static str> {
        if let Self::Unavailable { message, .. } = self {
            Some(*message)
        } else {
            None
        }
    }
}
#[derive(Clone, Debug)]
pub struct Fence {
    pub event_key: String,
    pub generation: i64,
}
struct Receipt {
    fence: Fence,
    config: RuntimeConfig,
    payload: Value,
    received_at: i64,
    deadline: i64,
}

pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS qq_received_events(event_key TEXT PRIMARY KEY,account_id TEXT NOT NULL,event_type TEXT NOT NULL,message_id TEXT NOT NULL,credential_ref TEXT NOT NULL,config_json TEXT,payload_json TEXT,status TEXT NOT NULL,generation INTEGER NOT NULL DEFAULT 0,received_at INTEGER NOT NULL,deadline INTEGER NOT NULL,error_code TEXT,finished_at INTEGER,UNIQUE(account_id,event_type,message_id));CREATE INDEX IF NOT EXISTS qq_received_queue ON qq_received_events(status,received_at);")?;
    // A fetch has no outbound side effect. Durable Inbox dedup prevents a second
    // Run if the previous consumer committed immediately before a process crash.
    db.execute("UPDATE qq_received_events SET status='queued',generation=generation+1 WHERE status='processing'",[])?;
    prune_in(db)?;
    Ok(())
}
pub fn prune_in(db: &Connection) -> anyhow::Result<()> {
    db.execute("UPDATE qq_received_events SET status='expired',payload_json=NULL,config_json=NULL,error_code='reply_deadline_expired',finished_at=?1,generation=generation+1 WHERE status IN ('queued','processing') AND deadline<?1",[now()])?;
    // These are short-lived transport receipts, not conversations or memory.
    db.execute("DELETE FROM qq_received_events WHERE payload_json IS NULL AND coalesce(finished_at,received_at)<?1",[now()-86400])?;
    Ok(())
}
pub fn reply_deadline(payload: &Value, received_at: i64) -> i64 {
    let at = payload["d"]["timestamp"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.timestamp())
        .unwrap_or(received_at)
        .min(received_at);
    at.saturating_add(if payload["t"] == "C2C_MESSAGE_CREATE" {
        3600
    } else {
        300
    })
}

/// Called only after a signed Webhook or authenticated Gateway event. The
/// caller advances/ACKs its event cursor only if this transaction succeeded.
pub fn receive(state: &AppState, config: &RuntimeConfig, payload: &Value) -> ApiResult<()> {
    let event_type = payload["t"].as_str().unwrap_or("");
    if !TYPES.contains(&event_type) || payload["d"]["author"]["bot"].as_bool() == Some(true) {
        return Ok(());
    }
    let message = payload["d"]["id"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 512 && !s.contains('\0'))
        .ok_or_else(|| ApiError::bad("invalid_event", "QQ消息ID无效"))?;
    let event = payload["id"].as_str().unwrap_or(message);
    if event.len() > 512 || event.contains('\0') {
        return Err(ApiError::bad("invalid_event", "QQ事件ID无效"));
    }
    let raw = serde_json::to_string(payload)
        .map_err(|_| ApiError::bad("invalid_event", "QQ事件无法保存"))?;
    if raw.len() > MAX_EVENT_BYTES {
        return Err(ApiError::bad("event_too_large", "QQ事件超过128KiB"));
    }
    let config_json = serde_json::to_string(config).map_err(|_| ApiError::internal())?;
    let received = now();
    let deadline = reply_deadline(payload, received);
    let key = format!("qq:{}:{event_type}:{event}", config.qq.app_id);
    let credential = config
        .qq
        .credential_ref
        .as_deref()
        .ok_or_else(|| ApiError::bad("qq_unconfigured", "QQ凭据未配置"))?;
    let queued=state.db(|db|{
        let tx=db.transaction()?;
        prune_in(&tx)?;
        let duplicate:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM qq_received_events WHERE event_key=?1 OR (account_id=?2 AND event_type=?3 AND message_id=?4)) OR EXISTS(SELECT 1 FROM inbox WHERE event_key=?1)",params![key,config.qq.app_id,event_type,message],|r|r.get(0))?;
        if duplicate{tx.commit()?;return Ok(true);}
        let (count,total):(i64,i64)=tx.query_row("SELECT count(*),coalesce(sum(length(payload_json)+length(config_json)),0) FROM qq_received_events WHERE status IN ('queued','processing')",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if count>=MAX_PENDING||total+(raw.len()+config_json.len())as i64>MAX_PENDING_BYTES{return Ok(false);}
        crate::chat_reply::require_reader(&tx,9)?;
        tx.execute("INSERT INTO qq_received_events(event_key,account_id,event_type,message_id,credential_ref,config_json,payload_json,status,received_at,deadline) VALUES(?1,?2,?3,?4,?5,?6,?7,'queued',?8,?9)",params![key,config.qq.app_id,event_type,message,credential,config_json,raw,received,deadline])?;
        tx.commit()?;Ok(true)
    })?;
    if !queued {
        return Err(ApiError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "qq_receive_queue_full",
            "QQ接收队列暂满，事件尚未确认",
        ));
    }
    state.0.notify.notify_waiters();
    Ok(())
}

fn claim(state: &AppState) -> ApiResult<Option<Receipt>> {
    state.db(|db|{
        let tx=db.transaction()?;
        prune_in(&tx)?;
        let row=tx.query_row("SELECT event_key,generation,config_json,payload_json,received_at,deadline FROM qq_received_events WHERE status='queued' ORDER BY received_at,rowid LIMIT 1",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?,r.get::<_,i64>(5)?))).optional()?;
        let Some((key,generation,config,payload,received_at,deadline))=row else{tx.commit()?;return Ok(None);};
        let config:RuntimeConfig=serde_json::from_str(&config)?;
        let payload:Value=serde_json::from_str(&payload)?;
        let generation=generation+1;
        tx.execute("UPDATE qq_received_events SET status='processing',generation=?1 WHERE event_key=?2 AND status='queued'",params![generation,key])?;
        tx.commit()?;
        Ok(Some(Receipt{fence:Fence{event_key:key,generation},config,payload,received_at,deadline}))
    })
}
pub fn verify_fence_in(db: &Connection, fence: &Fence) -> anyhow::Result<()> {
    let valid:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM qq_received_events WHERE event_key=?1 AND generation=?2 AND status='processing' AND deadline>=?3)",params![fence.event_key,fence.generation,now()],|r|r.get(0))?;
    anyhow::ensure!(valid, "QQ receive lease expired or replaced");
    Ok(())
}
pub fn finish_in(
    db: &Connection,
    fence: &Fence,
    status: &str,
    code: Option<&str>,
) -> anyhow::Result<()> {
    let changed=db.execute("UPDATE qq_received_events SET status=?1,error_code=?2,payload_json=NULL,config_json=NULL,finished_at=?3 WHERE event_key=?4 AND generation=?5 AND status='processing'",params![status,code,now(),fence.event_key,fence.generation])?;
    anyhow::ensure!(changed == 1, "QQ receive lease no longer held");
    Ok(())
}
async fn process(state: &AppState, receipt: &Receipt) -> ApiResult<()> {
    let (_, active) = state.current_config()?;
    if !active.qq.enabled
        || active.qq.app_id != receipt.config.qq.app_id
        || active.qq.credential_ref != receipt.config.qq.credential_ref
    {
        return state.db(|db| {
            finish_in(
                db,
                &receipt.fence,
                "cancelled",
                Some("account_or_credential_changed"),
            )
        });
    }
    let data = &receipt.payload["d"];
    let (kind, target) = match receipt.payload["t"].as_str().unwrap_or("") {
        "C2C_MESSAGE_CREATE" => ("c2c", data["author"]["user_openid"].as_str().unwrap_or("")),
        "GROUP_AT_MESSAGE_CREATE" => ("group", data["group_openid"].as_str().unwrap_or("")),
        "DIRECT_MESSAGE_CREATE" => ("dms", data["guild_id"].as_str().unwrap_or("")),
        _ => ("channel", data["channel_id"].as_str().unwrap_or("")),
    };
    let owner = format!("qq:{}:{kind}:{target}", receipt.config.qq.app_id);
    let duplicate=state.db(|db|Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM inbox WHERE event_key=?1) OR EXISTS(SELECT 1 FROM runs WHERE owner_id=?2 AND client_message_id=?3)",params![receipt.fence.event_key,owner,data["id"].as_str().unwrap_or("")],|r|r.get::<_,bool>(0))?))?;
    if duplicate {
        return state.db(|db| finish_in(db, &receipt.fence, "completed", None));
    }
    let has_media = receipt.payload["d"]["attachments"]
        .as_array()
        .is_some_and(|a| !a.is_empty());
    let available = crate::conversation_images::capabilities(state)?.available;
    let prepared = if has_media && !available {
        Prepared::Unavailable {
            code: "vision_provider_unavailable",
            message: "图片理解暂未配置可用的视觉模型，尚未查看图片，请改用文字。",
        }
    } else {
        prepare(&receipt.payload).await
    };
    if receipt.deadline < now() {
        return state.db(|db| {
            finish_in(
                db,
                &receipt.fence,
                "expired",
                Some("reply_deadline_expired"),
            )
        });
    }
    crate::qq::ingest_prepared(
        state,
        &receipt.config,
        &receipt.payload,
        receipt.received_at,
        receipt.deadline,
        prepared,
        Some(&receipt.fence),
    )?;
    // Ignored bot/empty events create no Inbox row; still release their receipt.
    state.db(|db|{let held:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM qq_received_events WHERE event_key=?1 AND generation=?2 AND status='processing')",params![receipt.fence.event_key,receipt.fence.generation],|r|r.get(0))?;if held{finish_in(db,&receipt.fence,"completed",None)?;}Ok(())})
}
pub async fn run(state: AppState, cancel: CancellationToken) {
    while !cancel.is_cancelled() {
        let next = claim(&state);
        let Ok(Some(receipt)) = next else {
            tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_millis(250))=>{}}
            continue;
        };
        let result = tokio::select! {_=cancel.cancelled()=>break,result=tokio::time::timeout(Duration::from_secs(20),process(&state,&receipt))=>result};
        match result {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                let _ =
                    state.db(|db| finish_in(db, &receipt.fence, "failed", Some("ingest_failed")));
                tracing::warn!(
                    channel = "qq",
                    stage = "receive_ingest",
                    class = "failed",
                    "QQ received event failed"
                );
            }
            Err(_) => {
                let _ = state.db(|db| {
                    finish_in(db, &receipt.fence, "failed", Some("media_prepare_timeout"))
                });
                tracing::warn!(
                    channel = "qq",
                    stage = "receive_media",
                    class = "timeout",
                    "QQ received media timed out"
                );
            }
        }
    }
}

pub async fn prepare(payload: &Value) -> Prepared {
    let Some(attachments) = payload["d"]["attachments"]
        .as_array()
        .filter(|a| !a.is_empty())
    else {
        return Prepared::Text;
    };
    let unavailable = |code, message| Prepared::Unavailable { code, message };
    if payload["t"] != "C2C_MESSAGE_CREATE" {
        return unavailable(
            "media_channel_unsupported",
            "目前仅支持QQ单聊的单张PNG图片理解，请使用文字描述。",
        );
    }
    if attachments.len() != 1 {
        return unavailable(
            "media_count_unsupported",
            "目前每条消息只支持一张PNG图片，请分别发送。",
        );
    }
    if payload["d"]["content"]
        .as_str()
        .unwrap_or("")
        .trim_start()
        .starts_with('/')
    {
        return unavailable(
            "media_control_command",
            "图片消息暂不执行控制命令，请把命令单独作为文字发送。",
        );
    }
    let attachment = &attachments[0];
    if attachment["content_type"]
        .as_str()
        .is_some_and(|s| s != "image/png")
    {
        return unavailable(
            "media_format_unsupported",
            "目前图片理解只支持PNG，视频和语音暂不能作为输入理解，请发送PNG或文字。",
        );
    }
    if attachment["size"]
        .as_u64()
        .is_some_and(|n| n > image_analysis::MAX_IMAGE_BYTES as u64)
    {
        return unavailable(
            "media_size_limit",
            "图片超过1MiB，请压缩为一张较小的PNG后重试。",
        );
    }
    let Some(raw) = attachment["url"].as_str().filter(|s| s.len() <= 8192) else {
        return unavailable(
            "media_url_missing",
            "QQ未提供可用的图片下载引用，请重新发送PNG图片。",
        );
    };
    let bytes = match download(raw).await {
        Ok(bytes) => bytes,
        Err(code) => {
            return unavailable(
                code,
                "图片下载未完成，尚未查看图片，请重新发送PNG或改用文字。",
            )
        }
    };
    let permit = match crate::image_analysis_api::DECODE_SLOTS.acquire().await {
        Ok(p) => p,
        Err(_) => {
            return unavailable(
                "media_decoder_unavailable",
                "图片校验暂不可用，请稍后重试。",
            )
        }
    };
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        image_analysis::canonicalize(&STANDARD.encode(bytes))
    })
    .await;
    match result {
        Ok(Ok(image)) => Prepared::Image(image),
        _ => unavailable(
            "media_png_invalid",
            "图片格式或尺寸未通过校验，尚未查看图片，请发送1MiB以内的PNG。",
        ),
    }
}

pub fn checked_cdn_url(raw: &str) -> Result<url::Url, &'static str> {
    let normalized = if raw.starts_with("//") {
        format!("https:{raw}")
    } else {
        raw.to_string()
    };
    let url = url::Url::parse(&normalized).map_err(|_| "media_url_invalid")?;
    let host = url.host_str().unwrap_or("");
    let allowed = matches!(
        host,
        "multimedia.nt.qq.com.cn" | "gchat.qpic.cn" | "c2cpicdw.qpic.cn" | "p.qpic.cn"
    ) || host.ends_with(".myqcloud.com") && host.starts_with("gchat-");
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !allowed
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("media_url_not_allowed");
    }
    Ok(url)
}
async fn download(raw: &str) -> Result<Vec<u8>, &'static str> {
    let url = checked_cdn_url(raw)?;
    let host = url.host_str().ok_or("media_url_invalid")?.to_string();
    let addresses: Vec<_> = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::lookup_host((host.as_str(), 443)),
    )
    .await
    .map_err(|_| "media_dns_timeout")?
    .map_err(|_| "media_dns_failed")?
    .collect();
    if addresses.is_empty() || addresses.iter().any(|a| !public_ip(a.ip())) {
        return Err("media_dns_private");
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8))
        .resolve_to_addrs(&host, &addresses)
        .build()
        .map_err(|_| "media_client_failed")?;
    let mut reply = client
        .get(url)
        .send()
        .await
        .map_err(|_| "media_download_failed")?;
    if !reply.status().is_success() {
        return Err("media_download_http");
    }
    if reply
        .content_length()
        .is_some_and(|n| n > image_analysis::MAX_IMAGE_BYTES as u64)
    {
        return Err("media_size_limit");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = reply
        .chunk()
        .await
        .map_err(|_| "media_download_incomplete")?
    {
        if bytes.len() + chunk.len() > image_analysis::MAX_IMAGE_BYTES {
            return Err("media_size_limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn state(dir: &tempfile::TempDir) -> AppState {
        AppState::open(crate::state::Options {
            data_dir: dir.path().into(),
            origin: "http://localhost".into(),
            allow_private_model: true,
            cookie_secure: false,
            web_dir: dir.path().into(),
        })
        .unwrap()
    }
    #[test]
    fn cdn_allowlist_rejects_credentials_redirect_hosts_private_urls_and_spoofed_suffixes() {
        assert!(checked_cdn_url("https://multimedia.nt.qq.com.cn/download?rkey=opaque").is_ok());
        for url in [
            "http://multimedia.nt.qq.com.cn/a",
            "https://multimedia.nt.qq.com.cn.attacker.invalid/a",
            "https://127.0.0.1/a",
            "https://gchat.qpic.cn@attacker.invalid/a",
            "https://u:p@gchat.qpic.cn/a",
            "https://gchat.qpic.cn:8443/a",
            "file:///etc/passwd",
        ] {
            assert!(checked_cdn_url(url).is_err(), "{url}");
        }
    }
    #[test]
    fn received_timestamp_bounds_the_deadline_and_never_extends_it_after_download() {
        let p = json!({"t":"C2C_MESSAGE_CREATE","d":{"timestamp":"2026-10-25T00:00:00Z"}});
        let sent = chrono::DateTime::parse_from_rfc3339("2026-10-25T00:00:00Z")
            .unwrap()
            .timestamp();
        assert_eq!(reply_deadline(&p, sent + 120), sent + 3600);
        assert_eq!(
            reply_deadline(&json!({"t":"AT_MESSAGE_CREATE"}), sent),
            sent + 300
        );
    }
    #[test]
    fn receive_dedup_uses_ids_not_text_and_restart_fences_the_old_consumer() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = state(&dir);
        initialize(&state.0.db.lock().unwrap()).unwrap();
        let mut config = RuntimeConfig::default();
        config.qq.enabled = true;
        config.qq.app_id = "testapp".into();
        config.qq.credential_ref = Some("testkey".into());
        let mut payload = json!({"t":"C2C_MESSAGE_CREATE","id":"event1","d":{"id":"message1","content":"same text","author":{"user_openid":"target"}}});
        receive(&state, &config, &payload).unwrap();
        receive(&state, &config, &payload).unwrap();
        payload["id"] = json!("event2");
        payload["d"]["id"] = json!("message2");
        receive(&state, &config, &payload).unwrap();
        let count = state
            .db(|db| {
                Ok(
                    db.query_row("SELECT count(*) FROM qq_received_events", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(count, 2);
        let first = claim(&state).unwrap().unwrap();
        assert!(first.fence.event_key.ends_with(":event1"));
        state
            .db(|db| {
                initialize(db)?;
                Ok(())
            })
            .unwrap();
        assert!(state.db(|db| verify_fence_in(db, &first.fence)).is_err());
        let next = claim(&state).unwrap().unwrap();
        assert!(next.fence.event_key.ends_with(":event1"));
        state
            .db(|db| finish_in(db, &next.fence, "failed", Some("synthetic_test")))
            .unwrap();
        let cleared=state.db(|db|Ok(db.query_row("SELECT payload_json IS NULL AND config_json IS NULL FROM qq_received_events WHERE event_key=?1",[next.fence.event_key],|r|r.get::<_,bool>(0))?)).unwrap();
        assert!(cleared);
    }
    #[test]
    fn prepared_png_and_reusable_asset_commit_with_the_same_private_inbox_source() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = state(&dir);
        let mut config = RuntimeConfig::default();
        config.qq.enabled = true;
        config.qq.app_id = "testapp".into();
        config.qq.credential_ref = Some("testkey".into());
        state
            .db(|db| {
                db.execute(
                    "UPDATE config_revisions SET config_json=?1 WHERE revision=1",
                    [serde_json::to_string(&config)?],
                )?;
                Ok(())
            })
            .unwrap();
        let payload = json!({"t":"C2C_MESSAGE_CREATE","id":"image-event","d":{"id":"image-message","content":"请描述这张合成图片。","author":{"user_openid":"target"},"attachments":[{"content_type":"image/png","url":"https://multimedia.nt.qq.com.cn/synthetic?rkey=private-reference"}]}});
        receive(&state, &config, &payload).unwrap();
        let receipt = claim(&state).unwrap().unwrap();
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 2, 2);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[0, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255, 255])
                .unwrap();
        }
        let image = image_analysis::canonicalize(&STANDARD.encode(png)).unwrap();
        let hash = image.sha256.clone();
        crate::qq::ingest_prepared(
            &state,
            &config,
            &payload,
            receipt.received_at,
            receipt.deadline,
            Prepared::Image(image),
            Some(&receipt.fence),
        )
        .unwrap();
        state.db(|db|{
            let(source_owner,source_conversation,asset_owner,asset_conversation,asset_hash,payload_cleared):(String,String,String,String,String,bool)=db.query_row("SELECT i.owner_id,i.conversation_id,a.owner_id,a.conversation_id,a.sha256,e.payload_json IS NULL AND e.config_json IS NULL FROM conversation_images i JOIN conversation_media_assets a ON a.conversation_id=i.conversation_id JOIN qq_received_events e ON e.event_key=?1 WHERE i.image_sha256=?2",params![receipt.fence.event_key,hash],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
            assert_eq!(source_owner,"qq:testapp:c2c:target");assert_eq!(source_owner,asset_owner);assert_eq!(source_conversation,asset_conversation);assert_eq!(asset_hash,hash);assert!(payload_cleared);
            let count:i64=db.query_row("SELECT count(*) FROM inbox JOIN conversation_images i ON inbox.run_id=i.run_id JOIN messages m ON m.id=i.source_message_id WHERE m.role='user' AND m.content NOT LIKE '%rkey%'",[],|r|r.get(0))?;assert_eq!(count,1);
            Ok(())
        }).unwrap();
        receive(&state, &config, &payload).unwrap();
        assert_eq!(
            state
                .db(|db| Ok(db.query_row("SELECT count(*) FROM runs", [], |r| r.get::<_, i64>(0))?))
                .unwrap(),
            1
        );
    }
    #[test]
    fn unavailable_media_never_executes_caption_control_commands() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = state(&dir);
        let mut config = RuntimeConfig::default();
        config.qq.enabled = true;
        config.qq.app_id = "testapp".into();
        config.qq.credential_ref = Some("testkey".into());
        state
            .db(|db| {
                db.execute(
                    "UPDATE config_revisions SET config_json=?1 WHERE revision=1",
                    [serde_json::to_string(&config)?],
                )?;
                Ok(())
            })
            .unwrap();
        let payload = json!({"t":"C2C_MESSAGE_CREATE","id":"notice-event","d":{"id":"notice-message","content":"/记住 这是合成媒体控制命令测试","author":{"user_openid":"target"}}});
        receive(&state, &config, &payload).unwrap();
        let receipt = claim(&state).unwrap().unwrap();
        crate::qq::ingest_prepared(
            &state,
            &config,
            &payload,
            receipt.received_at,
            receipt.deadline,
            Prepared::Unavailable {
                code: "synthetic_media_rejected",
                message: "尚未查看图片，请使用PNG。",
            },
            Some(&receipt.fence),
        )
        .unwrap();
        state
            .db(|db| {
                let (kind, status): (String, String) =
                    db.query_row("SELECT run_kind,status FROM runs", [], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })?;
                assert_eq!(kind, "media_notice");
                assert_eq!(status, "completed");
                let memories: i64 =
                    db.query_row("SELECT count(*) FROM memory_records", [], |r| r.get(0))?;
                assert_eq!(memories, 0);
                Ok(())
            })
            .unwrap();
    }
    #[tokio::test]
    async fn ignored_empty_and_oversize_messages_release_their_private_receipts() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = state(&dir);
        let mut config = RuntimeConfig::default();
        config.qq.enabled = true;
        config.qq.app_id = "testapp".into();
        config.qq.credential_ref = Some("testkey".into());
        state
            .db(|db| {
                db.execute(
                    "UPDATE config_revisions SET config_json=?1 WHERE revision=1",
                    [serde_json::to_string(&config)?],
                )?;
                Ok(())
            })
            .unwrap();
        for (index, content) in ["".to_string(), "x".repeat(16001)].into_iter().enumerate() {
            let payload = json!({"t":"C2C_MESSAGE_CREATE","id":format!("ignored-event-{index}"),"d":{"id":format!("ignored-message-{index}"),"content":content,"author":{"user_openid":"target"}}});
            receive(&state, &config, &payload).unwrap();
            let receipt = claim(&state).unwrap().unwrap();
            process(&state, &receipt).await.unwrap();
            let result=state.db(|db|Ok(db.query_row("SELECT status,payload_json IS NULL AND config_json IS NULL FROM qq_received_events WHERE event_key=?1",[receipt.fence.event_key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,bool>(1)?)))?)).unwrap();
            assert_eq!(result.0, "completed");
            assert!(result.1);
        }
        assert_eq!(
            state
                .db(|db| Ok(db.query_row("SELECT count(*) FROM runs", [], |r| r.get::<_, i64>(0))?))
                .unwrap(),
            0
        );
    }
}
