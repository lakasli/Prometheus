//! Durable, bounded coalescing of consecutive ordinary QQ C2C text receipts.
//! The aggregate is mutable only before a worker claims it. Receipt fragments
//! retain their own immutable content and identity for provenance and deduping.
use crate::worker::ReplyTarget;
use anyhow::ensure;
use rusqlite::{params, Connection, OptionalExtension};
use std::time::{SystemTime, UNIX_EPOCH};

pub const QUIET_MS: i64 = 1_500;
pub const MAX_WAIT_MS: i64 = 5_000;
pub const MAX_FRAGMENTS: i64 = 8;
pub const MAX_BYTES: usize = 4_096;
const VERSION_KEY: &str = "qq.text_batches.schema.v1";

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    let tx = db.unchecked_transaction()?;
    let version: Option<String> = tx
        .query_row(
            "SELECT value FROM settings WHERE key=?1",
            [VERSION_KEY],
            |r| r.get(0),
        )
        .optional()?;
    ensure!(
        version.as_deref().is_none_or(|v| v == "1"),
        "unsupported QQ text batch schema"
    );
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS qq_text_batches(
           run_id TEXT PRIMARY KEY REFERENCES runs(id),
           first_ms INTEGER NOT NULL CHECK(first_ms>=0),
           last_ms INTEGER NOT NULL CHECK(last_ms>=first_ms),
           due_ms INTEGER NOT NULL CHECK(due_ms>=0),
           closed INTEGER NOT NULL CHECK(closed IN (0,1)),
           fragment_count INTEGER NOT NULL CHECK(fragment_count BETWEEN 1 AND 8),
           byte_count INTEGER NOT NULL CHECK(byte_count BETWEEN 1 AND 4096),
           history_floor INTEGER NOT NULL CHECK(history_floor>=0));
         CREATE TABLE IF NOT EXISTS qq_text_fragments(
           run_id TEXT NOT NULL REFERENCES runs(id),
           ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 1 AND 8),
           owner_id TEXT NOT NULL,account_id TEXT NOT NULL,target_id TEXT NOT NULL,
           event_type TEXT NOT NULL CHECK(event_type='C2C_MESSAGE_CREATE'),
           message_id TEXT NOT NULL,content TEXT NOT NULL,
           received_at INTEGER NOT NULL CHECK(received_at>=0),
           received_ms INTEGER NOT NULL CHECK(received_ms>=0),
           PRIMARY KEY(run_id,ordinal),UNIQUE(owner_id,account_id,message_id),
           CHECK(owner_id='qq:'||account_id||':c2c:'||target_id),
           CHECK(length(content)>0 AND length(CAST(content AS BLOB))<=1024));
         CREATE INDEX IF NOT EXISTS qq_text_fragment_identity ON qq_text_fragments(owner_id,message_id);
         CREATE TRIGGER IF NOT EXISTS qq_text_fragment_immutable BEFORE UPDATE ON qq_text_fragments
           BEGIN SELECT RAISE(ABORT,'QQ receipt fragments are immutable'); END;",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO settings(key,value) VALUES(?1,'1')",
        [VERSION_KEY],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn is_eligible(kind: &str, content: &str, has_media: bool, run_kind: &str) -> bool {
    kind == "c2c"
        && run_kind == "model"
        && !has_media
        && !content.trim().is_empty()
        && !content.trim_start().starts_with('/')
        && content.len() <= 1_024
        && content.chars().count() <= 256
}

fn transaction(db: &Connection) -> anyhow::Result<()> {
    ensure!(
        !db.is_autocommit(),
        "QQ text batching requires an outer transaction"
    );
    Ok(())
}

fn canonical_owner(owner: &str, reply: &ReplyTarget) -> bool {
    !reply.account_id.is_empty()
        && !reply.target.is_empty()
        && !reply.message_id.is_empty()
        && !reply.credential_ref.is_empty()
        && reply.kind == "c2c"
        && owner == format!("qq:{}:c2c:{}", reply.account_id, reply.target)
}

fn same_destination(a: &ReplyTarget, b: &ReplyTarget) -> bool {
    a.kind == b.kind
        && a.target == b.target
        && a.account_id == b.account_id
        && a.credential_ref == b.credential_ref
}

