//! Bounded, local Piper synthesis. Model and runtime paths are deployment-owned.
use crate::{
    error::{ApiError, ApiResult},
    state::{now, AppState},
};
use axum::http::StatusCode;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    process::Stdio,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Semaphore,
};

pub const VOICE: &str = "zh_CN-huayan-medium";
pub const MODEL_SHA256: &str = "9929917bf8cabb26fd528ea44d3a6699c11e87317a14765312420be230be0f3d";
const CONFIG_SHA256: &str = "d521dc45504a8ccc99e325822b35946dd701840bfb07e3dbb31a40929ed6a82b";
pub const MAX_AUDIO_BYTES: usize = 4_000_000;
pub const ASSET_TTL_SECONDS: i64 = 3600;
const TOTAL_ASSET_BYTES: i64 = 32 * 1024 * 1024;
const WORKER: &str = include_str!("../../../tools/piper-worker.py");
static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
fn slots() -> &'static Arc<Semaphore> {
    SLOTS.get_or_init(|| Arc::new(Semaphore::new(1)))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpeechConfig {
    pub enabled: bool,
    pub voice: String,
    pub timeout_seconds: u64,
    pub max_text_chars: usize,
}
impl Default for SpeechConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            voice: VOICE.into(),
            timeout_seconds: 30,
            max_text_chars: 300,
        }
    }
}
impl SpeechConfig {
    pub fn validate(&self) -> Vec<Value> {
        let mut result = vec![];
        if self.voice != VOICE {
            result.push(json!({"field":"speech.voice","message":"仅支持已部署的固定中文声音 zh_CN-huayan-medium"}));
        }
        if !(5..=60).contains(&self.timeout_seconds) {
            result.push(
                json!({"field":"speech.timeout_seconds","message":"语音合成超时须为5到60秒"}),
            );
        }
        if !(1..=400).contains(&self.max_text_chars) {
            result.push(
                json!({"field":"speech.max_text_chars","message":"单次语音限额须为1到400字符"}),
            );
        }
        result
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpeechAsset {
    pub id: String,
    pub conversation_id: Option<String>,
    pub provider: String,
    pub voice: String,
    pub mime_type: String,
    pub sha256: String,
    pub bytes: usize,
    pub duration_ms: u64,
    pub sample_rate: u32,
    pub synthesis_ms: u64,
    pub created_at: i64,
    pub expires_at: i64,
    pub audio_url: String,
}

pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS speech_assets(
      id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,conversation_id TEXT REFERENCES conversations(id),
      context_floor INTEGER NOT NULL,metadata_json TEXT NOT NULL,audio BLOB NOT NULL,
      created_at INTEGER NOT NULL,expires_at INTEGER NOT NULL);
      CREATE INDEX IF NOT EXISTS speech_assets_expiry ON speech_assets(expires_at);",
    )?;
    let present = {
        let mut stmt = db.prepare("PRAGMA table_info(speech_assets)")?;
        let names = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        names.iter().any(|name| name == "source_run_id")
    };
    if !present {
        db.execute(
            "ALTER TABLE speech_assets ADD COLUMN source_run_id TEXT REFERENCES runs(id)",
            [],
        )?;
    }
    prune_in(db)?;
    Ok(())
}
pub fn prune_in(db: &Connection) -> anyhow::Result<()> {
    db.execute("DELETE FROM speech_assets WHERE expires_at<=?1 OR (conversation_id IS NOT NULL AND (NOT EXISTS(SELECT 1 FROM conversations c WHERE c.id=speech_assets.conversation_id AND c.owner_id=speech_assets.owner_id) OR context_floor<>COALESCE((SELECT through_run_rowid FROM memory_context_watermarks w WHERE w.conversation_id=speech_assets.conversation_id),0))) OR (source_run_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM runs r WHERE r.id=speech_assets.source_run_id AND r.owner_id=speech_assets.owner_id AND r.conversation_id=speech_assets.conversation_id AND r.status IN ('running','completed')))", [now()])?;
    Ok(())
}

