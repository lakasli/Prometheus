//! Versioned, locally reviewed civil calendars. No runtime URL fetching.
use chrono::{Datelike, NaiveDate, Weekday};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const CALENDAR_ID: &str = "cn-official";
pub const CALENDAR_ZONE: &str = "Asia/Shanghai";
pub const OFFICIAL_2026_SOURCE: &str =
    "https://www.beijing.gov.cn/zhengce/zhengcefagui/202511/t20251104_4258873.html";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarSource {
    pub authority: String,
    pub document_number: String,
    pub published_at: String,
    pub url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarDocument {
    pub year: i32,
    pub time_zone: String,
    pub holidays: Vec<String>,
    pub working_days: Vec<String>,
    pub source: CalendarSource,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CalendarRevision {
    pub id: String,
    pub calendar_id: String,
    pub revision: i64,
    pub document: CalendarDocument,
    pub sha256: String,
    pub active: bool,
    pub verified_by: Option<String>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Default)]
pub struct CalendarSet {
    pub years: BTreeMap<i32, CalendarRevision>,
}
impl CalendarSet {
    pub fn workday(&self, date: NaiveDate) -> Result<(bool, i64), String> {
        let revision = self
            .years
            .get(&date.year())
            .ok_or_else(|| format!("calendar_unknown:{}", date.year()))?;
        let value = date.format("%Y-%m-%d").to_string();
        let workday = if revision.document.working_days.contains(&value) {
            true
        } else if revision.document.holidays.contains(&value) {
            false
        } else {
            !matches!(date.weekday(), Weekday::Sat | Weekday::Sun)
        };
        Ok((workday, revision.revision))
    }
    pub fn coverage_end(&self, start_year: i32) -> Option<String> {
        let mut year = start_year;
        if !self.years.contains_key(&year) {
            return None;
        }
        while self.years.contains_key(&(year + 1)) {
            year += 1;
        }
        Some(format!("{year}-12-31"))
    }
}

pub fn parse_date(value: &str) -> Result<NaiveDate, String> {
    if value.len() != 10
        || value.as_bytes().get(4) != Some(&b'-')
        || value.as_bytes().get(7) != Some(&b'-')
    {
        return Err("日期须为 YYYY-MM-DD".into());
    }
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| "日期无效")?;
    if date.format("%Y-%m-%d").to_string() != value || !(1970..=2100).contains(&date.year()) {
        return Err("日期须在1970到2100年间".into());
    }
    Ok(date)
}

pub fn validate(document: &CalendarDocument) -> Result<(), String> {
    if !(1970..=2100).contains(&document.year)
        || document.time_zone != CALENDAR_ZONE
        || document.holidays.len() > 366
        || document.working_days.len() > 366
        || document.source.authority.trim().is_empty()
        || document.source.authority.len() > 256
        || document.source.document_number.trim().is_empty()
        || document.source.document_number.len() > 256
        || document.source.url.len() > 2048
    {
        return Err("日历年份、时区、来源或日期数量无效".into());
    }
    parse_date(&document.source.published_at)?;
    let url = url::Url::parse(&document.source.url).map_err(|_| "日历来源URL无效")?;
    let host = url.host_str().ok_or("日历来源URL缺少主机")?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !(host == "gov.cn" || host.ends_with(".gov.cn"))
    {
        return Err("中国官方日历的来源须为不含账户和查询参数的HTTPS gov.cn资料地址".into());
    }
    let mut days = BTreeSet::new();
    for day in document.holidays.iter().chain(document.working_days.iter()) {
        if parse_date(day)?.year() != document.year || !days.insert(day) {
            return Err("日期必须属于该年度、去重，且放假与补班不得冲突".into());
        }
    }
    Ok(())
}

fn canonical(mut document: CalendarDocument) -> Result<(CalendarDocument, String), String> {
    validate(&document)?;
    document.holidays.sort();
    document.working_days.sort();
    let encoded = serde_json::to_vec(&document).map_err(|_| "日历编码失败")?;
    let digest = hex::encode(Sha256::digest(encoded));
    Ok((document, digest))
}

