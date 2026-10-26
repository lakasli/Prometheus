//! Immutable daily-parent snapshots and resumable bounded model reductions.
use super::*;

pub(super) const INPUT_BYTES: usize = 48000;
pub(super) fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS diary_aggregation_coverage(entry_id TEXT PRIMARY KEY,expected_parents INTEGER NOT NULL,present_parents INTEGER NOT NULL,covered_sources INTEGER NOT NULL,parents_complete INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS diary_entry_version_coverage(entry_id TEXT NOT NULL,revision INTEGER NOT NULL,complete_raw INTEGER NOT NULL,PRIMARY KEY(entry_id,revision));
      CREATE TABLE IF NOT EXISTS diary_model_reductions(entry_id TEXT NOT NULL,entry_revision INTEGER NOT NULL,level INTEGER NOT NULL,PRIMARY KEY(entry_id,entry_revision));
      CREATE TABLE IF NOT EXISTS diary_model_fragments(entry_id TEXT NOT NULL,entry_revision INTEGER NOT NULL,level INTEGER NOT NULL,ordinal INTEGER NOT NULL,unit_start INTEGER NOT NULL,unit_end INTEGER NOT NULL,total_units INTEGER NOT NULL,input_digest TEXT NOT NULL,facts TEXT NOT NULL,feelings TEXT NOT NULL,model TEXT NOT NULL,created_at INTEGER NOT NULL,PRIMARY KEY(entry_id,entry_revision,level,ordinal));")?;
    db.execute("INSERT OR IGNORE INTO diary_entry_version_coverage SELECT entry_id,revision,CASE WHEN generator LIKE 'model:%' THEN 1 WHEN instr(facts,'[摘录达到正文预算')>0 THEN 0 ELSE 1 END FROM diary_entry_versions WHERE facts IS NOT NULL",[])?;
    Ok(())
}
pub(super) fn clear(db: &Connection, id: &str) -> ApiResult<()> {
    for table in [
        "diary_model_fragments",
        "diary_model_reductions",
        "diary_aggregation_coverage",
        "diary_entry_version_coverage",
    ] {
        db.execute(&format!("DELETE FROM {table} WHERE entry_id=?1"), [id])
            .api()?;
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParentReference {
    pub id: String,
    pub revision: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParentExcerpt {
    pub id: String,
    pub revision: i64,
    pub period_start: String,
    pub period_end: String,
    pub facts: String,
    pub feelings: String,
    pub generator: String,
    pub source_count: usize,
    pub raw_coverage_complete: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FragmentExcerpt {
    pub level: i64,
    pub ordinal: i64,
    pub facts: String,
    pub feelings: String,
    pub input_digest: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReductionStep {
    pub level: i64,
    pub ordinal: i64,
    pub unit_start: usize,
    pub unit_end: usize,
    pub total_units: usize,
    pub final_batch: bool,
    pub input_digest: String,
}

fn live_scope() -> &'static str {
    "assistant_id=?1 AND visibility=?2 AND source_owner_id IS ?3 AND deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM diary_source_revocations v WHERE v.source_owner_id=diary_events.source_owner_id AND diary_events.rowid<=v.through_event_rowid) AND happened_at>=?4 AND happened_at<?5"
}
fn period_raw_fingerprint(
    db: &Connection,
    assistant: &str,
    visibility: &str,
    owner: Option<&str>,
    begin: i64,
    end: i64,
) -> ApiResult<(String, usize, i64)> {
    let mut stmt = db
        .prepare(&format!(
            "SELECT id,revision,observed_at FROM diary_events WHERE {} ORDER BY happened_at,id",
            live_scope()
        ))
        .api()?;
    let rows = stmt
        .query_map(params![assistant, visibility, owner, begin, end], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .api()?;
    let mut hash = Sha256::new();
    let mut count = 0;
    let mut watermark = 0;
    for row in rows {
        let (id, revision, observed) = row.api()?;
        hash.update(id.as_bytes());
        hash.update(revision.to_le_bytes());
        count += 1;
        watermark = watermark.max(observed);
    }
    Ok((hex::encode(hash.finalize()), count, watermark))
}
fn chain_fingerprint(db: &Connection, id: &str) -> ApiResult<(String, usize)> {
    let mut stmt=db.prepare("SELECT e.id,e.revision,s.event_revision FROM diary_entry_sources s JOIN diary_events e ON e.id=s.event_id WHERE s.entry_id=?1 AND e.deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM diary_source_revocations v WHERE v.source_owner_id=e.source_owner_id AND e.rowid<=v.through_event_rowid) ORDER BY e.happened_at,e.id").api()?;
    let rows = stmt
        .query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .api()?;
    let mut hash = Sha256::new();
    let mut count = 0;
    for row in rows {
        let (id, revision, linked) = row.api()?;
        if revision != linked {
            return Err(ApiError::conflict());
        }
        hash.update(id.as_bytes());
        hash.update(revision.to_le_bytes());
        count += 1;
    }
    Ok((hex::encode(hash.finalize()), count))
}
fn parent_is_complete(p: &ParentExcerpt) -> bool {
    p.raw_coverage_complete
}

pub(super) fn consolidate(
    db: &Connection,
    visibility: &str,
    owner: Option<&str>,
    kind: &str,
    start: NaiveDate,
    end: NaiveDate,
    timezone: &str,
    timestamp: i64,
) -> ApiResult<Entry> {
    let assistant = memory::identity(db)?;
    let zone = tz(timezone)?;
    let begin = boundary(start, zone)?;
    let finish = boundary(end, zone)?;
    let (raw_hash, count, watermark) =
        period_raw_fingerprint(db, &assistant, visibility, owner, begin, finish)?;
    // Empty days before the first observed event are explicit unobserved
    // parents; a cursor beginning later must not strand its first week forever.
    let today = local_date(timestamp, zone)?;
    let mut empty_day = start;
    while empty_day < end && empty_day < today {
        let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM diary_entries WHERE assistant_id=?1 AND visibility=?2 AND source_owner_id IS ?3 AND period='day' AND period_start=?4 AND timezone=?5 AND deleted_at IS NULL AND status!='invalidated')",params![assistant,visibility,owner,empty_day.to_string(),timezone],|r|r.get(0)).api()?;
        if !exists {
            let from = boundary(empty_day, zone)?;
            let to = boundary(empty_day + Duration::days(1), zone)?;
            let any: bool = db
                .query_row(
                    &format!(
                        "SELECT EXISTS(SELECT 1 FROM diary_events WHERE {})",
                        live_scope()
                    ),
                    params![assistant, visibility, owner, from, to],
                    |r| r.get(0),
                )
                .api()?;
            if !any {
                consolidate_in(
                    db,
                    visibility,
                    owner,
                    "day",
                    &empty_day.to_string(),
                    timezone,
                    timestamp,
                )?;
            }
        }
        empty_day += Duration::days(1);
    }
    // The per-day cap still applies. No valid month can exceed 31*200 sources;
    // the count is checked before loading any raw body into memory.
    if count > MAX_EVENTS * 31 {
        return Err(ApiError::bad(
            "diary_period_budget_exceeded",
            "本周期超出逐日容量，先处理受阻日记；不会只取首200条",
        ));
    }
    let expected = (end - start).num_days() as usize;
    let mut stmt=db.prepare(&format!("SELECT {ENTRY_COLUMNS} FROM diary_entries WHERE assistant_id=?1 AND visibility=?2 AND source_owner_id IS ?3 AND period='day' AND period_start>=?4 AND period_start<?5 AND timezone=?6 AND deleted_at IS NULL AND status NOT IN ('invalidated','draft') ORDER BY period_start,id LIMIT 31")).api()?;
    let parents = stmt
        .query_map(
            params![
                assistant,
                visibility,
                owner,
                start.to_string(),
                end.to_string(),
                timezone
            ],
            entry_row,
        )
        .api()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .api()?;
    drop(stmt);
    let covered: usize = parents.iter().map(|p| p.source_count).sum();
    let mut parent_text_complete = true;
    for p in &parents {
        let valid:bool=db.query_row("SELECT coalesce((SELECT complete_raw FROM diary_entry_version_coverage WHERE entry_id=?1 AND revision=?2),0)",params![p.id,p.revision],|r|r.get(0)).api()?;
        parent_text_complete &= valid;
    }
    let parents_complete = parents.len() == expected && covered == count && parent_text_complete;
    let complete = parents_complete && parents.iter().all(|p| p.observation_complete);
    let mut hash = Sha256::new();
    hash.update(raw_hash.as_bytes());
    hash.update(
        format!(
            "{expected}:{complete}:{}:{parents_complete}",
            finish <= timestamp
        )
        .as_bytes(),
    );
    // Model enrichment alone does not change this fingerprint: a selected old
    // parent revision stays a valid immutable snapshot until source invalidation.
    for p in &parents {
        hash.update(p.id.as_bytes());
    }
    let fingerprint = hex::encode(hash.finalize());
    let key = format!(
        "{assistant}:{visibility}:{}:{kind}:{start}:{timezone}",
        owner.unwrap_or("")
    );
    let id = format!("diary:{}", hex::encode(Sha256::digest(key.as_bytes())));
    let old: Option<(i64, String, String)> = db
        .query_row(
            "SELECT revision,source_fingerprint,status FROM diary_entries WHERE id=?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .api()?;
    if old
        .as_ref()
        .is_some_and(|(_, f, s)| f == &fingerprint && s != "invalidated")
    {
        return entry(db, &id, None);
    }
    let revision = old.map(|v| v.0 + 1).unwrap_or(1);
    clear(db, &id)?;
    let mut facts=format!("周期来源目录：{}/{}份已封存日记，{covered}/{count}条原始来源已由父日记覆盖。此目录不是已完成的模型周/月记；完整来源分页保留。\n",parents.len(),expected);
    for p in &parents {
        facts.push_str(&format!(
            "{}：{}条；{} r{}；{}。\n",
            p.period_start, p.source_count, p.id, p.revision, p.generator
        ));
    }
    let status = if finish > timestamp {
        "draft"
    } else if complete {
        "final"
    } else {
        "incomplete"
    };
    let epoch: i64 = db
        .query_row(
            "SELECT deletion_epoch FROM diary_meta WHERE id=1",
            [],
            |r| r.get(0),
        )
        .api()?;
    db.execute("INSERT INTO diary_entries(id,assistant_id,revision,period,period_start,period_end,timezone,visibility,source_owner_id,facts,feelings,content,status,generator,source_count,observation_complete,coverage_watermark,deletion_epoch,source_fingerprint,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'',?10,?11,'parent_catalogue_v2',?12,?13,?14,?15,?16,?17) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,facts=excluded.facts,feelings='',content=excluded.content,status=excluded.status,generator=excluded.generator,source_count=excluded.source_count,observation_complete=excluded.observation_complete,coverage_watermark=excluded.coverage_watermark,deletion_epoch=excluded.deletion_epoch,source_fingerprint=excluded.source_fingerprint,updated_at=excluded.updated_at,deleted_at=NULL",params![id,assistant,revision,kind,start.to_string(),end.to_string(),timezone,visibility,owner,facts,status,count as i64,complete,watermark,epoch,fingerprint,timestamp]).api()?;
    db.execute("DELETE FROM diary_entry_sources WHERE entry_id=?1", [&id])
        .api()?;
    db.execute(
        &format!(
            "INSERT INTO diary_entry_sources SELECT ?6,id,revision FROM diary_events WHERE {}",
            live_scope()
        ),
        params![assistant, visibility, owner, begin, finish, id],
    )
    .api()?;
    db.execute("DELETE FROM diary_entry_parents WHERE entry_id=?1", [&id])
        .api()?;
    for p in &parents {
        db.execute(
            "INSERT INTO diary_entry_parents VALUES(?1,?2,?3)",
            params![id, p.id, p.revision],
        )
        .api()?;
    }
    db.execute(
        "INSERT INTO diary_entry_versions VALUES(?1,?2,?3,'','parent_catalogue_v2','system',?4)",
        params![id, revision, facts, timestamp],
    )
    .api()?;
    db.execute(
        "INSERT INTO diary_aggregation_coverage VALUES(?1,?2,?3,?4,?5)",
        params![
            id,
            expected as i64,
            parents.len() as i64,
            covered as i64,
            parents_complete
        ],
    )
    .api()?;
    let allow:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM diary_source_policies WHERE source_owner_id=?1 AND enabled=1 AND allow_model=1)",[owner],|r|r.get(0)).api()?;
    if allow && count > 0 {
        let status = if parents_complete {
            "queued"
        } else {
            "waiting_parents"
        };
        db.execute("INSERT INTO diary_model_jobs(entry_id,entry_revision,status,error,updated_at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(entry_id) DO UPDATE SET entry_revision=excluded.entry_revision,status=excluded.status,attempts=0,error=excluded.error,updated_at=excluded.updated_at",params![id,revision,status,if parents_complete{None}else{Some("parent_coverage_incomplete")},timestamp]).api()?;
    }
    Ok(entry(db, &id, None)?)
}

fn parent_snapshots(db: &Connection, item: &Entry) -> ApiResult<Vec<ParentExcerpt>> {
    let mut stmt=db.prepare("SELECT p.id,s.parent_revision,p.period_start,p.period_end,v.facts,v.feelings,v.generator,p.source_count,coalesce(c.complete_raw,0) FROM diary_entry_parents s JOIN diary_entries p ON p.id=s.parent_id JOIN diary_entry_versions v ON v.entry_id=p.id AND v.revision=s.parent_revision LEFT JOIN diary_entry_version_coverage c ON c.entry_id=p.id AND c.revision=s.parent_revision WHERE s.entry_id=?1 AND p.deleted_at IS NULL AND p.status!='invalidated' AND p.assistant_id=?2 AND p.visibility=?3 AND p.source_owner_id IS ?4 AND v.facts IS NOT NULL ORDER BY p.period_start,p.id LIMIT 31").api()?;
    let rows = stmt
        .query_map(
            params![
                item.id,
                item.assistant_id,
                item.visibility,
                item.source_owner_id
            ],
            |r| {
                Ok(ParentExcerpt {
                    id: r.get(0)?,
                    revision: r.get(1)?,
                    period_start: r.get(2)?,
                    period_end: r.get(3)?,
                    facts: r.get(4)?,
                    feelings: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    generator: r.get(6)?,
                    source_count: r.get::<_, i64>(7)? as usize,
                    raw_coverage_complete: r.get(8)?,
                })
            },
        )
        .api()?;
    rows.collect::<rusqlite::Result<Vec<_>>>().api()
}
pub(super) fn payload(input: &GenerationInput) -> Value {
    json!({"period":input.period,"period_start":input.period_start,"period_end_exclusive":input.period_end,"timezone":input.timezone,"raw_source_count":input.raw_source_count,"events":input.events,"daily_parents":input.parents,"prior_fragments":input.fragments,"reduction":input.reduction})
}
fn digest(parents: &[ParentExcerpt], fragments: &[FragmentExcerpt]) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(&(parents, fragments)).unwrap_or_default(),
    ))
}

pub(super) fn prepare(
    db: &Connection,
    item: &Entry,
    policy: i64,
) -> ApiResult<Option<GenerationInput>> {
    let coverage:Option<(usize,usize,usize,bool)>=db.query_row("SELECT expected_parents,present_parents,covered_sources,parents_complete FROM diary_aggregation_coverage WHERE entry_id=?1",[&item.id],|r|Ok((r.get::<_,i64>(0)? as usize,r.get::<_,i64>(1)? as usize,r.get::<_,i64>(2)? as usize,r.get(3)?))).optional().api()?;
    let parents = parent_snapshots(db, item)?;
    if !coverage.is_some_and(|(expected, present, covered, ready)| {
        ready && expected == present && parents.len() == expected && covered == item.source_count
    }) || !parents.iter().all(parent_is_complete)
    {
        db.execute("UPDATE diary_model_jobs SET status='waiting_parents',error='parent_coverage_incomplete' WHERE entry_id=?1",[&item.id]).api()?;
        return Ok(None);
    }
    let (raw_fingerprint, count) = chain_fingerprint(db, &item.id)?;
    if count != item.source_count {
        return Err(ApiError::conflict());
    }
    let level: i64 = db
        .query_row(
            "SELECT level FROM diary_model_reductions WHERE entry_id=?1 AND entry_revision=?2",
            params![item.id, item.revision],
            |r| r.get(0),
        )
        .optional()
        .api()?
        .unwrap_or(0);
    let mut previous = Vec::new();
    if level > 0 {
        let mut stmt=db.prepare("SELECT level,ordinal,facts,feelings,input_digest FROM diary_model_fragments WHERE entry_id=?1 AND entry_revision=?2 AND level=?3 ORDER BY ordinal").api()?;
        previous = stmt
            .query_map(params![item.id, item.revision, level - 1], |r| {
                Ok(FragmentExcerpt {
                    level: r.get(0)?,
                    ordinal: r.get(1)?,
                    facts: r.get(2)?,
                    feelings: r.get(3)?,
                    input_digest: r.get(4)?,
                })
            })
            .api()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .api()?;
    }
    let total = if level == 0 {
        parents.len()
    } else {
        previous.len()
    };
    let(start,ordinal):(usize,i64)=db.query_row("SELECT coalesce(max(unit_end),0),count(*) FROM diary_model_fragments WHERE entry_id=?1 AND entry_revision=?2 AND level=?3",params![item.id,item.revision,level],|r|Ok((r.get::<_,i64>(0)? as usize,r.get(1)?))).api()?;
    if start >= total || total == 0 {
        return Err(ApiError::conflict());
    }
    let epoch: i64 = db
        .query_row(
            "SELECT deletion_epoch FROM diary_meta WHERE id=1",
            [],
            |r| r.get(0),
        )
        .api()?;
    let source_fingerprint = db
        .query_row(
            "SELECT source_fingerprint FROM diary_entries WHERE id=?1",
            [&item.id],
            |r| r.get(0),
        )
        .api()?;
    let refs = parents
        .iter()
        .map(|p| ParentReference {
            id: p.id.clone(),
            revision: p.revision,
        })
        .collect();
    let mut input = GenerationInput {
        entry_id: item.id.clone(),
        entry_revision: item.revision,
        deletion_epoch: epoch,
        policy_revision: policy,
        period: item.period.clone(),
        period_start: item.period_start.clone(),
        period_end: item.period_end.clone(),
        timezone: item.timezone.clone(),
        source_owner_id: item.source_owner_id.clone().unwrap_or_default(),
        source_fingerprint,
        events: Vec::new(),
        parents: Vec::new(),
        fragments: Vec::new(),
        parent_references: refs,
        raw_fingerprint: Some(raw_fingerprint),
        raw_source_count: count,
        reduction: Some(ReductionStep {
            level,
            ordinal,
            unit_start: start,
            unit_end: start,
            total_units: total,
            final_batch: false,
            input_digest: String::new(),
        }),
    };
    let mut end = start;
    while end < total {
        if level == 0 {
            input.parents.push(parents[end].clone());
        } else {
            input.fragments.push(previous[end].clone());
        }
        input.reduction.as_mut().unwrap().unit_end = end + 1;
        if serde_json::to_vec(&payload(&input))
            .map_err(|_| ApiError::internal())?
            .len()
            > INPUT_BYTES - 4000
        {
            if level == 0 {
                input.parents.pop();
            } else {
                input.fragments.pop();
            }
            input.reduction.as_mut().unwrap().unit_end = end;
            break;
        }
        end += 1;
    }
    if end == start {
        db.execute("UPDATE diary_model_jobs SET status='blocked',error='single_parent_input_budget' WHERE entry_id=?1",[&item.id]).api()?;
        return Ok(None);
    }
    let digest = digest(&input.parents, &input.fragments);
    let step = input.reduction.as_mut().unwrap();
    step.final_batch = start == 0 && end == total;
    step.input_digest = digest;
    db.execute("UPDATE diary_model_jobs SET status='running',attempts=attempts+1,updated_at=?1 WHERE entry_id=?2",params![now(),item.id]).api()?;
    Ok(Some(input))
}

pub(super) fn commit(
    db: &Connection,
    input: &GenerationInput,
    output: &GenerationOutput,
    model: &str,
) -> ApiResult<Entry> {
    let item = entry(db, &input.entry_id, None)?;
    let step = input.reduction.as_ref().ok_or_else(ApiError::conflict)?;
    let policy:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM diary_source_policies WHERE source_owner_id=?1 AND revision=?2 AND enabled=1 AND allow_model=1)",params![input.source_owner_id,input.policy_revision],|r|r.get(0)).api()?;
    let epoch: i64 = db
        .query_row(
            "SELECT deletion_epoch FROM diary_meta WHERE id=1",
            [],
            |r| r.get(0),
        )
        .api()?;
    let stored: String = db
        .query_row(
            "SELECT source_fingerprint FROM diary_entries WHERE id=?1",
            [&item.id],
            |r| r.get(0),
        )
        .api()?;
    let job:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM diary_model_jobs WHERE entry_id=?1 AND entry_revision=?2 AND status='running')",params![item.id,item.revision],|r|r.get(0)).api()?;
    let (raw, count) = chain_fingerprint(db, &item.id)?;
    let parents = parent_snapshots(db, &item)?;
    let refs: Vec<_> = parents
        .iter()
        .map(|p| (p.id.as_str(), p.revision))
        .collect();
    let supplied: Vec<_> = input
        .parent_references
        .iter()
        .map(|p| (p.id.as_str(), p.revision))
        .collect();
    if item.revision != input.entry_revision
        || !policy
        || epoch != input.deletion_epoch
        || !job
        || stored != input.source_fingerprint
        || input.raw_fingerprint.as_deref() != Some(&raw)
        || count != input.raw_source_count
        || refs != supplied
        || step.input_digest != digest(&input.parents, &input.fragments)
    {
        return Err(ApiError::conflict());
    }
    let checkpoint:(usize,i64)=db.query_row("SELECT coalesce(max(unit_end),0),count(*) FROM diary_model_fragments WHERE entry_id=?1 AND entry_revision=?2 AND level=?3",params![item.id,item.revision,step.level],|r|Ok((r.get::<_,i64>(0)? as usize,r.get(1)?))).api()?;
    if checkpoint != (step.unit_start, step.ordinal) {
        return Err(ApiError::conflict());
    }
    if step.final_batch {
        let generator = format!("model:{model}");
        let content = format!(
            "{}\n\n主观感受（非他人事实）：\n{}",
            output.facts, output.feelings
        );
        db.execute("UPDATE diary_entries SET revision=revision+1,facts=?1,feelings=?2,content=?3,generator=?4,updated_at=?5 WHERE id=?6 AND revision=?7",params![output.facts,output.feelings,content,generator,now(),item.id,item.revision]).api()?;
        db.execute(
            "INSERT INTO diary_entry_versions VALUES(?1,?2,?3,?4,?5,'authorized_model',?6)",
            params![
                item.id,
                item.revision + 1,
                output.facts,
                output.feelings,
                generator,
                now()
            ],
        )
        .api()?;
        db.execute("UPDATE diary_model_jobs SET status='completed',error=NULL,updated_at=?1 WHERE entry_id=?2",params![now(),item.id]).api()?;
        audit(db, "system", "diary.model_consolidate", &item.id)?;
    } else {
        db.execute(
            "INSERT INTO diary_model_fragments VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                item.id,
                item.revision,
                step.level,
                step.ordinal,
                step.unit_start as i64,
                step.unit_end as i64,
                step.total_units as i64,
                step.input_digest,
                output.facts,
                output.feelings,
                model,
                now()
            ],
        )
        .api()?;
        let next_level = if step.unit_end == step.total_units {
            step.level + 1
        } else {
            step.level
        };
        db.execute("INSERT INTO diary_model_reductions VALUES(?1,?2,?3) ON CONFLICT(entry_id,entry_revision) DO UPDATE SET level=excluded.level",params![item.id,item.revision,next_level]).api()?;
        db.execute("UPDATE diary_model_jobs SET status='queued',attempts=0,error=NULL,updated_at=?1 WHERE entry_id=?2",params![now(),item.id]).api()?;
        audit(db, "system", "diary.model_fragment", &item.id)?;
    }
    entry(db, &item.id, None)
}