#[derive(Clone)]
struct Runtime {
    executable: PathBuf,
    model: PathBuf,
}
impl Runtime {
    fn resolve(state: &AppState) -> Self {
        let base = state
            .0
            .options
            .data_dir
            .parent()
            .unwrap_or(&state.0.options.data_dir)
            .join("speech/piper-1.8.0");
        Self {
            executable: std::env::var_os("DSH_PIPER_EXECUTABLE")
                .map(PathBuf::from)
                .unwrap_or_else(|| base.join("venv/bin/python")),
            model: std::env::var_os("DSH_PIPER_MODEL")
                .map(PathBuf::from)
                .unwrap_or_else(|| base.join("models/zh_CN-huayan-medium.onnx")),
        }
    }
    fn present(&self) -> bool {
        self.executable.is_file()
            && self.model.is_file()
            && self.model.with_extension("onnx.json").is_file()
    }
    fn verify(&self) -> ApiResult<()> {
        if !self.present() {
            return Err(unavailable());
        }
        for (path, size, sha) in [
            (&self.model, 63_201_294, MODEL_SHA256),
            (&self.model.with_extension("onnx.json"), 4822, CONFIG_SHA256),
        ] {
            if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) != size {
                return Err(unavailable());
            }
            let bytes = std::fs::read(path).map_err(|_| unavailable())?;
            if hex::encode(Sha256::digest(&bytes)) != sha {
                return Err(unavailable());
            }
        }
        Ok(())
    }
}
fn unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "speech_unavailable",
        "本地Piper运行时或已校验的中文声音不可用，请检查部署；可继续使用文字",
    )
}

/// Safe diagnostics: no paths, text, audio bytes, or inherited secrets.
pub fn status(state: &AppState, config: &SpeechConfig) -> Value {
    json!({"enabled":config.enabled,"provider":"piper","voice":config.voice,
      "runtime_present":Runtime::resolve(state).present(),"available":config.enabled && Runtime::resolve(state).present() && config.validate().is_empty(),
      "active_workers":1-slots().available_permits(),"max_workers":1,"data_path":"local",
      "model_sha256":MODEL_SHA256,"engine_license":"GPL-3.0-or-later","model_dataset_license":"Unknown",
      "policy":{"timeout_seconds":config.timeout_seconds,"max_text_chars":config.max_text_chars,"max_audio_bytes":MAX_AUDIO_BYTES,"max_audio_seconds":90,"asset_ttl_seconds":ASSET_TTL_SECONDS,"private_assets":true,"speaker_cloning":false}})
}

/// Removes code blocks, URL tokens and raw phoneme syntax; never reads a URL.
pub fn clean_text(text: &str, max: usize) -> ApiResult<String> {
    if text.chars().count() > max || text.chars().any(|c| c.is_control() && !c.is_whitespace()) {
        return Err(ApiError::bad(
            "speech_text_limit",
            "朗读文字超过限额或含控制字符",
        ));
    }
    let mut code = false;
    let mut pieces = vec![];
    for (index, part) in text.split("```").enumerate() {
        if index % 2 == 0 {
            pieces.extend(
                part.split_whitespace()
                    .filter(|word| !word.starts_with("https://") && !word.starts_with("http://"))
                    .map(str::to_owned),
            );
        } else {
            code = true;
        }
    }
    let result = pieces
        .join(" ")
        .replace(['`', '*', '#'], "")
        .replace("[[", "")
        .replace("]]", "");
    let result = result.trim();
    if result.is_empty() {
        return Err(ApiError::bad(
            "speech_no_text",
            if code {
                "代码内容不适合朗读，请提供普通文字"
            } else {
                "请提供可朗读的普通文字"
            },
        ));
    }
    Ok(result.to_owned())
}

fn context_floor(state: &AppState, owner: &str, conversation: Option<&str>) -> ApiResult<i64> {
    let Some(conversation) = conversation else {
        return Ok(0);
    };
    state
        .db(|db| {
            let owns: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM conversations WHERE id=?1 AND owner_id=?2)",
                params![conversation, owner],
                |r| r.get(0),
            )?;
            if !owns {
                return Ok(None);
            }
            Ok(Some(crate::memory_context::history_floor(
                db,
                conversation,
            )?))
        })?
        .ok_or_else(ApiError::not_found)
}