pub fn bundled_2026() -> CalendarDocument {
    let mut holidays = Vec::new();
    for (month, from, to) in [
        (1, 1, 3),
        (2, 15, 23),
        (4, 4, 6),
        (5, 1, 5),
        (6, 19, 21),
        (9, 25, 27),
        (10, 1, 7),
    ] {
        for day in from..=to {
            holidays.push(format!("2026-{month:02}-{day:02}"));
        }
    }
    CalendarDocument {
        year: 2026,
        time_zone: CALENDAR_ZONE.into(),
        holidays,
        working_days: [
            "2026-01-04",
            "2026-02-14",
            "2026-02-28",
            "2026-05-09",
            "2026-09-20",
            "2026-10-10",
        ]
        .map(str::to_owned)
        .to_vec(),
        source: CalendarSource {
            authority: "国务院办公厅（北京市政府官方转载）".into(),
            document_number: "国办发明电〔2025〕7号".into(),
            published_at: "2025-11-04".into(),
            url: OFFICIAL_2026_SOURCE.into(),
        },
    }
}

pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS trigger_calendar_revisions(id TEXT PRIMARY KEY,calendar_id TEXT NOT NULL,revision INTEGER NOT NULL,year INTEGER NOT NULL,document_json TEXT NOT NULL,sha256 TEXT NOT NULL,active INTEGER NOT NULL DEFAULT 0,verified_by TEXT,created_at INTEGER NOT NULL,UNIQUE(calendar_id,revision)); CREATE UNIQUE INDEX IF NOT EXISTS trigger_calendar_active_year ON trigger_calendar_revisions(calendar_id,year) WHERE active=1;")?;
    let seeded: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM settings WHERE key='trigger_calendar_2026_seed_v1')",
        [],
        |r| r.get(0),
    )?;
    if !seeded {
        let (document, digest) = canonical(bundled_2026()).map_err(anyhow::Error::msg)?;
        let tx = db.unchecked_transaction()?;
        tx.execute("INSERT OR IGNORE INTO trigger_calendar_revisions(id,calendar_id,revision,year,document_json,sha256,active,verified_by,created_at) VALUES('cn-official-2026-bundled-v1','cn-official',1,2026,?1,?2,1,'bundled-official-review',?3)",params![serde_json::to_string(&document)?,digest,crate::state::now()])?;
        tx.execute(
            "INSERT INTO settings(key,value) VALUES('trigger_calendar_2026_seed_v1','1')",
            [],
        )?;
        tx.commit()?;
    }
    Ok(())
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CalendarRevision> {
    let text: String = row.get(3)?;
    let document = serde_json::from_str(&text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(CalendarRevision {
        id: row.get(0)?,
        calendar_id: row.get(1)?,
        revision: row.get(2)?,
        document,
        sha256: row.get(4)?,
        active: row.get(5)?,
        verified_by: row.get(6)?,
        created_at: row.get(7)?,
    })
}

pub fn list(db: &Connection) -> anyhow::Result<Vec<CalendarRevision>> {
    let mut stmt=db.prepare("SELECT id,calendar_id,revision,document_json,sha256,active,verified_by,created_at FROM trigger_calendar_revisions ORDER BY revision DESC LIMIT 256")?;
    let values = stmt.query_map([], row)?.collect::<Result<Vec<_>, _>>()?;
    Ok(values)
}
pub fn load(db: &Connection) -> anyhow::Result<CalendarSet> {
    let mut set = CalendarSet::default();
    for revision in list(db)?
        .into_iter()
        .filter(|r| r.active && r.verified_by.is_some())
    {
        let (_, digest) = canonical(revision.document.clone()).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            digest == revision.sha256,
            "calendar content digest mismatch"
        );
        set.years.insert(revision.document.year, revision);
    }
    Ok(set)
}
pub fn get(db: &Connection, id: &str) -> anyhow::Result<Option<CalendarRevision>> {
    Ok(db.query_row("SELECT id,calendar_id,revision,document_json,sha256,active,verified_by,created_at FROM trigger_calendar_revisions WHERE id=?1",[id],row).optional()?)
}

