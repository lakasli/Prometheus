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
/// A bounded, self-contained H.264 MP4 container; compressed frames are not
/// decoded by DSH and this type does not imply video understanding.
pub struct ValidatedVideo {
    bytes: Vec<u8>,
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
}
pub fn validate_video(bytes: Vec<u8>) -> ApiResult<ValidatedVideo> {
    let (duration_ms, width, height) = validate_mp4(&bytes).map_err(|_| {
        ApiError::bad(
            "invalid_video",
            "视频须为20MiB以内、最长60秒、自包含H.264轨道的标准MP4",
        )
    })?;
    Ok(ValidatedVideo {
        bytes,
        duration_ms,
        width,
        height,
    })
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Asset {
    pub id: String,
    pub kind: String,
    pub mime: String,
    pub sha256: String,
    pub bytes: usize,
    pub expires_at: i64,
}
/// Host-only provenance captured before materialization; never model supplied.
#[derive(Clone, Debug)]
pub struct MaterializationSource {
    pub run_id: String,
    pub expected_floor: i64,
}
pub fn source_in(
    db: &Connection,
    run_id: &str,
    owner: &str,
    conversation: &str,
) -> anyhow::Result<Option<MaterializationSource>> {
    let source = MaterializationSource {
        run_id: run_id.into(),
        expected_floor: crate::memory_context::history_floor(db, conversation)?,
    };
    Ok(source_active_in(db, &source, owner, conversation)?.then_some(source))
}
pub fn source_active_in(
    db: &Connection,
    source: &MaterializationSource,
    owner: &str,
    conversation: &str,
) -> anyhow::Result<bool> {
    let active: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM runs r JOIN conversations c ON c.id=r.conversation_id WHERE r.id=?1 AND r.owner_id=?2 AND r.conversation_id=?3 AND c.owner_id=?2 AND r.status='running')",
        params![source.run_id, owner, conversation], |r|r.get(0),
    )?;
    Ok(active && crate::memory_context::history_floor(db, conversation)? == source.expected_floor)
}
pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS conversation_media_assets(id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,conversation_id TEXT NOT NULL REFERENCES conversations(id),context_floor INTEGER NOT NULL,kind TEXT NOT NULL,mime TEXT NOT NULL,sha256 TEXT NOT NULL,bytes BLOB NOT NULL,created_at INTEGER NOT NULL,expires_at INTEGER NOT NULL);CREATE INDEX IF NOT EXISTS media_asset_scope ON conversation_media_assets(owner_id,conversation_id,expires_at);")?;
    let present = {
        let mut stmt = db.prepare("PRAGMA table_info(conversation_media_assets)")?;
        let names = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        names.iter().any(|name| name == "source_run_id")
    };
    if !present {
        db.execute("ALTER TABLE conversation_media_assets ADD COLUMN source_run_id TEXT REFERENCES runs(id)",[])?;
    }
    Ok(())
}
fn owns(db: &Connection, owner: &str, conversation: &str) -> anyhow::Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM conversations WHERE id=?1 AND owner_id=?2)",
        params![conversation, owner],
        |r| r.get(0),
    )?)
}
fn upload_allowed_in(db: &Connection, owner: &str, conversation: &str) -> ApiResult<()> {
    if !owns(db, owner, conversation).map_err(|_| ApiError::internal())? {
        return Err(ApiError::not_found());
    }
    let draft:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM conversation_character_bindings WHERE conversation_id=?1 AND draft_card_json IS NOT NULL)",[conversation],|r|r.get(0)).map_err(|_|ApiError::internal())?;
    if draft {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "conversation_media_draft_test",
            "角色草稿测试暂不支持私有媒体附件，请使用普通会话",
        ));
    }
    Ok(())
}
pub fn list_in(db: &Connection, owner: &str, conversation: &str) -> anyhow::Result<Vec<Asset>> {
    prune_in(db)?;
    if !owns(db, owner, conversation)? {
        return Ok(Vec::new());
    }
    let mut query=db.prepare("SELECT id,kind,mime,sha256,length(bytes),expires_at FROM conversation_media_assets a WHERE owner_id=?1 AND conversation_id=?2 AND expires_at>?3 AND (source_run_id IS NULL OR EXISTS(SELECT 1 FROM runs r WHERE r.id=a.source_run_id AND r.owner_id=a.owner_id AND r.conversation_id=a.conversation_id AND r.status='completed')) ORDER BY a.created_at DESC,a.rowid DESC LIMIT 32")?;
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
        let result=db.query_row("SELECT id,kind,mime,sha256,bytes,expires_at FROM conversation_media_assets a WHERE id=?1 AND owner_id=?2 AND conversation_id=?3 AND expires_at>?4 AND EXISTS(SELECT 1 FROM conversations c WHERE c.id=a.conversation_id AND c.owner_id=a.owner_id) AND (source_run_id IS NULL OR EXISTS(SELECT 1 FROM runs r WHERE r.id=a.source_run_id AND r.owner_id=a.owner_id AND r.conversation_id=a.conversation_id AND r.status='completed'))",params![id,owner,conversation,now()],|r|{let data:Vec<u8>=r.get(4)?;Ok((Asset{id:r.get(0)?,kind:r.get(1)?,mime:r.get(2)?,sha256:r.get(3)?,bytes:data.len(),expires_at:r.get(5)?},data))}).optional()?;
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
    state.db(|db| Ok(upload_allowed_in(db, owner, conversation)))??;
    let bytes = if kind == "image" {
        crate::image_analysis::canonicalize(&STANDARD.encode(bytes))?.bytes
    } else {
        bytes
    };
    store_inner(state, owner, conversation, kind, bytes, None)
}
pub fn store_for_run(
    state: &AppState,
    owner: &str,
    conversation: &str,
    kind: &str,
    bytes: Vec<u8>,
    source: &MaterializationSource,
) -> ApiResult<Asset> {
    store_inner(state, owner, conversation, kind, bytes, Some(source))
}
fn store_inner(
    state: &AppState,
    owner: &str,
    conversation: &str,
    kind: &str,
    bytes: Vec<u8>,
    source: Option<&MaterializationSource>,
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
    state.db(|db| {
        let tx = db.transaction()?;
        if let Err(error) = upload_allowed_in(&tx, owner, conversation) {
            return Ok(Err(error));
        }
        if let Some(source) = source {
            if !source_active_in(&tx, source, owner, conversation)? {
                return Ok(Err(ApiError::not_found()));
            }
        }
        let asset = store_verified_in(&tx, owner, conversation, kind, mime, &bytes, source)?;
        tx.commit()?;
        Ok(Ok(asset))
    })?
}
/// Reuses the existing image validator's typed canonical PNG inside the caller's
/// Inbox/Run transaction. No nested state lock and no duplicate pixel decoding.
pub(crate) fn store_png_in(
    db: &Connection,
    owner: &str,
    conversation: &str,
    image: &crate::image_analysis::CanonicalImage,
) -> anyhow::Result<Asset> {
    anyhow::ensure!(
        image.bytes.len() <= 4 * 1024 * 1024
            && image.bytes.starts_with(b"\x89PNG\r\n\x1a\n")
            && image.width > 0
            && image.height > 0
            && image.width <= 2048
            && image.height <= 2048,
        "invalid canonical PNG reference"
    );
    anyhow::ensure!(
        hex::encode(Sha256::digest(&image.bytes)) == image.sha256,
        "canonical PNG digest mismatch"
    );
    store_verified_in(
        db,
        owner,
        conversation,
        "image",
        "image/png",
        &image.bytes,
        None,
    )
}
pub(crate) fn store_video_in(
    db: &Connection,
    owner: &str,
    conversation: &str,
    video: &ValidatedVideo,
) -> anyhow::Result<Asset> {
    store_verified_in(
        db,
        owner,
        conversation,
        "video",
        "video/mp4",
        &video.bytes,
        None,
    )
}
fn store_verified_in(
    db: &Connection,
    owner: &str,
    conversation: &str,
    kind: &str,
    mime: &str,
    bytes: &[u8],
    source: Option<&MaterializationSource>,
) -> anyhow::Result<Asset> {
    anyhow::ensure!(
        !db.is_autocommit(),
        "media storage requires caller transaction"
    );
    anyhow::ensure!(owns(db, owner, conversation)?, "media owner mismatch");
    if let Some(source) = source {
        anyhow::ensure!(
            source_active_in(db, source, owner, conversation)?,
            "media source is no longer active"
        );
    }
    let asset = Asset {
        id: uuid::Uuid::new_v4().to_string(),
        kind: kind.into(),
        mime: mime.into(),
        sha256: hex::encode(Sha256::digest(&bytes)),
        bytes: bytes.len(),
        expires_at: now() + 86400,
    };
    prune_in(db)?;
    let (count,total):(usize,usize)=db.query_row("SELECT count(*),coalesce(sum(length(bytes)),0) FROM conversation_media_assets WHERE owner_id=?1",[owner],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let all: usize = db.query_row(
        "SELECT coalesce(sum(length(bytes)),0) FROM conversation_media_assets",
        [],
        |r| r.get(0),
    )?;
    anyhow::ensure!(
        count < 32
            && total + bytes.len() <= 64 * 1024 * 1024
            && all + bytes.len() <= 128 * 1024 * 1024,
        "media quota exceeded"
    );
    let floor = crate::memory_context::history_floor(db, conversation)?;
    crate::chat_reply::require_reader(db, if source.is_some() { 10 } else { 9 })?;
    db.execute("INSERT INTO conversation_media_assets(id,owner_id,conversation_id,context_floor,kind,mime,sha256,bytes,created_at,expires_at,source_run_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",params![asset.id,owner,conversation,floor,kind,mime,asset.sha256,bytes,now(),asset.expires_at,source.map(|s|s.run_id.as_str())])?;
    Ok(asset)
}
pub fn prune_in(db: &Connection) -> anyhow::Result<()> {
    db.execute("DELETE FROM conversation_media_assets WHERE expires_at<=?1 OR NOT EXISTS(SELECT 1 FROM conversations c WHERE c.id=conversation_media_assets.conversation_id AND c.owner_id=conversation_media_assets.owner_id) OR context_floor<>coalesce((SELECT through_run_rowid FROM memory_context_watermarks w WHERE w.conversation_id=conversation_media_assets.conversation_id),0) OR (source_run_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM runs r WHERE r.id=conversation_media_assets.source_run_id AND r.owner_id=conversation_media_assets.owner_id AND r.conversation_id=conversation_media_assets.conversation_id AND r.status IN ('running','completed')))",[now()])?;
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
fn one<'a>(items: &[(&[u8], &'a [u8])], kind: &[u8]) -> Result<&'a [u8], ()> {
    let mut found = items.iter().filter(|(name, _)| *name == kind);
    let value = found.next().ok_or(())?.1;
    if found.next().is_some() {
        return Err(());
    }
    Ok(value)
}
fn be32(bytes: &[u8], at: usize) -> Result<u32, ()> {
    Ok(u32::from_be_bytes(
        bytes
            .get(at..at + 4)
            .ok_or(())?
            .try_into()
            .map_err(|_| ())?,
    ))
}
fn be64(bytes: &[u8], at: usize) -> Result<u64, ()> {
    Ok(u64::from_be_bytes(
        bytes
            .get(at..at + 8)
            .ok_or(())?
            .try_into()
            .map_err(|_| ())?,
    ))
}
fn media_time(header: &[u8]) -> Result<(u64, u64), ()> {
    let (scale, duration) = match header.first() {
        Some(0) => (be32(header, 12)? as u64, be32(header, 16)? as u64),
        Some(1) => (be32(header, 20)? as u64, be64(header, 24)?),
        _ => return Err(()),
    };
    if scale == 0 || duration == 0 || duration > scale.saturating_mul(60) {
        return Err(());
    }
    Ok((scale, duration))
}
fn avc_config(bytes: &[u8]) -> Result<(), ()> {
    if bytes.len() < 7 || bytes[0] != 1 || bytes[4] & 3 == 2 {
        return Err(());
    }
    let mut at = 6;
    let sps = bytes[5] & 31;
    if sps == 0 {
        return Err(());
    }
    for _ in 0..sps {
        let size = u16::from_be_bytes(
            bytes
                .get(at..at + 2)
                .ok_or(())?
                .try_into()
                .map_err(|_| ())?,
        ) as usize;
        at = at.checked_add(2).ok_or(())?;
        if size == 0 || bytes.get(at..at + size).is_none() || bytes[at] & 31 != 7 {
            return Err(());
        }
        at += size;
    }
    let pps = *bytes.get(at).ok_or(())?;
    at += 1;
    if pps == 0 {
        return Err(());
    }
    for _ in 0..pps {
        let size = u16::from_be_bytes(
            bytes
                .get(at..at + 2)
                .ok_or(())?
                .try_into()
                .map_err(|_| ())?,
        ) as usize;
        at += 2;
        if size == 0 || bytes.get(at..at + size).is_none() || bytes[at] & 31 != 8 {
            return Err(());
        }
        at += size;
    }
    Ok(())
}
fn sample_ranges(
    table: &[(&[u8], &[u8])],
    scale: u64,
    mdat: &[(usize, usize)],
) -> Result<Vec<(usize, usize)>, ()> {
    let sizes = one(table, b"stsz")?;
    let fixed = be32(sizes, 4)? as usize;
    let count = be32(sizes, 8)? as usize;
    if count == 0 || count > 60000 || sizes.len() != 12 + if fixed == 0 { count * 4 } else { 0 } {
        return Err(());
    }
    let sizes = (0..count)
        .map(|i| {
            if fixed == 0 {
                be32(sizes, 12 + i * 4).map(|v| v as usize)
            } else {
                Ok(fixed)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    if sizes.iter().any(|n| *n == 0 || *n > MAX_VIDEO) {
        return Err(());
    }
    let timing = one(table, b"stts")?;
    let entries = be32(timing, 4)? as usize;
    if entries == 0 || entries > count || timing.len() != 8 + entries * 8 {
        return Err(());
    }
    let mut samples = 0u64;
    let mut duration = 0u64;
    for i in 0..entries {
        let n = be32(timing, 8 + i * 8)? as u64;
        let delta = be32(timing, 12 + i * 8)? as u64;
        if n == 0 || delta == 0 {
            return Err(());
        }
        samples = samples.checked_add(n).ok_or(())?;
        duration = duration
            .checked_add(n.checked_mul(delta).ok_or(())?)
            .ok_or(())?;
    }
    if samples != count as u64 || duration > scale.saturating_mul(60) {
        return Err(());
    }
    let offsets = table
        .iter()
        .filter(|(k, _)| *k == b"stco" || *k == b"co64")
        .collect::<Vec<_>>();
    if offsets.len() != 1 {
        return Err(());
    }
    let width = if offsets[0].0 == b"co64" { 8 } else { 4 };
    let offsets = offsets[0].1;
    let chunks = be32(offsets, 4)? as usize;
    if chunks == 0 || chunks > 8192 || chunks > count || offsets.len() != 8 + chunks * width {
        return Err(());
    }
    let mapping = one(table, b"stsc")?;
    let entries = be32(mapping, 4)? as usize;
    if entries == 0 || entries > chunks || mapping.len() != 8 + entries * 12 {
        return Err(());
    }
    let mut map = Vec::with_capacity(entries);
    for i in 0..entries {
        let first = be32(mapping, 8 + i * 12)? as usize;
        let n = be32(mapping, 12 + i * 12)? as usize;
        if first == 0
            || first > chunks
            || (i == 0 && first != 1)
            || map.last().is_some_and(|(previous, _)| *previous >= first)
            || n == 0
            || n > count
            || be32(mapping, 16 + i * 12)? != 1
        {
            return Err(());
        }
        map.push((first, n));
    }
    let mut ranges = Vec::with_capacity(chunks);
    let mut sample = 0usize;
    let mut mapping_index = 0;
    for chunk in 0..chunks {
        while mapping_index + 1 < map.len() && map[mapping_index + 1].0 <= chunk + 1 {
            mapping_index += 1;
        }
        let n = map[mapping_index].1;
        let data = sizes
            .get(sample..sample.checked_add(n).ok_or(())?)
            .ok_or(())?;
        sample += n;
        let length = data
            .iter()
            .try_fold(0usize, |total, n| total.checked_add(*n).ok_or(()))?;
        let offset = if width == 8 {
            usize::try_from(be64(offsets, 8 + chunk * width)?).map_err(|_| ())?
        } else {
            be32(offsets, 8 + chunk * width)? as usize
        };
        let end = offset.checked_add(length).ok_or(())?;
        if !mdat
            .iter()
            .any(|(start, limit)| offset >= *start && end <= *limit)
        {
            return Err(());
        }
        ranges.push((offset, end));
    }
    if sample != count {
        return Err(());
    }
    Ok(ranges)
}
fn validate_mp4(bytes: &[u8]) -> Result<(u64, u32, u32), ()> {
    if bytes.len() > MAX_VIDEO || bytes.len() < 32 {
        return Err(());
    }
    let root = boxes(bytes)?;
    if root.first().is_none_or(|(kind, _)| *kind != b"ftyp") {
        return Err(());
    }
    let ftyp = one(&root, b"ftyp")?;
    if ftyp.len() < 8
        || ftyp.len() % 4 != 0
        || !ftyp
            .chunks_exact(4)
            .enumerate()
            .filter(|(i, _)| *i != 1)
            .any(|(_, brand)| matches!(brand, b"isom" | b"iso2" | b"mp41" | b"mp42" | b"avc1"))
        || root.iter().any(|(k, _)| *k == b"moof" || *k == b"sidx")
    {
        return Err(());
    }
    let mdat = root
        .iter()
        .filter(|(k, d)| *k == b"mdat" && !d.is_empty())
        .map(|(_, data)| {
            let start = data.as_ptr() as usize - bytes.as_ptr() as usize;
            (start, start + data.len())
        })
        .collect::<Vec<_>>();
    if mdat.is_empty() {
        return Err(());
    }
    let movie = boxes(one(&root, b"moov")?)?;
    let (scale, duration) = media_time(one(&movie, b"mvhd")?)?;
    let tracks = movie
        .iter()
        .filter(|(k, _)| *k == b"trak")
        .collect::<Vec<_>>();
    if tracks.is_empty() || tracks.len() > 4 {
        return Err(());
    }
    let mut video = None;
    let mut ranges = Vec::new();
    for (_, track) in tracks {
        let track = boxes(track)?;
        let mdia = boxes(one(&track, b"mdia")?)?;
        let (track_scale, _) = media_time(one(&mdia, b"mdhd")?)?;
        let handler = one(&mdia, b"hdlr")?.get(8..12).ok_or(())?;
        let minf = boxes(one(&mdia, b"minf")?)?;
        let dinf = boxes(one(&minf, b"dinf")?)?;
        let dref = one(&dinf, b"dref")?;
        if be32(dref, 4)? != 1 {
            return Err(());
        }
        let references = boxes(dref.get(8..).ok_or(())?)?;
        let reference = one(&references, b"url ")?;
        if references.len() != 1 || reference != [0, 0, 0, 1] {
            return Err(());
        }
        let table = boxes(one(&minf, b"stbl")?)?;
        let description = one(&table, b"stsd")?;
        if be32(description, 4)? != 1 {
            return Err(());
        }
        let descriptions = boxes(description.get(8..).ok_or(())?)?;
        if descriptions.len() != 1 {
            return Err(());
        }
        let (codec, data) = descriptions[0];
        if data.get(6..8) != Some(&[0, 1]) {
            return Err(());
        }
        if handler == b"vide" {
            if video.is_some() || !matches!(codec, b"avc1" | b"avc3") || data.len() < 78 {
                return Err(());
            }
            let width = u16::from_be_bytes(data[24..26].try_into().map_err(|_| ())?) as u32;
            let height = u16::from_be_bytes(data[26..28].try_into().map_err(|_| ())?) as u32;
            if width == 0
                || height == 0
                || width > 4096
                || height > 4096
                || width as u64 * height as u64 > 8_294_400
            {
                return Err(());
            }
            avc_config(one(&boxes(&data[78..])?, b"avcC")?)?;
            video = Some((width, height));
        } else if handler == b"soun" {
            if codec != b"mp4a" || data.len() < 28 {
                return Err(());
            }
            one(&boxes(&data[28..])?, b"esds")?;
        } else {
            return Err(());
        }
        ranges.extend(sample_ranges(&table, track_scale, &mdat)?);
    }
    ranges.sort_unstable();
    if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(());
    }
    let (width, height) = video.ok_or(())?;
    Ok((duration * 1000 / scale, width, height))
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
    // Decode in the same bounded slot as inbound chat images, outside Tokio's
    // async executor and outside a database transaction.
    let permit = crate::image_analysis_api::DECODE_SLOTS
        .try_acquire()
        .map_err(|_| {
            ApiError::new(
                StatusCode::CONFLICT,
                "image_analysis_busy",
                "图片正在处理，请稍后再试",
            )
        })?;
    let task_state = state.clone();
    let task_owner = admin.id.clone();
    let asset = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        store(&task_state, &task_owner, &conversation, &body.kind, bytes)
    })
    .await
    .map_err(|_| ApiError::internal())??;
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
    fn same_second_media_order_matches_newest_upload_and_draft_scope_is_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::open(crate::state::Options {
            data_dir: dir.path().into(),
            origin: "http://127.0.0.1:18419".into(),
            allow_private_model: false,
            cookie_secure: false,
            web_dir: dir.path().into(),
        })
        .unwrap();
        state.db(|db|{
            db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('normal','owner','normal',0),('draft','owner','draft',0),('peer-normal','peer','peer',0)",[])?;
            db.execute("INSERT INTO character_cards(id,created_by,created_at) VALUES('draft-card','owner',0)",[])?;
            db.execute("INSERT INTO conversation_character_bindings(conversation_id,card_id,revision,draft_card_json) VALUES('draft','draft-card',0,'{}')",[])?;
            Ok(())
        }).unwrap();
        let first = store(&state, "owner", "normal", "image", picture()).unwrap();
        let second = store(&state, "owner", "normal", "image", picture()).unwrap();
        state.db(|db|{db.execute("UPDATE conversation_media_assets SET created_at=123 WHERE conversation_id='normal'",[])?;Ok(())}).unwrap();
        let ordered = state.db(|db| list_in(db, "owner", "normal")).unwrap();
        assert_eq!(
            ordered.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            vec![second.id.as_str(), first.id.as_str()]
        );
        let rejected = store(&state, "owner", "draft", "image", picture()).unwrap_err();
        assert_eq!(rejected.status, StatusCode::FORBIDDEN);
        assert_eq!(rejected.code, "conversation_media_draft_test");
        assert_eq!(
            store(&state, "peer", "normal", "image", picture())
                .unwrap_err()
                .status,
            StatusCode::NOT_FOUND
        );
        let peer = store(&state, "peer", "peer-normal", "image", picture()).unwrap();
        assert!(read(&state, "peer", "peer-normal", &peer.id).is_ok());
        assert_eq!(
            state
                .db(|db| Ok(db.query_row(
                    "SELECT count(*) FROM conversation_media_assets WHERE conversation_id='draft'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?))
                .unwrap(),
            0
        );
    }
    #[test]
    fn additive_source_columns_preserve_legacy_unbound_assets_and_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::open(crate::state::Options {
            data_dir: dir.path().into(),
            origin: "http://127.0.0.1:18419".into(),
            allow_private_model: false,
            cookie_secure: false,
            web_dir: dir.path().into(),
        })
        .unwrap();
        state.db(|db|{
            db.execute("INSERT INTO conversations VALUES('c','owner','fixture',NULL,0)",[])?;
            db.execute_batch("DROP TABLE conversation_media_assets; CREATE TABLE conversation_media_assets(id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,conversation_id TEXT NOT NULL REFERENCES conversations(id),context_floor INTEGER NOT NULL,kind TEXT NOT NULL,mime TEXT NOT NULL,sha256 TEXT NOT NULL,bytes BLOB NOT NULL,created_at INTEGER NOT NULL,expires_at INTEGER NOT NULL); DROP TABLE speech_assets; CREATE TABLE speech_assets(id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,conversation_id TEXT REFERENCES conversations(id),context_floor INTEGER NOT NULL,metadata_json TEXT NOT NULL,audio BLOB NOT NULL,created_at INTEGER NOT NULL,expires_at INTEGER NOT NULL);")?;
            db.execute("INSERT INTO conversation_media_assets VALUES('legacy-image','owner','c',0,'image','image/png','fixture',?1,0,?2)",params![picture(),now()+60])?;
            db.execute("INSERT INTO speech_assets VALUES('legacy-audio','owner',NULL,0,'{}',?1,0,?2)",params![b"fixture".as_slice(),now()+60])?;
            initialize(db)?;crate::speech::initialize(db)?;
            initialize(db)?;crate::speech::initialize(db)?;
            assert_eq!(db.query_row("SELECT count(*) FROM conversation_media_assets WHERE id='legacy-image' AND source_run_id IS NULL",[],|r|r.get::<_,i64>(0))?,1);
            assert_eq!(db.query_row("SELECT count(*) FROM speech_assets WHERE id='legacy-audio' AND source_run_id IS NULL",[],|r|r.get::<_,i64>(0))?,1);
            Ok(())
        }).unwrap();
    }
    fn picture() -> Vec<u8> {
        let mut result = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut result, 1, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[10, 20, 30])
                .unwrap();
        }
        result
    }
    #[test]
    fn generated_media_cannot_relabel_cancelled_or_forgotten_source_with_new_floor() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::open(crate::state::Options {
            data_dir: dir.path().into(),
            origin: "http://127.0.0.1:18419".into(),
            allow_private_model: false,
            cookie_secure: false,
            web_dir: dir.path().into(),
        })
        .unwrap();
        state.db(|db|{
            db.execute("INSERT INTO conversations VALUES('c','owner','fixture',NULL,0)",[])?;
            db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at) VALUES('r','c','owner','r','q','{}','running',0,0)",[])?;
            Ok(())
        }).unwrap();
        let source = state
            .db(|db| source_in(db, "r", "owner", "c"))
            .unwrap()
            .unwrap();
        state
            .db(|db| {
                db.execute("UPDATE runs SET status='cancelled' WHERE id='r'", [])?;
                Ok(())
            })
            .unwrap();
        assert!(store_for_run(&state, "owner", "c", "image", picture(), &source).is_err());
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
        assert!(store_for_run(&state, "owner", "c", "image", picture(), &source).is_err());
        assert_eq!(
            state
                .db(|db| Ok(db.query_row(
                    "SELECT count(*) FROM conversation_media_assets",
                    [],
                    |r| r.get::<_, i64>(0)
                )?))
                .unwrap(),
            0
        );
        let fresh = state
            .db(|db| source_in(db, "r", "owner", "c"))
            .unwrap()
            .unwrap();
        let asset = store_for_run(&state, "owner", "c", "image", picture(), &fresh).unwrap();
        assert!(read(&state, "owner", "c", &asset.id).is_err());
        assert!(state.db(|db| list_in(db, "owner", "c")).unwrap().is_empty());
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
        state
            .db(|db| {
                db.execute("UPDATE runs SET status='completed' WHERE id='r'", [])?;
                Ok(())
            })
            .unwrap();
        assert!(read(&state, "owner", "c", &asset.id).is_ok());
        state.db(|db|{db.execute("UPDATE memory_context_watermarks SET through_run_rowid=2 WHERE conversation_id='c'",[])?;Ok(())}).unwrap();
        assert!(read(&state, "owner", "c", &asset.id).is_err());
        state
            .db(|db| {
                db.execute("UPDATE runs SET status='running' WHERE id='r'", [])?;
                Ok(())
            })
            .unwrap();
        let latest = state
            .db(|db| source_in(db, "r", "owner", "c"))
            .unwrap()
            .unwrap();
        let orphan = store_for_run(&state, "owner", "c", "image", picture(), &latest).unwrap();
        state
            .db(|db| {
                db.execute("UPDATE runs SET status='cancelled' WHERE id='r'", [])?;
                Ok(())
            })
            .unwrap();
        assert!(read(&state, "owner", "c", &orphan.id).is_err());
        assert!(state.db(|db| list_in(db, "owner", "c")).unwrap().is_empty());
    }
    #[test]
    fn validates_mp4_box_bounds_and_duration() {
        let good = include_bytes!("../testdata/synthetic-h264.mp4");
        assert_eq!(validate_mp4(good).unwrap(), (2000, 320, 240));
        assert!(validate_mp4(&good[..good.len() - 1]).is_err());
        let at = good.windows(4).position(|b| b == b"mvhd").unwrap();
        let mut bad = good.to_vec();
        bad[at + 20..at + 24].copy_from_slice(&61000u32.to_be_bytes());
        assert!(validate_mp4(&bad).is_err());
        let at = good.windows(4).position(|b| b == b"stco").unwrap();
        let mut bad = good.to_vec();
        bad[at + 12..at + 16].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(validate_mp4(&bad).is_err());
        let at = good.windows(4).position(|b| b == b"url ").unwrap();
        let mut bad = good.to_vec();
        bad[at + 4..at + 8].fill(0);
        assert!(validate_mp4(&bad).is_err());
        let at = good.windows(4).position(|b| b == b"avcC").unwrap();
        let mut bad = good.to_vec();
        bad[at..at + 4].copy_from_slice(b"junk");
        assert!(validate_mp4(&bad).is_err());
    }
}