/// Detached admission lease lasts until the subprocess has actually exited,
/// including if an HTTP caller disconnects. There is no unbounded waiting queue.
pub async fn synthesize(
    state: &AppState,
    config: &SpeechConfig,
    owner: &str,
    conversation: Option<&str>,
    text: &str,
) -> ApiResult<SpeechAsset> {
    synthesize_inner(state, config, owner, conversation, text, None).await
}
pub async fn synthesize_for_run(
    state: &AppState,
    config: &SpeechConfig,
    owner: &str,
    conversation: &str,
    text: &str,
    source: &crate::media_assets::MaterializationSource,
) -> ApiResult<SpeechAsset> {
    synthesize_inner(
        state,
        config,
        owner,
        Some(conversation),
        text,
        Some(source.clone()),
    )
    .await
}
fn source_active(
    state: &AppState,
    source: Option<&crate::media_assets::MaterializationSource>,
    owner: &str,
    conversation: Option<&str>,
) -> ApiResult<bool> {
    let Some(source) = source else {
        return Ok(true);
    };
    let Some(conversation) = conversation else {
        return Ok(false);
    };
    state.db(|db| crate::media_assets::source_active_in(db, source, owner, conversation))
}
async fn synthesize_inner(
    state: &AppState,
    config: &SpeechConfig,
    owner: &str,
    conversation: Option<&str>,
    text: &str,
    source: Option<crate::media_assets::MaterializationSource>,
) -> ApiResult<SpeechAsset> {
    if !source_active(state, source.as_ref(), owner, conversation)? {
        return Err(ApiError::conflict());
    }
    if !config.enabled {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "speech_disabled",
            "本地语音尚未启用，可继续使用文字",
        ));
    }
    if !config.validate().is_empty() {
        return Err(ApiError::bad("invalid_speech_config", "语音配置无效"));
    }
    let text = clean_text(text, config.max_text_chars)?;
    let floor = context_floor(state, owner, conversation)?;
    let permit = slots().clone().try_acquire_owned().map_err(|_| {
        ApiError::new(
            StatusCode::CONFLICT,
            "speech_busy",
            "本地语音正在合成，请稍后重试",
        )
    })?;
    let state = state.clone();
    let runtime = Runtime::resolve(&state);
    let config = config.clone();
    let owner = owner.to_owned();
    let conversation = conversation.map(str::to_owned);
    tokio::spawn(async move {
        let _permit = permit;
        let verification = runtime.clone();
        tokio::task::spawn_blocking(move || verification.verify())
            .await
            .map_err(|_| ApiError::internal())??;
        if !source_active(&state, source.as_ref(), &owner, conversation.as_deref())? {
            return Err(ApiError::conflict());
        }
        let started = Instant::now();
        let audio = run_worker(&runtime, &text, config.timeout_seconds).await?;
        let (sample_rate, duration_ms) = validate_wav(&audio)?;
        if context_floor(&state, &owner, conversation.as_deref())? != floor {
            return Err(ApiError::conflict());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let asset = SpeechAsset {
            id: id.clone(),
            conversation_id: conversation.clone(),
            provider: "piper".into(),
            voice: config.voice,
            mime_type: "audio/wav".into(),
            sha256: hex::encode(Sha256::digest(&audio)),
            bytes: audio.len(),
            duration_ms,
            sample_rate,
            synthesis_ms: started.elapsed().as_millis() as u64,
            created_at: now(),
            expires_at: now() + ASSET_TTL_SECONDS,
            audio_url: format!("/api/admin/v1/speech/assets/{id}"),
        };
        persist_asset(
            &state,
            &owner,
            conversation.as_deref(),
            floor,
            source.as_ref(),
            &asset,
            &audio,
        )?;
        Ok(asset)
    })
    .await
    .map_err(|_| ApiError::internal())?
}
fn persist_asset(
    state: &AppState,
    owner: &str,
    conversation: Option<&str>,
    floor: i64,
    source: Option<&crate::media_assets::MaterializationSource>,
    asset: &SpeechAsset,
    audio: &[u8],
) -> ApiResult<()> {
    state.db(|db| {
            let tx = db.transaction()?;
            prune_in(&tx)?;
            if let Some(conversation)=conversation {
                let owns:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM conversations WHERE id=?1 AND owner_id=?2)",params![conversation,owner],|r|r.get(0))?;
                if !owns || crate::memory_context::history_floor(&tx,conversation)?!=floor {return Ok(2);}
            }
            if let Some(source)=&source {
                if !crate::media_assets::source_active_in(&tx,source,owner,conversation.unwrap_or_default())? {return Ok(2);}
            }
            crate::chat_reply::require_reader(&tx, if source.is_some() {10}else{9})?;
            let (count,bytes):(i64,i64) = tx.query_row("SELECT count(*),COALESCE(sum(length(audio)),0) FROM speech_assets",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
            let owner_count:i64 = tx.query_row("SELECT count(*) FROM speech_assets WHERE owner_id=?1",[owner],|r|r.get(0))?;
            if count>=64 || owner_count>=20 || bytes+audio.len() as i64 > TOTAL_ASSET_BYTES { return Ok(0); }
            tx.execute("INSERT INTO speech_assets(id,owner_id,conversation_id,context_floor,metadata_json,audio,created_at,expires_at,source_run_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![asset.id,owner,conversation,floor,serde_json::to_string(asset)?,audio,asset.created_at,asset.expires_at,source.map(|s|s.run_id.as_str())])?;
            tx.commit()?;
            Ok(1)
        }).and_then(|outcome|match outcome {
            1=>Ok(()),2=>Err(ApiError::conflict()),
            _=>Err(ApiError::new(StatusCode::CONFLICT,"speech_storage_limit","短期语音制品已达限额，请等待过期或清理制品")),
        })
}

async fn run_worker(runtime: &Runtime, text: &str, timeout_seconds: u64) -> ApiResult<Vec<u8>> {
    let mut command = tokio::process::Command::new(&runtime.executable);
    command
        .args(["-I", "-c", WORKER])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("OMP_NUM_THREADS", "2")
        .env("OPENBLAS_NUM_THREADS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| unavailable())?;
    let mut input = child.stdin.take().ok_or_else(ApiError::internal)?;
    let mut output = child.stdout.take().ok_or_else(ApiError::internal)?;
    let payload = serde_json::to_vec(&json!({"text":text,"model":runtime.model}))
        .map_err(|_| ApiError::internal())?;
    let operation = async {
        input
            .write_all(&payload)
            .await
            .map_err(|_| ApiError::internal())?;
        input.shutdown().await.map_err(|_| ApiError::internal())?;
        drop(input);
        let mut audio = vec![];
        (&mut output)
            .take((MAX_AUDIO_BYTES + 1) as u64)
            .read_to_end(&mut audio)
            .await
            .map_err(|_| ApiError::internal())?;
        if audio.len() > MAX_AUDIO_BYTES {
            return Err(ApiError::bad(
                "speech_output_limit",
                "合成音频超过限制，已停止",
            ));
        }
        let exit = child.wait().await.map_err(|_| ApiError::internal())?;
        if !exit.success() {
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "speech_worker_failed",
                "本地语音合成失败，可继续使用文字",
            ));
        }
        Ok(audio)
    };
    match tokio::time::timeout(Duration::from_secs(timeout_seconds), operation).await {
        Ok(Ok(audio)) => Ok(audio),
        result => {
            // Do not return admission until an overlong/failed process is reaped.
            let _ = child.kill().await;
            let _ = child.wait().await;
            match result {
                Ok(Err(error)) => Err(error),
                _ => Err(ApiError::new(
                    StatusCode::GATEWAY_TIMEOUT,
                    "speech_timeout",
                    "语音合成超时，已终止worker；可继续使用文字",
                )),
            }
        }
    }
}