fn deadline_bound(reply: &ReplyTarget) -> i64 {
    reply
        .deadline
        .saturating_mul(1_000)
        .saturating_sub(1_000)
        .max(0)
}

fn due(first_ms: i64, last_ms: i64, reply: &ReplyTarget) -> i64 {
    last_ms
        .saturating_add(QUIET_MS)
        .min(first_ms.saturating_add(MAX_WAIT_MS))
        .min(deadline_bound(reply))
}

/// This lookup includes completed batches so late redelivery cannot create a
/// fresh response for a follower whose platform ID is not runs.client_message_id.
pub fn existing_run_in(
    db: &Connection,
    owner: &str,
    message_id: &str,
) -> anyhow::Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT f.run_id FROM qq_text_fragments f JOIN runs r ON r.id=f.run_id
         JOIN conversations c ON c.id=r.conversation_id AND c.owner_id=r.owner_id
         WHERE f.owner_id=?1 AND f.message_id=?2 AND r.owner_id=f.owner_id LIMIT 1",
            params![owner, message_id],
            |r| r.get(0),
        )
        .optional()?)
}

fn insert_fragment(
    db: &Connection,
    run: &str,
    ordinal: i64,
    owner: &str,
    content: &str,
    reply: &ReplyTarget,
    received_at: i64,
    received_ms: i64,
) -> anyhow::Result<()> {
    db.execute(
        "INSERT INTO qq_text_fragments(run_id,ordinal,owner_id,account_id,target_id,event_type,message_id,content,received_at,received_ms)
         VALUES(?1,?2,?3,?4,?5,'C2C_MESSAGE_CREATE',?6,?7,?8,?9)",
        params![run,ordinal,owner,reply.account_id,reply.target,reply.message_id,content,received_at,received_ms],
    )?;
    Ok(())
}

/// Register only after the Run and its sole user message have been inserted in
/// the caller's Inbox transaction. Old receipts keep their arrival-based due;
/// processing backlog or a nearly expired passive reply never creates a wait.
pub fn register_in(
    db: &Connection,
    run: &str,
    conversation: &str,
    owner: &str,
    content: &str,
    reply: &ReplyTarget,
    received_at: i64,
    received_ms: i64,
    observed_ms: i64,
    config_json: &str,
    character_digest: Option<&str>,
) -> anyhow::Result<()> {
    transaction(db)?;
    ensure!(
        is_eligible(&reply.kind, content, false, "model") && canonical_owner(owner, reply),
        "invalid QQ text batch receipt"
    );
    ensure!(
        received_at >= 0 && received_ms >= 0 && observed_ms >= 0,
        "invalid QQ receipt time"
    );
    let digest = character_digest
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("QQ text batches require a frozen personality"))?;
    let stored: Option<String> = db.query_row(
        "SELECT r.reply_json FROM runs r JOIN conversations c ON c.id=r.conversation_id AND c.owner_id=r.owner_id
         JOIN messages m ON m.run_id=r.id AND m.conversation_id=r.conversation_id AND m.role='user'
         WHERE r.id=?1 AND r.conversation_id=?2 AND r.owner_id=?3 AND r.status='queued' AND r.run_kind='model'
           AND r.input=?4 AND m.content=?4 AND r.config_json=?5 AND r.character_digest=?6 AND r.character_snapshot_json IS NOT NULL
           AND r.rowid=(SELECT max(rowid) FROM runs WHERE conversation_id=?2)
           AND NOT EXISTS(SELECT 1 FROM native_worker_leases l WHERE l.run_id=r.id)
           AND NOT EXISTS(SELECT 1 FROM run_prompt_snapshots p WHERE p.run_id=r.id)",
        params![run,conversation,owner,content,config_json,digest], |r|r.get(0),
    ).optional()?.flatten();
    let stored: ReplyTarget = serde_json::from_str(
        &stored.ok_or_else(|| anyhow::anyhow!("QQ batch Run is no longer eligible"))?,
    )?;
    ensure!(
        same_destination(&stored, reply)
            && stored.message_id == reply.message_id
            && stored.deadline == reply.deadline,
        "QQ batch passive reply snapshot mismatch"
    );
    let floor = crate::memory_context::history_floor(db, conversation)?;
    let mut due_ms = due(received_ms, received_ms, reply);
    let closed = observed_ms < received_ms || observed_ms >= due_ms;
    if observed_ms < received_ms {
        due_ms = due_ms.min(observed_ms);
    }
    db.execute("INSERT INTO qq_text_batches(run_id,first_ms,last_ms,due_ms,closed,fragment_count,byte_count,history_floor) VALUES(?1,?2,?2,?3,?4,1,?5,?6)",params![run,received_ms,due_ms,closed,content.len() as i64,floor])?;
    insert_fragment(db, run, 1, owner, content, reply, received_at, received_ms)?;
    ensure!(
        db.execute(
            "UPDATE messages SET created_at=?1 WHERE run_id=?2 AND role='user'",
            params![received_at, run]
        )? == 1,
        "QQ batch user message is unavailable"
    );
    Ok(())
}

