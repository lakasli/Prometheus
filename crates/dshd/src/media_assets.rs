//! Bounded, owner/conversation-scoped media. Model output never names a path or URL.
use crate::{
    error::{ApiError, ApiResult},
    state::{now, AppState},
};
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path, State},
    http::{header, StatusCode},
    response::Response,
    routing::get,
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const MAX_VIDEO: usize = 20 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Asset {
    pub id: String,
    pub kind: String,
    pub mime: String,
    pub sha256: String,
    pub bytes: usize,
    pub expires_at: i64,
}
pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS conversation_media_assets(id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,conversation_id TEXT NOT NULL REFERENCES conversations(id),context_floor INTEGER NOT NULL,kind TEXT NOT NULL,mime TEXT NOT NULL,sha256 TEXT NOT NULL,bytes BLOB NOT NULL,created_at INTEGER NOT NULL,expires_at INTEGER NOT NULL);CREATE INDEX IF NOT EXISTS media_asset_scope ON conversation_media_assets(owner_id,conversation_id,expires_at);")?;
    Ok(())
}
fn owns(db: &Connection, owner: &str, conversation: &str) -> anyhow::Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM conversations WHERE id=?1 AND owner_id=?2)",
        params![conversation, owner],
        |r| r.get(0),
    )?)
}
pub fn list_in(db: &Connection, owner: &str, conversation: &str) -> anyhow::Result<Vec<Asset>> {
    prune_in(db)?;
    if !owns(db, owner, conversation)? {
        return Ok(Vec::new());
    }
    let mut query=db.prepare("SELECT id,kind,mime,sha256,length(bytes),expires_at FROM conversation_media_assets WHERE owner_id=?1 AND conversation_id=?2 AND expires_at>?3 ORDER BY created_at DESC LIMIT 32")?;
    let rows = query.query_map(params![owner, conversation, now()], |r| {
        Ok(Asset {
            id: r.get(0)?,
            kind: r.get(1)?,
            mime: r.get(2)?,
            sha256: r.get(3)?,
            bytes: r.get(4)?,
            expires_at: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}
pub fn read(
    state: &AppState,
    owner: &str,
    conversation: &str,
    id: &str,
) -> ApiResult<(Asset, Vec<u8>)> {
    state.db(|db| {
        prune_in(db)?;
        let result=db.query_row("SELECT id,kind,mime,sha256,bytes,expires_at FROM conversation_media_assets a WHERE id=?1 AND owner_id=?2 AND conversation_id=?3 AND expires_at>?4 AND EXISTS(SELECT 1 FROM conversations c WHERE c.id=a.conversation_id AND c.owner_id=a.owner_id)",params![id,owner,conversation,now()],|r|{let data:Vec<u8>=r.get(4)?;Ok((Asset{id:r.get(0)?,kind:r.get(1)?,mime:r.get(2)?,sha256:r.get(3)?,bytes:data.len(),expires_at:r.get(5)?},data))}).optional()?;
        Ok(result)
    })?.ok_or_else(ApiError::not_found)
}
pub fn store(
    state: &AppState,
    owner: &str,
    conversation: &str,
    kind: &str,
    bytes: Vec<u8>,
) -> ApiResult<Asset> {
    let (mime, bytes) = match kind {
        "image" => (
            "image/png",
            crate::skill_drawing::canonical_png(&STANDARD.encode(bytes))
                .map_err(|_| ApiError::bad("invalid_png", "只允许受限大小、去除元数据的PNG图片"))?,
        ),
        "video" => {
            validate_mp4(&bytes).map_err(|_| {
                ApiError::bad("invalid_video", "视频须为20MiB以内、最长60秒的有效MP4")
            })?;
            ("video/mp4", bytes)
        }
        _ => {
            return Err(ApiError::bad(
                "invalid_media_kind",
                "仅支持PNG图片与MP4视频制品",
            ))
        }
    };
    let asset = Asset {
        id: uuid::Uuid::new_v4().to_string(),
        kind: kind.into(),
        mime: mime.into(),
        sha256: hex::encode(Sha256::digest(&bytes)),
        bytes: bytes.len(),
        expires_at: now() + 86400,
    };
    let stored=state.db(|db|{
        let tx=db.transaction()?;
        if !owns(&tx,owner,conversation)?{return Ok(false);}
        prune_in(&tx)?;
        let (count,total):(usize,usize)=tx.query_row("SELECT count(*),coalesce(sum(length(bytes)),0) FROM conversation_media_assets WHERE owner_id=?1",[owner],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let all:usize=tx.query_row("SELECT coalesce(sum(length(bytes)),0) FROM conversation_media_assets",[],|r|r.get(0))?;
        anyhow::ensure!(count<32 && total+bytes.len()<=64*1024*1024 && all+bytes.len()<=128*1024*1024,"media quota exceeded");
        let floor=crate::memory_context::history_floor(&tx,conversation)?;
        crate::chat_reply::require_reader(&tx,9)?;
        tx.execute("INSERT INTO conversation_media_assets(id,owner_id,conversation_id,context_floor,kind,mime,sha256,bytes,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![asset.id,owner,conversation,floor,kind,mime,asset.sha256,bytes,now(),asset.expires_at])?;
        tx.commit()?;Ok(true)
    })?;
    if !stored {
        return Err(ApiError::not_found());
    }
    Ok(asset)
}
pub fn prune_in(db: &Connection) -> anyhow::Result<()> {
    db.execute("DELETE FROM conversation_media_assets WHERE expires_at<=?1 OR NOT EXISTS(SELECT 1 FROM conversations c WHERE c.id=conversation_media_assets.conversation_id AND c.owner_id=conversation_media_assets.owner_id) OR context_floor<>coalesce((SELECT through_run_rowid FROM memory_context_watermarks w WHERE w.conversation_id=conversation_media_assets.conversation_id),0)",[now()])?;
    Ok(())
}

fn boxes(bytes: &[u8]) -> Result<Vec<(&[u8], &[u8])>, ()> {
    let mut found = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        if found.len() > 10000 || bytes.len() - offset < 8 {
            return Err(());
        }
        let size =
            u32::from_be_bytes(bytes[offset..offset + 4].try_into().map_err(|_| ())?) as usize;
        let (length, header) = if size == 1 {
            if bytes.len() - offset < 16 {
                return Err(());
            }
            (
                usize::try_from(u64::from_be_bytes(
                    bytes[offset + 8..offset + 16].try_into().map_err(|_| ())?,
                ))
                .map_err(|_| ())?,
                16,
            )
        } else if size == 0 {
            (bytes.len() - offset, 8)
        } else {
            (size, 8)
        };
        if length < header || length > bytes.len() - offset {
            return Err(());
        }
        found.push((
            &bytes[offset + 4..offset + 8],
            &bytes[offset + header..offset + length],
        ));
        offset += length;
    }
    Ok(found)
}
fn validate_mp4(bytes: &[u8]) -> Result<(), ()> {
    if bytes.len() > MAX_VIDEO || bytes.len() < 32 {
        return Err(());
    }
    let root = boxes(bytes)?;
    if !root
        .iter()
        .any(|(kind, data)| *kind == b"ftyp" && data.len() >= 8)
        || !root
            .iter()
            .any(|(kind, data)| *kind == b"mdat" && !data.is_empty())
    {
        return Err(());
    }
    let moovs = root
        .iter()
        .filter(|(kind, _)| *kind == b"moov")
        .collect::<Vec<_>>();
    if moovs.len() != 1 {
        return Err(());
    }
    let movie = boxes(moovs[0].1)?;
    let header = movie.iter().find(|(kind, _)| *kind == b"mvhd").ok_or(())?.1;
    let (timescale, duration) = match header.first() {
        Some(0) if header.len() >= 20 => (
            u32::from_be_bytes(header[12..16].try_into().map_err(|_| ())?) as u64,
            u32::from_be_bytes(header[16..20].try_into().map_err(|_| ())?) as u64,
        ),
        Some(1) if header.len() >= 32 => (
            u32::from_be_bytes(header[20..24].try_into().map_err(|_| ())?) as u64,
            u64::from_be_bytes(header[24..32].try_into().map_err(|_| ())?),
        ),
        _ => return Err(()),
    };
    if timescale == 0
        || duration == 0
        || duration > timescale.saturating_mul(60)
        || !movie.iter().any(|(kind, _)| *kind == b"trak")
    {
        return Err(());
    }
    Ok(())
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/admin/v1/conversations/{conversation}/media",
            get(list).post(upload),
        )
        .route(
            "/api/admin/v1/conversations/{conversation}/media/{id}",
            get(download),
        )
        .layer(DefaultBodyLimit::max(29 * 1024 * 1024))
}
async fn list(
    State(state): State<AppState>,
    admin: crate::auth::Admin,
    Path(conversation): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        json!({"items":state.db(|db|list_in(db,&admin.id,&conversation))?,"max_video_bytes":MAX_VIDEO,"retention_seconds":86400}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Upload {
    kind: String,
    data_base64: String,
}
async fn upload(
    State(state): State<AppState>,
    admin: crate::auth::Admin,
    Path(conversation): Path<String>,
    Json(body): Json<Upload>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if body.data_base64.len() > MAX_VIDEO.div_ceil(3) * 4 {
        return Err(ApiError::bad("media_too_large", "媒体超过20MiB限制"));
    }
    let bytes = STANDARD
        .decode(body.data_base64)
        .map_err(|_| ApiError::bad("invalid_base64", "媒体编码无效"))?;
    let asset = store(&state, &admin.id, &conversation, &body.kind, bytes)?;
    state.audit(&admin.id, "media.upload", &asset.id)?;
    Ok((StatusCode::CREATED, Json(json!(asset))))
}
async fn download(
    State(state): State<AppState>,
    admin: crate::auth::Admin,
    Path((conversation, id)): Path<(String, String)>,
) -> ApiResult<Response> {
    let (meta, bytes) = read(&state, &admin.id, &conversation, &id)?;
    Response::builder()
        .header(header::CONTENT_TYPE, meta.mime)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from(bytes))
        .map_err(|_| ApiError::internal())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_mp4_box_bounds_and_duration() {
        fn boxed(kind: &[u8], data: &[u8]) -> Vec<u8> {
            let mut out = ((data.len() + 8) as u32).to_be_bytes().to_vec();
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            out
        }
        let mut header = vec![0; 20];
        header[12..16].copy_from_slice(&1000u32.to_be_bytes());
        header[16..20].copy_from_slice(&2000u32.to_be_bytes());
        let moov = [boxed(b"mvhd", &header), boxed(b"trak", &[])].concat();
        let good = [
            boxed(b"ftyp", b"isom0000"),
            boxed(b"moov", &moov),
            boxed(b"mdat", b"frame"),
        ]
        .concat();
        assert!(validate_mp4(&good).is_ok());
        assert!(validate_mp4(&good[..good.len() - 1]).is_err());
        header[16..20].copy_from_slice(&61000u32.to_be_bytes());
        let bad = [
            boxed(b"ftyp", b"isom0000"),
            boxed(
                b"moov",
                &[boxed(b"mvhd", &header), boxed(b"trak", &[])].concat(),
            ),
            boxed(b"mdat", b"f"),
        ]
        .concat();
        assert!(validate_mp4(&bad).is_err());
    }
}