/// Accept only complete PCM mono 16-bit WAV with bounded duration and length.
pub fn validate_wav(bytes: &[u8]) -> ApiResult<(u32, u64)> {
    let bad = || ApiError::bad("invalid_speech_audio", "合成结果不是允许的PCM WAV音频");
    if bytes.len() < 44
        || bytes.len() > MAX_AUDIO_BYTES
        || &bytes[..4] != b"RIFF"
        || &bytes[8..12] != b"WAVE"
    {
        return Err(bad());
    }
    let le32 = |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    if le32(4) as usize + 8 != bytes.len() {
        return Err(bad());
    }
    let mut offset = 12;
    let mut format = None;
    let mut size = None;
    while offset + 8 <= bytes.len() {
        let kind = &bytes[offset..offset + 4];
        let length = le32(offset + 4) as usize;
        let end = offset
            .checked_add(8)
            .and_then(|v| v.checked_add(length))
            .ok_or_else(bad)?;
        if end > bytes.len() {
            return Err(bad());
        }
        if kind == b"fmt " {
            if format.is_some() || length < 16 {
                return Err(bad());
            }
            let body = &bytes[offset + 8..end];
            let rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            if body[..4] != [1, 0, 1, 0]
                || body[12..16] != [2, 0, 16, 0]
                || !(16000..=48000).contains(&rate)
                || u32::from_le_bytes(body[8..12].try_into().unwrap()) != rate * 2
            {
                return Err(bad());
            }
            format = Some(rate);
        } else if kind == b"data" {
            if size.is_some() || length == 0 || length % 2 != 0 {
                return Err(bad());
            }
            size = Some(length);
        }
        offset = end + (length % 2);
    }
    if offset != bytes.len() {
        return Err(bad());
    }
    let (Some(rate), Some(size)) = (format, size) else {
        return Err(bad());
    };
    let duration = size as u64 * 1000 / (rate as u64 * 2);
    if duration == 0 || duration > 90_000 {
        return Err(bad());
    }
    Ok((rate, duration))
}