struct Tail {
    run: String,
    input: String,
    config: String,
    digest: Option<String>,
    reply: Option<String>,
    first_ms: i64,
    last_ms: i64,
    due_ms: i64,
    closed: bool,
    count: i64,
    bytes: i64,
    floor: i64,
}

/// Append only to the unclaimed last Run, before both arrival and processing
/// deadlines. None is a boundary: the caller closes the old tail and creates a
/// new Run. The caller also serializes durable receipts before reaching here.
pub fn try_append_in(
    db: &Connection,
    conversation: &str,
    owner: &str,
    content: &str,
    reply: &ReplyTarget,
    received_at: i64,
    received_ms: i64,
    observed_ms: i64,
    config_json: &str,
    character_digest: Option<&str>,
) -> anyhow::Result<Option<String>> {
    transaction(db)?;
    if !is_eligible(&reply.kind, content, false, "model")
        || !canonical_owner(owner, reply)
        || received_at < 0
        || received_ms < 0
        || observed_ms < 0
        || character_digest.is_none_or(str::is_empty)
    {
        return Ok(None);
    }
    if let Some(run) = existing_run_in(db, owner, &reply.message_id)? {
        return Ok(Some(run));
    }
    let tail=db.query_row(
        "SELECT r.id,r.input,r.config_json,r.character_digest,r.reply_json,b.first_ms,b.last_ms,b.due_ms,b.closed,b.fragment_count,b.byte_count,b.history_floor
         FROM runs r JOIN conversations c ON c.id=r.conversation_id AND c.owner_id=r.owner_id
         JOIN qq_text_batches b ON b.run_id=r.id
         WHERE r.conversation_id=?1 AND r.owner_id=?2 AND r.status='queued' AND r.run_kind='model'
           AND r.character_snapshot_json IS NOT NULL
           AND r.rowid=(SELECT max(rowid) FROM runs WHERE conversation_id=?1)
           AND NOT EXISTS(SELECT 1 FROM native_worker_leases l WHERE l.run_id=r.id)
           AND NOT EXISTS(SELECT 1 FROM run_prompt_snapshots p WHERE p.run_id=r.id)",
        params![conversation,owner],|r|Ok(Tail {run:r.get(0)?,input:r.get(1)?,config:r.get(2)?,digest:r.get(3)?,reply:r.get(4)?,first_ms:r.get(5)?,last_ms:r.get(6)?,due_ms:r.get(7)?,closed:r.get(8)?,count:r.get(9)?,bytes:r.get(10)?,floor:r.get(11)?}),
    ).optional()?;
    let Some(tail) = tail else {
        return Ok(None);
    };
    let Some(old_reply) = tail
        .reply
        .as_deref()
        .map(serde_json::from_str::<ReplyTarget>)
        .transpose()?
    else {
        return Ok(None);
    };
    let new_bytes = tail
        .input
        .len()
        .checked_add(1)
        .and_then(|n| n.checked_add(content.len()));
    let same = tail.config == config_json
        && tail.digest.as_deref() == character_digest
        && same_destination(&old_reply, reply)
        && tail.floor == crate::memory_context::history_floor(db, conversation)?;
    if !same
        || tail.closed
        || tail.count >= MAX_FRAGMENTS
        || new_bytes.is_none_or(|n| n > MAX_BYTES)
        || tail.bytes != tail.input.len() as i64
        || received_ms < tail.last_ms
        || observed_ms < tail.last_ms
        || observed_ms < received_ms
        || received_ms >= tail.due_ms
        || observed_ms >= tail.due_ms
        || observed_ms >= tail.first_ms.saturating_add(MAX_WAIT_MS)
        || observed_ms >= deadline_bound(reply)
    {
        return Ok(None);
    }
    let due_ms = due(tail.first_ms, received_ms, reply);
    if due_ms <= observed_ms {
        return Ok(None);
    }
    let count = tail.count + 1;
    let bytes = new_bytes.expect("validated aggregate length");
    let closed = count >= MAX_FRAGMENTS || bytes >= MAX_BYTES;
    let due_ms = if closed {
        due_ms.min(observed_ms)
    } else {
        due_ms
    };
    let aggregate = format!("{}\n{content}", tail.input);
    insert_fragment(
        db,
        &tail.run,
        count,
        owner,
        content,
        reply,
        received_at,
        received_ms,
    )?;
    ensure!(
        db.execute(
            "UPDATE runs SET input=?1,reply_json=?2,updated_at=?3 WHERE id=?4 AND status='queued'",
            params![
                aggregate,
                serde_json::to_string(reply)?,
                observed_ms / 1000,
                tail.run
            ]
        )? == 1,
        "QQ batch Run was claimed"
    );
    ensure!(db.execute("UPDATE messages SET content=?1,created_at=?2 WHERE run_id=?3 AND role='user' AND content=?4",params![aggregate,received_at,tail.run,tail.input])?==1,"QQ batch aggregate message changed");
    db.execute("UPDATE qq_text_batches SET last_ms=?1,due_ms=?2,closed=?3,fragment_count=?4,byte_count=?5 WHERE run_id=?6",params![received_ms,due_ms,closed,count,bytes as i64,tail.run])?;
    Ok(Some(tail.run))
}