/// Returns an unverified inactive revision. The API must audit its actor.
pub fn create_draft(
    db: &Connection,
    document: CalendarDocument,
) -> anyhow::Result<CalendarRevision> {
    let (document, digest) = canonical(document).map_err(anyhow::Error::msg)?;
    let count: i64 = db.query_row("SELECT count(*) FROM trigger_calendar_revisions", [], |r| {
        r.get(0)
    })?;
    anyhow::ensure!(count < 256, "日历版本数量已达上限");
    let revision:i64=db.query_row("SELECT coalesce(max(revision),0)+1 FROM trigger_calendar_revisions WHERE calendar_id='cn-official'",[],|r|r.get(0))?;
    let id = uuid::Uuid::new_v4().to_string();
    db.execute("INSERT INTO trigger_calendar_revisions(id,calendar_id,revision,year,document_json,sha256,active,created_at) VALUES(?1,'cn-official',?2,?3,?4,?5,0,?6)",params![id,revision,document.year,serde_json::to_string(&document)?,digest,crate::state::now()])?;
    get(db, &id)?.ok_or_else(|| anyhow::anyhow!("calendar revision unavailable"))
}

/// Call inside the caller's transaction after verifying reviewed digest and revision.
pub fn apply(
    db: &Connection,
    id: &str,
    expected_digest: &str,
    actor: &str,
) -> anyhow::Result<CalendarRevision> {
    anyhow::ensure!(
        !db.is_autocommit(),
        "calendar apply requires an outer transaction"
    );
    let revision = get(db, id)?.ok_or_else(|| anyhow::anyhow!("calendar not found"))?;
    let (_, digest) = canonical(revision.document.clone()).map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        digest == revision.sha256,
        "calendar content digest mismatch"
    );
    anyhow::ensure!(
        revision.sha256 == expected_digest,
        "calendar digest conflict"
    );
    db.execute("UPDATE trigger_calendar_revisions SET active=0 WHERE calendar_id=?1 AND year=?2 AND active=1",params![revision.calendar_id,revision.document.year])?;
    db.execute(
        "UPDATE trigger_calendar_revisions SET active=1,verified_by=?1 WHERE id=?2",
        params![actor, id],
    )?;
    get(db, id)?.ok_or_else(|| anyhow::anyhow!("calendar revision unavailable"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT);")
            .unwrap();
        initialize(&db).unwrap();
        db
    }
    #[test]
    fn official_calendar_distinguishes_weekend_makeup_holiday_and_unknown_year() {
        let db = db();
        let set = load(&db).unwrap();
        assert_eq!(
            set.workday(parse_date("2026-10-10").unwrap()).unwrap(),
            (true, 1)
        );
        assert_eq!(
            set.workday(parse_date("2026-10-01").unwrap()).unwrap(),
            (false, 1)
        );
        assert!(set
            .workday(parse_date("2027-01-04").unwrap())
            .unwrap_err()
            .contains("calendar_unknown"));
        assert_eq!(set.coverage_end(2026).as_deref(), Some("2026-12-31"));
    }
    #[test]
    fn import_is_bounded_immutable_and_requires_reviewed_apply() {
        let db = db();
        let mut doc = bundled_2026();
        doc.year = 2027;
        doc.holidays = vec!["2027-01-01".into()];
        doc.working_days.clear();
        let draft = create_draft(&db, doc.clone()).unwrap();
        assert!(!load(&db).unwrap().years.contains_key(&2027));
        assert!(apply(&db, &draft.id, &draft.sha256, "owner").is_err());
        let tx = db.unchecked_transaction().unwrap();
        assert!(apply(&tx, &draft.id, "wrong", "owner").is_err());
        tx.rollback().unwrap();
        let tx = db.unchecked_transaction().unwrap();
        apply(&tx, &draft.id, &draft.sha256, "owner").unwrap();
        tx.commit().unwrap();
        assert!(load(&db).unwrap().years.contains_key(&2027));
        doc.working_days = doc.holidays.clone();
        assert!(create_draft(&db, doc).is_err());
        initialize(&db).unwrap();
        assert_eq!(list(&db).unwrap().len(), 2);
    }
}