pub fn read_asset(state: &AppState, owner: &str, id: &str) -> ApiResult<(SpeechAsset, Vec<u8>)> {
    if uuid::Uuid::parse_str(id).is_err() {
        return Err(ApiError::not_found());
    }
    state.db(|db| {
        prune_in(db)?;
        let row:Option<(String,Vec<u8>)>=db.query_row("SELECT metadata_json,audio FROM speech_assets a WHERE id=?1 AND owner_id=?2 AND expires_at>?3 AND (source_run_id IS NULL OR EXISTS(SELECT 1 FROM runs r WHERE r.id=a.source_run_id AND r.owner_id=a.owner_id AND r.conversation_id=a.conversation_id AND r.status='completed'))",params![id,owner,now()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        row.map(|(metadata,audio)|Ok((serde_json::from_str(&metadata)?,audio))).transpose()
    })?.ok_or_else(ApiError::not_found)
}
pub fn delete_asset(state: &AppState, owner: &str, id: &str) -> ApiResult<()> {
    let removed = state.db(|db| {
        Ok(db.execute(
            "DELETE FROM speech_assets WHERE id=?1 AND owner_id=?2",
            params![id, owner],
        )?)
    })?;
    if removed == 0 {
        return Err(ApiError::not_found());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wav() -> Vec<u8> {
        let mut b = vec![];
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&356u32.to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&[1, 0, 1, 0]);
        b.extend_from_slice(&16000u32.to_le_bytes());
        b.extend_from_slice(&32000u32.to_le_bytes());
        b.extend_from_slice(&[2, 0, 16, 0]);
        b.extend_from_slice(b"data");
        b.extend_from_slice(&320u32.to_le_bytes());
        b.extend_from_slice(&[0; 320]);
        b
    }
    #[test]
    fn audio_validation_rejects_truncation_stereo_and_duration_bombs() {
        let b = wav();
        assert_eq!(validate_wav(&b).unwrap(), (16000, 10));
        assert!(validate_wav(&b[..b.len() - 1]).is_err());
        let mut invalid = b.clone();
        invalid[22] = 2;
        assert!(validate_wav(&invalid).is_err());
        let mut invalid = b.clone();
        invalid[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(validate_wav(&invalid).is_err());
        let mut invalid = b;
        invalid.extend_from_slice(b"extra");
        assert!(validate_wav(&invalid).is_err());
    }
    #[test]
    fn spoken_text_has_bounds_and_omits_code_urls_and_phoneme_markup() {
        assert_eq!(
            clean_text(
                "你好。\n```rust\npanic!()\n```\n请听。 https://example.com [[语音]]",
                300
            )
            .unwrap(),
            "你好。 请听。 语音"
        );
        assert!(clean_text("```x```", 300).is_err());
        assert!(clean_text("文本", 1).is_err());
        assert!(clean_text("a\0b", 300).is_err());
        assert_eq!(SpeechConfig::default().validate().len(), 0);
    }
    #[test]
    fn artifact_scope_expiry_and_context_invalidation_are_enforced() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open(crate::state::Options {
            data_dir: temp.path().join("data"),
            origin: "http://127.0.0.1:18419".into(),
            allow_private_model: false,
            cookie_secure: false,
            web_dir: temp.path().join("web"),
        })
        .unwrap();
        initialize(&state.0.db.lock().unwrap()).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let asset = SpeechAsset {
            id: id.clone(),
            conversation_id: Some("c".into()),
            provider: "piper".into(),
            voice: VOICE.into(),
            mime_type: "audio/wav".into(),
            sha256: "fixture".into(),
            bytes: 364,
            duration_ms: 10,
            sample_rate: 16000,
            synthesis_ms: 1,
            created_at: now(),
            expires_at: now() + 60,
            audio_url: "fixture".into(),
        };
        state.db(|db|{db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('c','owner','fixture',0)",[])?;
            db.execute("INSERT INTO speech_assets(id,owner_id,conversation_id,context_floor,metadata_json,audio,created_at,expires_at) VALUES(?1,'owner','c',0,?2,?3,0,?4)",params![id,serde_json::to_string(&asset)?,wav(),now()+60])?;Ok(())}).unwrap();
        assert!(read_asset(&state, "other", &id).is_err());
        assert!(read_asset(&state, "owner", &id).is_ok());
        state
            .db(|db| {
                db.execute(
                    "INSERT INTO memory_context_watermarks VALUES('c','assistant',1,0)",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(read_asset(&state, "owner", &id).is_err());
    }
    #[test]
    fn synthesized_audio_persistence_atomically_rejects_cancelled_source_and_new_floor() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open(crate::state::Options {
            data_dir: temp.path().into(),
            origin: "http://127.0.0.1:18419".into(),
            allow_private_model: false,
            cookie_secure: false,
            web_dir: temp.path().into(),
        })
        .unwrap();
        state.db(|db|{
            db.execute("INSERT INTO conversations VALUES('c','owner','fixture',NULL,0)",[])?;
            db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at) VALUES('r','c','owner','r','q','{}','running',0,0)",[])?;
            Ok(())
        }).unwrap();
        let source = state
            .db(|db| crate::media_assets::source_in(db, "r", "owner", "c"))
            .unwrap()
            .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let asset = SpeechAsset {
            id,
            conversation_id: Some("c".into()),
            provider: "piper".into(),
            voice: VOICE.into(),
            mime_type: "audio/wav".into(),
            sha256: "fixture".into(),
            bytes: 364,
            duration_ms: 10,
            sample_rate: 16000,
            synthesis_ms: 1,
            created_at: now(),
            expires_at: now() + 60,
            audio_url: "fixture".into(),
        };
        state
            .db(|db| {
                db.execute("UPDATE runs SET status='cancelled' WHERE id='r'", [])?;
                Ok(())
            })
            .unwrap();
        assert!(
            persist_asset(&state, "owner", Some("c"), 0, Some(&source), &asset, &wav()).is_err()
        );
        state
            .db(|db| {
                db.execute("UPDATE runs SET status='running' WHERE id='r'", [])?;
                db.execute(
                    "INSERT INTO memory_context_watermarks VALUES('c','assistant',1,0)",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(
            persist_asset(&state, "owner", Some("c"), 0, Some(&source), &asset, &wav()).is_err()
        );
        assert_eq!(
            state
                .db(
                    |db| Ok(db.query_row("SELECT count(*) FROM speech_assets", [], |r| r
                        .get::<_, i64>(0))?)
                )
                .unwrap(),
            0
        );
        let fresh = state
            .db(|db| crate::media_assets::source_in(db, "r", "owner", "c"))
            .unwrap()
            .unwrap();
        persist_asset(&state, "owner", Some("c"), 1, Some(&fresh), &asset, &wav()).unwrap();
        assert!(read_asset(&state, "owner", &asset.id).is_err());
        assert_eq!(
            state
                .db(
                    |db| Ok(db.query_row("SELECT count(*) FROM speech_assets", [], |r| r
                        .get::<_, i64>(0))?)
                )
                .unwrap(),
            1
        );
        state
            .db(|db| {
                db.execute("UPDATE runs SET status='completed' WHERE id='r'", [])?;
                Ok(())
            })
            .unwrap();
        assert!(read_asset(&state, "owner", &asset.id).is_ok());
        state
            .db(|db| {
                db.execute("UPDATE runs SET status='interrupted' WHERE id='r'", [])?;
                Ok(())
            })
            .unwrap();
        assert!(read_asset(&state, "owner", &asset.id).is_err());
        assert_eq!(
            state
                .db(
                    |db| Ok(db.query_row("SELECT count(*) FROM speech_assets", [], |r| r
                        .get::<_, i64>(0))?)
                )
                .unwrap(),
            0
        );
    }
}