/// A command, media receipt, changed configuration, or rejected append ends the
/// old quiet window. Closing never changes an aggregate or an already sent reply.
pub fn close_tail_in(db: &Connection, conversation: &str, observed_ms: i64) -> anyhow::Result<()> {
    transaction(db)?;
    db.execute("UPDATE qq_text_batches SET closed=1,due_ms=min(due_ms,?1) WHERE closed=0 AND run_id=(SELECT id FROM runs WHERE conversation_id=?2 ORDER BY rowid DESC LIMIT 1)",params![observed_ms.max(0),conversation])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const OWNER: &str = "qq:app:c2c:peer";
    const CONFIG: &str = "{\"version\":1}";
    const FIRST: i64 = 1_000_000;
    fn reply(id: &str) -> ReplyTarget {
        ReplyTarget {
            kind: "c2c".into(),
            target: "peer".into(),
            message_id: id.into(),
            account_id: "app".into(),
            credential_ref: "key".into(),
            deadline: 4_600,
        }
    }
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(include_str!("../schema.sql")).unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON; ALTER TABLE runs ADD COLUMN run_kind TEXT NOT NULL DEFAULT 'model'; ALTER TABLE runs ADD COLUMN character_snapshot_json TEXT; ALTER TABLE runs ADD COLUMN character_digest TEXT;
          CREATE TABLE native_worker_leases(run_id TEXT PRIMARY KEY,claimed_at INTEGER); CREATE TABLE run_prompt_snapshots(run_id TEXT PRIMARY KEY);
          INSERT INTO conversations(id,owner_id,title,created_at) VALUES('chat','qq:app:c2c:peer','test',0),('other','qq:app:c2c:peer','test',0),('foreign','qq:app:c2c:other','test',0);").unwrap();
        crate::memory_context::initialize(&db).unwrap();
        initialize(&db).unwrap();
        db.execute_batch("BEGIN").unwrap();
        db
    }
    fn run(
        db: &Connection,
        id: &str,
        conversation: &str,
        owner: &str,
        text: &str,
        target: &ReplyTarget,
    ) {
        db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,reply_json,status,created_at,updated_at,run_kind,character_snapshot_json,character_digest) VALUES(?1,?2,?3,?1,?4,?5,?6,'queued',1000,1000,'model','{}','digest')",params![id,conversation,owner,text,CONFIG,serde_json::to_string(target).unwrap()]).unwrap();
        db.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?1,?2,?3,'user',?4,1000)",params![format!("{id}-user"),conversation,id,text]).unwrap();
    }
    fn seed(db: &Connection, text: &str) {
        run(db, "run", "chat", OWNER, text, &reply("first"));
        register_in(
            db,
            "run",
            "chat",
            OWNER,
            text,
            &reply("first"),
            1000,
            FIRST,
            FIRST,
            CONFIG,
            Some("digest"),
        )
        .unwrap();
    }
    fn append(db: &Connection, id: &str, text: &str, at: i64) -> Option<String> {
        try_append_in(
            db,
            "chat",
            OWNER,
            text,
            &reply(id),
            at / 1000,
            at,
            at,
            CONFIG,
            Some("digest"),
        )
        .unwrap()
    }
    fn counts(db: &Connection) -> (i64, i64, i64, bool) {
        db.query_row("SELECT fragment_count,byte_count,due_ms,closed FROM qq_text_batches WHERE run_id='run'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap()
    }

    #[test]
    fn eligibility_excludes_commands_media_non_c2c_and_large_texts() {
        assert!(is_eligible("c2c", "你好", false, "model"));
        for (kind, text, media, run_kind) in [
            ("group", "你好", false, "model"),
            ("c2c", "/memory list", false, "model"),
            ("c2c", " \n /角色", false, "model"),
            ("c2c", "你好", true, "model"),
            ("c2c", "你好", false, "skill_text"),
            ("c2c", " ", false, "model"),
        ] {
            assert!(!is_eligible(kind, text, media, run_kind));
        }
        assert!(!is_eligible("c2c", &"中".repeat(257), false, "model"));
        assert!(!is_eligible("c2c", &"a".repeat(1025), false, "model"));
    }
    #[test]
    fn aggregate_latest_reply_and_immutable_sources_commit_together() {
        let db = fixture();
        seed(&db, "今天");
        let mut latest = reply("second");
        latest.deadline = 4_599;
        assert_eq!(
            try_append_in(
                &db,
                "chat",
                OWNER,
                "有点累",
                &latest,
                1001,
                FIRST + 1000,
                FIRST + 1000,
                CONFIG,
                Some("digest")
            )
            .unwrap(),
            Some("run".into())
        );
        let (input, stored): (String, String) = db
            .query_row(
                "SELECT input,reply_json FROM runs WHERE id='run'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(input, "今天\n有点累");
        let stored: ReplyTarget = serde_json::from_str(&stored).unwrap();
        assert_eq!(stored.message_id, "second");
        assert_eq!(stored.deadline, 4599);
        let (text, at): (String, i64) = db
            .query_row(
                "SELECT content,created_at FROM messages WHERE run_id='run'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(text, input);
        assert_eq!(at, 1001);
        assert_eq!(counts(&db), (2, input.len() as i64, FIRST + 2500, false));
        let source: String = db
            .query_row(
                "SELECT content FROM qq_text_fragments WHERE ordinal=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(source, "今天");
        assert!(db
            .execute("UPDATE qq_text_fragments SET content='changed'", [])
            .is_err());
    }
    #[test]
    fn follower_redelivery_survives_completion_without_mutating_a_batch() {
        let db = fixture();
        seed(&db, "one");
        assert!(append(&db, "second", "two", FIRST + 100).is_some());
        let before = counts(&db);
        assert!(append(&db, "second", "different duplicate", FIRST + 200).is_some());
        assert_eq!(counts(&db), before);
        db.execute("UPDATE runs SET status='completed'", [])
            .unwrap();
        assert_eq!(
            existing_run_in(&db, OWNER, "second").unwrap(),
            Some("run".into())
        );
        assert_eq!(
            existing_run_in(&db, "qq:app:c2c:other", "second").unwrap(),
            None
        );
    }
    #[test]
    fn only_the_same_owner_conversation_and_absolute_last_run_can_merge() {
        let db = fixture();
        seed(&db, "one");
        assert_eq!(
            try_append_in(
                &db,
                "other",
                OWNER,
                "two",
                &reply("second"),
                1000,
                FIRST + 100,
                FIRST + 100,
                CONFIG,
                Some("digest")
            )
            .unwrap(),
            None
        );
        let mut wrong = reply("wrong");
        wrong.target = "other".into();
        assert_eq!(
            try_append_in(
                &db,
                "chat",
                "qq:app:c2c:other",
                "two",
                &wrong,
                1000,
                FIRST + 100,
                FIRST + 100,
                CONFIG,
                Some("digest")
            )
            .unwrap(),
            None
        );
        run(&db, "later", "chat", OWNER, "/memory list", &reply("later"));
        db.execute(
            "UPDATE runs SET run_kind='memory_command',status='completed' WHERE id='later'",
            [],
        )
        .unwrap();
        assert_eq!(append(&db, "second", "two", FIRST + 100), None);
        assert_eq!(counts(&db).0, 1);
    }
    #[test]
    fn running_lease_and_prompt_snapshot_each_fence_an_aggregate() {
        for guard in [
            "UPDATE runs SET status='running'",
            "INSERT INTO native_worker_leases VALUES('run',0)",
            "INSERT INTO run_prompt_snapshots VALUES('run')",
        ] {
            let db = fixture();
            seed(&db, "one");
            db.execute_batch(guard).unwrap();
            assert_eq!(append(&db, "second", "two", FIRST + 100), None);
            assert_eq!(counts(&db).0, 1);
        }
    }
    #[test]
    fn quiet_and_processing_deadlines_are_strict() {
        let db = fixture();
        seed(&db, "one");
        assert_eq!(append(&db, "late", "two", FIRST + QUIET_MS), None);
        assert_eq!(
            try_append_in(
                &db,
                "chat",
                OWNER,
                "two",
                &reply("backlog"),
                1000,
                FIRST + 100,
                FIRST + QUIET_MS,
                CONFIG,
                Some("digest")
            )
            .unwrap(),
            None
        );
        assert!(append(&db, "before", "two", FIRST + QUIET_MS - 1).is_some());
    }
    #[test]
    fn continuous_text_never_extends_the_five_second_window() {
        let db = fixture();
        seed(&db, "one");
        for n in 1..=4 {
            assert!(append(&db, &format!("m{n}"), "next", FIRST + n * 1000).is_some());
        }
        assert_eq!(counts(&db).2, FIRST + MAX_WAIT_MS);
        assert_eq!(append(&db, "late", "next", FIRST + MAX_WAIT_MS), None);
    }
    #[test]
    fn count_and_byte_budgets_end_the_batch() {
        let db = fixture();
        seed(&db, "one");
        for n in 1..MAX_FRAGMENTS {
            assert!(append(&db, &format!("m{n}"), "two", FIRST + n * 10).is_some());
        }
        assert_eq!(counts(&db).0, 8);
        assert!(counts(&db).3);
        assert_eq!(append(&db, "ninth", "x", FIRST + 90), None);
        let large = fixture();
        seed(&large, &"中".repeat(256));
        for n in 1..=4 {
            assert!(append(&large, &format!("m{n}"), &"中".repeat(256), FIRST + n * 10).is_some());
        }
        assert_eq!(
            append(&large, "overflow", &"中".repeat(256), FIRST + 100),
            None
        );
        assert!(counts(&large).1 <= MAX_BYTES as i64);
    }
    #[test]
    fn configuration_personality_and_forgetting_are_boundaries() {
        let db = fixture();
        seed(&db, "one");
        for (config, digest) in [
            ("different", Some("digest")),
            (CONFIG, Some("new-digest")),
            (CONFIG, None),
        ] {
            assert_eq!(
                try_append_in(
                    &db,
                    "chat",
                    OWNER,
                    "two",
                    &reply("second"),
                    1000,
                    FIRST + 100,
                    FIRST + 100,
                    config,
                    digest
                )
                .unwrap(),
                None
            );
        }
        db.execute(
            "INSERT INTO memory_context_watermarks VALUES('chat','assistant',1,0)",
            [],
        )
        .unwrap();
        assert_eq!(append(&db, "second", "two", FIRST + 100), None);
        assert_eq!(counts(&db).0, 1);
    }
    #[test]
    fn destination_and_credential_changes_do_not_reuse_the_old_reply() {
        let db = fixture();
        seed(&db, "one");
        for field in ["kind", "target", "account", "credential"] {
            let mut changed = reply("second");
            match field {
                "kind" => changed.kind = "group".into(),
                "target" => changed.target = "other".into(),
                "account" => changed.account_id = "other".into(),
                _ => changed.credential_ref = "rotated".into(),
            };
            assert_eq!(
                try_append_in(
                    &db,
                    "chat",
                    OWNER,
                    "two",
                    &changed,
                    1000,
                    FIRST + 100,
                    FIRST + 100,
                    CONFIG,
                    Some("digest")
                )
                .unwrap(),
                None
            );
        }
        let original: String = db
            .query_row("SELECT reply_json FROM runs WHERE id='run'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            serde_json::from_str::<ReplyTarget>(&original)
                .unwrap()
                .message_id,
            "first"
        );
    }
    #[test]
    fn passive_expiry_backlog_and_clock_rollback_never_add_wait() {
        let db = fixture();
        let mut short = reply("first");
        short.deadline = 1002;
        run(&db, "run", "chat", OWNER, "one", &short);
        register_in(
            &db,
            "run",
            "chat",
            OWNER,
            "one",
            &short,
            1000,
            FIRST,
            FIRST,
            CONFIG,
            Some("digest"),
        )
        .unwrap();
        assert_eq!(counts(&db).2, FIRST + 1000);
        assert_eq!(append(&db, "second", "two", FIRST + 1000), None);
        let delayed = fixture();
        run(&delayed, "run", "chat", OWNER, "one", &reply("first"));
        register_in(
            &delayed,
            "run",
            "chat",
            OWNER,
            "one",
            &reply("first"),
            1000,
            FIRST,
            FIRST + 2000,
            CONFIG,
            Some("digest"),
        )
        .unwrap();
        assert!(counts(&delayed).3);
        let rollback = fixture();
        seed(&rollback, "one");
        assert_eq!(append(&rollback, "reverse", "two", FIRST - 1), None);
        close_tail_in(&rollback, "chat", FIRST - 1).unwrap();
        assert_eq!(counts(&rollback).2, FIRST - 1);
        assert!(counts(&rollback).3);
    }
    #[test]
    fn explicit_boundaries_release_the_tail_and_rollback_restores_everything() {
        let db = fixture();
        seed(&db, "one");
        db.execute_batch("SAVEPOINT receipt").unwrap();
        assert!(append(&db, "second", "two", FIRST + 100).is_some());
        db.execute("INSERT INTO inbox(event_key,account_id,event_type,received_at,run_id) VALUES('second-event','app','C2C_MESSAGE_CREATE',1000,'run')",[]).unwrap();
        db.execute_batch("ROLLBACK TO receipt; RELEASE receipt")
            .unwrap();
        assert_eq!(counts(&db).0, 1);
        assert_eq!(existing_run_in(&db, OWNER, "second").unwrap(), None);
        let events: i64 = db
            .query_row("SELECT count(*) FROM inbox", [], |r| r.get(0))
            .unwrap();
        assert_eq!(events, 0);
        close_tail_in(&db, "chat", FIRST + 200).unwrap();
        assert!(counts(&db).3);
        assert_eq!(counts(&db).2, FIRST + 200);
        assert_eq!(append(&db, "second", "two", FIRST + 300), None);
        db.execute_batch("COMMIT").unwrap();
        assert!(try_append_in(
            &db,
            "chat",
            OWNER,
            "two",
            &reply("second"),
            1000,
            FIRST + 100,
            FIRST + 100,
            CONFIG,
            Some("digest")
        )
        .is_err());
    }
}