pub(super) fn retry_periods(db: &Connection, timestamp: i64) -> ApiResult<()> {
    // Recover both old reader-1 budget blocks and waiting parent coverage after
    // days finish, without rewinding/reprocessing every successful daily job.
    let rows = {
        let mut stmt=db.prepare("SELECT e.id,e.visibility,e.source_owner_id,e.period,e.period_start,e.timezone FROM diary_entries e LEFT JOIN diary_model_jobs j ON j.entry_id=e.id WHERE e.period!='day' AND e.deleted_at IS NULL AND (e.status='invalidated' OR j.status='waiting_parents') AND NOT EXISTS(SELECT 1 FROM diary_source_revocations r WHERE r.source_owner_id=e.source_owner_id AND NOT EXISTS(SELECT 1 FROM diary_source_policies p WHERE p.source_owner_id=r.source_owner_id AND p.enabled=1)) ORDER BY e.period_start LIMIT 4").api()?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })
            .api()?;
        rows.collect::<rusqlite::Result<Vec<_>>>().api()?
    };
    for (_, visibility, owner, period, day, timezone) in rows {
        let _ = consolidate_in(
            db,
            &visibility,
            owner.as_deref(),
            &period,
            &day,
            &timezone,
            timestamp,
        )?;
    }
    let blocks = {
        let mut stmt=db.prepare("SELECT b.scope_key,b.period,b.period_start,c.visibility,c.source_owner_id,c.timezone FROM diary_consolidation_blocks b JOIN diary_scope_checkpoints c ON c.scope_key=b.scope_key WHERE b.period!='day' ORDER BY b.updated_at LIMIT 4").api()?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })
            .api()?;
        rows.collect::<rusqlite::Result<Vec<_>>>().api()?
    };
    for (scope, kind, day, visibility, owner, timezone) in blocks {
        if consolidate_in(
            db,
            &visibility,
            owner.as_deref(),
            &kind,
            &day,
            &timezone,
            timestamp,
        )
        .is_ok()
        {
            clear_consolidation_block(db, &scope, &kind, date(&day)?)?;
        }
    }
    Ok(())
}
