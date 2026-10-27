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
    Video(crate::media_assets::ValidatedVideo),
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
        if let Self::Unavailable { code, .. } = self {
            Some(match *code {
                "media_size_limit" => "这张图有点大，我这边没打开。换张小一点的发我吧。",
                "media_count_unsupported" => "先一张一张发我吧，我好仔细看。",
                "media_channel_unsupported" => "这张图我这边没打开，可以单独发给我吗？",
                "media_control_command" => "图片和要做的事分开发我吧，我好接着看。",
                "media_mp4_invalid" => "这个视频我这边没打开。你可以截张图给我，或者直接说说内容。",
                "media_format_unsupported" => {
                    "这条内容我这边没打开。换张图发我，或者直接跟我说说吧。"
                }
                _ => "这张图我这边没打开，你再发一次给我吧。",
            })
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
    let has_ms = {
        let mut statement = db.prepare("PRAGMA table_info(qq_received_events)")?;
        let columns = statement.query_map([], |r| r.get::<_, String>(1))?;
        columns
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|name| name == "received_ms")
    };
    if !has_ms {
        db.execute_batch("ALTER TABLE qq_received_events ADD COLUMN received_ms INTEGER;")?;
    }
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
    let received_ms = crate::qq::text_batch::now_ms();
    let received = received_ms / 1000;
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
        tx.execute("INSERT INTO qq_received_events(event_key,account_id,event_type,message_id,credential_ref,config_json,payload_json,status,received_at,deadline,received_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,'queued',?8,?9,?10)",params![key,config.qq.app_id,event_type,message,credential,config_json,raw,received,deadline,received_ms])?;
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
        let row=tx.query_row("SELECT event_key,generation,config_json,payload_json,received_at,deadline FROM qq_received_events WHERE status='queued' ORDER BY rowid LIMIT 1",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?,r.get::<_,i64>(5)?))).optional()?;
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
fn requeue_capacity_in(db: &Connection, fence: &Fence) -> anyhow::Result<()> {
    let changed=db.execute("UPDATE qq_received_events SET status='queued',generation=generation+1,error_code='run_queue_full' WHERE event_key=?1 AND generation=?2 AND status='processing'",params![fence.event_key,fence.generation])?;
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
    let duplicate=state.db(|db|Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM inbox WHERE event_key=?1) OR EXISTS(SELECT 1 FROM runs WHERE owner_id=?2 AND client_message_id=?3) OR EXISTS(SELECT 1 FROM qq_text_fragments WHERE owner_id=?2 AND message_id=?3)",params![receipt.fence.event_key,owner,data["id"].as_str().unwrap_or("")],|r|r.get::<_,bool>(0))?))?;
    if duplicate {
        return state.db(|db| finish_in(db, &receipt.fence, "completed", None));
    }
    let available = crate::conversation_images::capabilities(state)?.available;
    let declared_image = receipt.payload["d"]["attachments"]
        .as_array()
        .filter(|items| items.len() == 1)
        .is_some_and(|items| items[0]["content_type"].as_str().is_some_and(image_mime));
    let prepared = if declared_image && !available {
        Prepared::Unavailable {
            code: "vision_provider_unavailable",
            message: "图片理解暂未配置可用的视觉模型，尚未查看图片，请改用文字。",
        }
    } else {
        prepare(&receipt.payload).await
    };
    let prepared = if matches!(&prepared, Prepared::Image(_)) && !available {
        Prepared::Unavailable {
            code: "vision_provider_unavailable",
            message: "图片理解暂未配置可用的视觉模型，尚未查看图片，请改用文字。",
        }
    } else {
        prepared
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
            Ok(Err(error)) if error.code == "qq_run_queue_full" => {
                // Only pre-Run capacity failures are retried, keeping the original
                // payload, receive times and passive deadline. Never replay a model
                // request or an unknown delivery. The consumer applies backpressure.
                let _ = state.db(|db| requeue_capacity_in(db, &receipt.fence));
                tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_millis(500))=>{}}
            }
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
            "目前媒体接收仅支持QQ单聊的一张PNG图片或一个MP4视频，请使用文字描述。",
        );
    }
    if attachments.len() != 1 {
        return unavailable(
            "media_count_unsupported",
            "目前每条消息只支持一张PNG图片或一个MP4视频，请分别发送。",
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
            "媒体消息暂不执行控制命令，请把命令单独作为文字发送。",
        );
    }
    let attachment = &attachments[0];
    let declared = attachment["content_type"].as_str();
    if declared
        .is_some_and(|s| !image_mime(s) && s != "video/mp4" && s != "application/octet-stream")
    {
        return unavailable(
            "media_format_unsupported",
            "仅接收PNG图片或20MiB/60秒以内的标准H.264 MP4；视频只保存用于回发，不读取帧或音轨，语音输入尚不支持。",
        );
    }
    let limit = if declared.is_some_and(image_mime) {
        image_analysis::MAX_IMAGE_BYTES
    } else {
        crate::media_assets::MAX_VIDEO
    };
    if attachment["size"]
        .as_u64()
        .is_some_and(|n| n > limit as u64)
    {
        return unavailable(
            "media_size_limit",
            "媒体超过大小限制：PNG最多1MiB，MP4最多20MiB，请压缩后重试。",
        );
    }
    let Some(raw) = attachment["url"].as_str().filter(|s| s.len() <= 8192) else {
        return unavailable(
            "media_url_missing",
            "QQ未提供可用的媒体下载引用，请重新发送PNG图片或MP4视频。",
        );
    };
    let bytes = match download(raw, limit).await {
        Ok(bytes) => bytes,
        Err(code) => {
            return unavailable(code, "媒体下载未完成，尚未读取内容，请重新发送或改用文字。")
        }
    };
    prepare_bytes(declared, bytes).await
}
fn image_mime(value: &str) -> bool {
    matches!(
        value
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "image/png"
            | "image/jpeg"
            | "image/jpg"
            | "image/pjpeg"
            | "image/webp"
            | "image/gif"
            | "image/bmp"
            | "image/x-ms-bmp"
    )
}
pub(crate) async fn prepare_bytes(declared: Option<&str>, bytes: Vec<u8>) -> Prepared {
    let unavailable = |code, message| Prepared::Unavailable { code, message };
    let is_image = image::guess_format(&bytes).is_ok_and(|f| {
        matches!(
            f,
            image::ImageFormat::Png
                | image::ImageFormat::Jpeg
                | image::ImageFormat::WebP
                | image::ImageFormat::Gif
                | image::ImageFormat::Bmp
        )
    });
    if declared == Some("video/mp4") || !is_image {
        if declared.is_some_and(image_mime) {
            return unavailable(
                "media_type_mismatch",
                "QQ媒体类型与实际文件不一致，请发送有效PNG或MP4。",
            );
        }
        return match crate::media_assets::validate_video(bytes) {
            Ok(video) => Prepared::Video(video),
            Err(_) => unavailable(
                "media_mp4_invalid",
                "视频须为20MiB以内、最长60秒、自包含H.264轨道的标准MP4；未读取帧或音轨。",
            ),
        };
    }
    if bytes.len() > image_analysis::MAX_IMAGE_BYTES {
        return unavailable("media_size_limit", "image byte budget exceeded");
    }
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
            "media_image_invalid",
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
async fn download(raw: &str, limit: usize) -> Result<Vec<u8>, &'static str> {
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
        .timeout(Duration::from_secs(12))
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
    if reply.content_length().is_some_and(|n| n > limit as u64) {
        return Err("media_size_limit");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = reply
        .chunk()
        .await
        .map_err(|_| "media_download_incomplete")?
    {
        if bytes.len().saturating_add(chunk.len()) > limit {
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
    #[tokio::test]
    async fn acknowledged_text_waits_for_run_capacity_without_losing_original_receipt() {
        let directory = tempfile::TempDir::new().unwrap();
        let state = state(&directory);
        let config = state
            .db(|db| {
                let (_, mut config) = crate::character::current_config_in(db)?;
                config.qq.enabled = true;
                config.qq.app_id = "capacity_app".into();
                config.qq.credential_ref = Some("capacity_key".into());
                db.execute(
                    "UPDATE config_revisions SET config_json=?1 WHERE revision=1",
                    [serde_json::to_string(&config)?],
                )?;
                Ok(config)
            })
            .unwrap();
        for index in 0..10 {
            let event = json!({"t":"C2C_MESSAGE_CREATE","id":format!("full_{index}"),"d":{"id":format!("full_message_{index}"),"content":"x".repeat(257),"author":{"user_openid":"peer"}}});
            crate::qq::ingest(&state, &config, &event).unwrap();
        }
        let event = json!({"t":"C2C_MESSAGE_CREATE","id":"kept_event","d":{"id":"kept_message","content":"keep this acknowledged text","author":{"user_openid":"peer"}}});
        receive(&state, &config, &event).unwrap();
        let receipt = claim(&state).unwrap().unwrap();
        let original_ms = state
            .db(|db| {
                Ok(db.query_row(
                    "SELECT received_ms FROM qq_received_events WHERE event_key=?1",
                    [&receipt.fence.event_key],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .unwrap();
        assert_eq!(
            process(&state, &receipt).await.unwrap_err().code,
            "qq_run_queue_full"
        );
        state
            .db(|db| requeue_capacity_in(db, &receipt.fence))
            .unwrap();
        let retained=state.db(|db|Ok(db.query_row("SELECT status,payload_json IS NOT NULL AND config_json IS NOT NULL,received_ms,deadline FROM qq_received_events WHERE event_key=?1",[&receipt.fence.event_key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,bool>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?)))?)).unwrap();
        assert_eq!(
            retained,
            ("queued".into(), true, original_ms, receipt.deadline)
        );
        assert!(state
            .db(|db| finish_in(db, &receipt.fence, "failed", None))
            .is_err());
        state
            .db(|db| {
                db.execute(
                    "UPDATE runs SET status='cancelled' WHERE client_message_id='full_message_0'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let retry = claim(&state).unwrap().unwrap();
        process(&state, &retry).await.unwrap();
        let final_state=state.db(|db|Ok(db.query_row("SELECT e.status,e.payload_json IS NULL, f.content,f.received_ms FROM qq_received_events e JOIN qq_text_fragments f ON f.message_id=e.message_id WHERE e.event_key=?1",[&retry.fence.event_key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,bool>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?)))?)).unwrap();
        assert_eq!(
            final_state,
            (
                "completed".into(),
                true,
                "keep this acknowledged text".into(),
                original_ms
            )
        );
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
    #[tokio::test]
    async fn video_policy_rejects_groups_multiple_files_commands_oversize_and_foreign_urls_before_download(
    ) {
        let base = json!({"t":"C2C_MESSAGE_CREATE","d":{"content":"收到视频。","attachments":[{"content_type":"video/mp4","url":"https://127.0.0.1/private"}]}});
        let mut cases = vec![];
        let mut group = base.clone();
        group["t"] = json!("GROUP_AT_MESSAGE_CREATE");
        cases.push((group, "media_channel_unsupported"));
        let mut multiple = base.clone();
        multiple["d"]["attachments"] = json!([{}, {}]);
        cases.push((multiple, "media_count_unsupported"));
        let mut command = base.clone();
        command["d"]["content"] = json!("/记住 视频不是控制命令");
        cases.push((command, "media_control_command"));
        let mut large = base.clone();
        large["d"]["attachments"][0]["size"] = json!(crate::media_assets::MAX_VIDEO + 1);
        cases.push((large, "media_size_limit"));
        cases.push((base, "media_url_not_allowed"));
        for (payload, expected) in cases {
            assert!(
                matches!(prepare(&payload).await,Prepared::Unavailable {code,..} if code==expected)
            );
        }
    }
    #[tokio::test]
    async fn uploaded_private_video_reaches_native_caption_reply_and_durable_video_outbox_without_vision(
    ) {
        use axum::{routing::post, Json, Router};
        use std::sync::{Arc, Mutex};
        let dir = tempfile::tempdir().unwrap();
        let state = state(&dir);
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let seen = requests.clone();
        let provider_state = state.clone();
        let app=Router::new().route("/chat/completions",post(move |Json(body):Json<Value>|{
            let seen=seen.clone();let provider_state=provider_state.clone();
            async move {
                seen.lock().unwrap().push(body);
                let id=provider_state.db(|db|Ok(db.query_row("SELECT id FROM conversation_media_assets WHERE kind='video'",[],|r|r.get::<_,String>(0))?)).unwrap();
                Json(json!({"choices":[{"message":{"content":json!([{"type":"video","text":"这是你刚才上传的视频。","asset_id":id,"delay_ms":0}]).to_string()}}]}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let provider = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let key = uuid::Uuid::new_v4().to_string();
        crate::state::private_write(
            &dir.path().join("secrets").join(&key),
            b"synthetic-video-model-key",
        )
        .unwrap();
        let mut config = RuntimeConfig::default();
        config.qq.enabled = true;
        config.qq.app_id = "10001".into();
        config.qq.credential_ref = Some("synthetic-qq-key".into());
        config.model.base_url = format!("http://127.0.0.1:{port}");
        config.model.model = "synthetic-video-caption".into();
        config.model.credential_ref = Some(key.clone());
        state
            .db(|db| {
                db.execute(
                    "INSERT INTO secrets(id,kind,created_at) VALUES(?1,'model_api_key',0)",
                    [key],
                )?;
                db.execute(
                    "UPDATE config_revisions SET config_json=?1 WHERE revision=1",
                    [serde_json::to_string(&config)?],
                )?;
                Ok(())
            })
            .unwrap();
        let payload = json!({"t":"C2C_MESSAGE_CREATE","id":"video-event","d":{"id":"video-message","content":"请把这个视频原样发回给我。","author":{"user_openid":"target"},"attachments":[{"content_type":"video/mp4","url":"https://multimedia.nt.qq.com.cn/synthetic?rkey=private-video-reference"}]}});
        receive(&state, &config, &payload).unwrap();
        let receipt = claim(&state).unwrap().unwrap();
        let bytes = include_bytes!("../testdata/synthetic-h264.mp4").to_vec();
        crate::qq::ingest_prepared(
            &state,
            &config,
            &payload,
            receipt.received_at,
            receipt.deadline,
            Prepared::Video(crate::media_assets::validate_video(bytes.clone()).unwrap()),
            Some(&receipt.fence),
        )
        .unwrap();
        let (run,conversation,owner,asset)=state.db(|db|Ok(db.query_row("SELECT r.id,r.conversation_id,r.owner_id,a.id FROM runs r JOIN conversation_media_assets a ON a.conversation_id=r.conversation_id WHERE r.client_message_id='video-message'",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?)).unwrap();
        assert_eq!(owner, "qq:10001:c2c:target");
        assert_eq!(
            state
                .db(|db| Ok(
                    db.query_row("SELECT count(*) FROM conversation_images", [], |r| r
                        .get::<_, i64>(0))?
                ))
                .unwrap(),
            0
        );
        assert!(crate::media_assets::read(&state, "other-owner", &conversation, &asset).is_err());
        let cancel = CancellationToken::new();
        let worker_state = state.clone();
        let worker_cancel = cancel.clone();
        let worker = tokio::spawn(async move {
            crate::worker::run_worker(worker_state, worker_cancel).await;
        });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let status = loop {
            let status = state
                .db(|db| {
                    Ok(
                        db.query_row("SELECT status FROM runs WHERE id=?1", [&run], |r| {
                            r.get::<_, String>(0)
                        })?,
                    )
                })
                .unwrap();
            if status != "queued" && status != "running" {
                break status;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "synthetic caption run did not finish"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        cancel.cancel();
        worker.await.unwrap();
        provider.abort();
        let failure = state
            .db(|db| {
                Ok(
                    db.query_row("SELECT error FROM runs WHERE id=?1", [&run], |r| {
                        r.get::<_, Option<String>>(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(
            status, "completed",
            "synthetic caption failure: {failure:?}"
        );
        let observed = requests.lock().unwrap();
        assert_eq!(observed.len(), 1);
        let prompt = observed[0].to_string();
        assert!(prompt.contains("不读取视频帧或音轨"));
        assert!(prompt.contains("请把这个视频原样发回给我"));
        assert!(prompt.contains(&asset));
        assert!(!prompt.contains("private-video-reference"));
        assert!(!prompt.contains(&STANDARD.encode(&bytes)));
        drop(observed);
        state.db(|db|{
            let (part,target):(String,String)=db.query_row("SELECT p.payload_json,o.target_json FROM outbox_reply_parts p JOIN outbox o ON p.outbox_id=o.id WHERE o.run_id=?1",[&run],|r|Ok((r.get(0)?,r.get(1)?)))?;
            let part:Value=serde_json::from_str(&part)?;assert_eq!(part["type"],"video");assert_eq!(part["asset_id"],asset);
            let target:Value=serde_json::from_str(&target)?;assert_eq!(target["deadline"],receipt.deadline);
            let cleared:bool=db.query_row("SELECT payload_json IS NULL AND config_json IS NULL FROM qq_received_events WHERE event_key=?1",[&receipt.fence.event_key],|r|r.get(0))?;assert!(cleared);
            Ok(())
        }).unwrap();
        crate::qq::ingest_prepared(
            &state,
            &config,
            &payload,
            receipt.received_at,
            receipt.deadline,
            Prepared::Video(crate::media_assets::validate_video(bytes).unwrap()),
            None,
        )
        .unwrap();
        assert_eq!(
            state
                .db(|db| Ok(db.query_row(
                    "SELECT count(*) FROM conversation_media_assets",
                    [],
                    |r| r.get::<_, i64>(0)
                )?))
                .unwrap(),
            1
        );
        assert_eq!(
            state
                .db(|db| Ok(db.query_row("SELECT count(*) FROM runs", [], |r| r.get::<_, i64>(0))?))
                .unwrap(),
            1
        );
        state
            .db(|db| {
                db.execute(
                    "INSERT INTO memory_context_watermarks VALUES(?1,'assistant',999,0)",
                    [&conversation],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(crate::media_assets::read(&state, &owner, &conversation, &asset).is_err());
        // A storage failure must roll back Inbox/Run/message and the media row
        // together. The durable receive record remains available to its owner.
        state.db(|db|{db.execute_batch("CREATE TRIGGER synthetic_video_write_failure BEFORE INSERT ON conversation_media_assets WHEN NEW.kind='video' BEGIN SELECT RAISE(ABORT,'synthetic fixture refusal');END;")?;Ok(())}).unwrap();
        let mut refused = payload.clone();
        refused["id"] = json!("refused-video-event");
        refused["d"]["id"] = json!("refused-video-message");
        receive(&state, &config, &refused).unwrap();
        let receipt = claim(&state).unwrap().unwrap();
        assert!(crate::qq::ingest_prepared(
            &state,
            &config,
            &refused,
            receipt.received_at,
            receipt.deadline,
            Prepared::Video(
                crate::media_assets::validate_video(
                    include_bytes!("../testdata/synthetic-h264.mp4").to_vec()
                )
                .unwrap()
            ),
            Some(&receipt.fence)
        )
        .is_err());
        state
            .db(|db| {
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM runs WHERE client_message_id='refused-video-message'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?,
                    0
                );
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM inbox WHERE event_key=?1",
                        [&receipt.fence.event_key],
                        |r| r.get::<_, i64>(0)
                    )?,
                    0
                );
                assert_eq!(
                    db.query_row("SELECT count(*) FROM conversation_media_assets", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                    0
                );
                Ok(())
            })
            .unwrap();
    }
}
