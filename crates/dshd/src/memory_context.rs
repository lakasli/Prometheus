//! Persisted memory provenance and context invalidation.
//!
//! Memory content can survive in an earlier model answer even after the memory
//! index has been corrected. A watermark prevents those earlier turns from
//! being supplied again. Original messages and already delivered replies remain
//! history; removing them is a separate retention operation.
use anyhow::{bail, ensure, Context};
use rusqlite::{params, Connection, OptionalExtension};

const INVALIDATED: &str = "记忆已更正或遗忘，旧上下文已失效；请重新发送当前问题";

/// This may be called inside the memory schema migration transaction.
pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_run_uses(
           assistant_id TEXT NOT NULL,memory_id TEXT NOT NULL REFERENCES memory_records(id),
           revision INTEGER NOT NULL CHECK(revision>0),run_id TEXT NOT NULL REFERENCES runs(id),
           PRIMARY KEY(assistant_id,memory_id,revision,run_id));
         CREATE INDEX IF NOT EXISTS memory_uses_run ON memory_run_uses(run_id);
         CREATE TABLE IF NOT EXISTS memory_context_watermarks(
           conversation_id TEXT PRIMARY KEY REFERENCES conversations(id),assistant_id TEXT NOT NULL,
           through_run_rowid INTEGER NOT NULL CHECK(through_run_rowid>=0),updated_at INTEGER NOT NULL);",
    )?;
    Ok(())
}

/// Record the exact memory revisions placed in a model request.
///
/// The caller must retrieve the memories, call this function, and persist the
/// prompt snapshot in one transaction. The run's stored owner supplies the ACL;
/// a caller cannot make a private memory public by providing another owner.
pub fn record_usage(
    db: &Connection,
    run_id: &str,
    memories: &[(String, i64)],
) -> anyhow::Result<()> {
    ensure!(!db.is_autocommit(), "memory usage requires a transaction");
    ensure!(memories.len() <= 100, "too many memory references");
    let assistant: String = db.query_row(
        "SELECT value FROM settings WHERE key='assistant.identity.v1'",
        [],
        |r| r.get(0),
    )?;
    let (owner, status): (String, String) = db.query_row(
        "SELECT owner_id,status FROM runs WHERE id=?1",
        [run_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(status == "running", "run is no longer active");
    // Validate the complete set before writing any rows. This also fails closed
    // if a revision or its visibility changed before prompt preparation.
    for (id, revision) in memories {
        let allowed: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM memory_records
              WHERE id=?1 AND assistant_id=?2 AND revision=?3 AND deleted_at IS NULL
                AND (visibility='shared' OR (visibility='private' AND source_owner_id=?4)))",
            params![id, assistant, revision, owner],
            |r| r.get(0),
        )?;
        ensure!(allowed, "memory reference is stale or unavailable");
    }
    for (id, revision) in memories {
        db.execute(
            "INSERT OR IGNORE INTO memory_run_uses(assistant_id,memory_id,revision,run_id)
             VALUES(?1,?2,?3,?4)",
            params![assistant, id, revision, run_id],
        )?;
    }
    Ok(())
}

/// Exclude older model turns using `(r.rowid > floor OR r.id = current_run)`.
/// The current input remains available even if it was queued before a change.
pub fn history_floor(db: &Connection, conversation: &str) -> anyhow::Result<i64> {
    Ok(db
        .query_row(
            "SELECT through_run_rowid FROM memory_context_watermarks WHERE conversation_id=?1",
            [conversation],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// Stop future use of a corrected, restricted, or forgotten memory.
///
/// Call this in the same transaction as the memory mutation, while retaining the
/// record's source metadata. Usage rows deliberately survive invalidation: later
/// answers may have copied an earlier answer without retrieving the memory
/// again. Repeated corrections must still reset those derived contexts.
///
/// Replies already being sent cannot safely be recalled; their `sending` state
/// is preserved. Only not-yet-claimed outbox replies are cancelled here.
pub fn invalidate(db: &Connection, assistant: &str, memory: &str) -> anyhow::Result<()> {
    ensure!(
        !db.is_autocommit(),
        "memory invalidation requires a transaction"
    );
    let source: Option<Option<String>> = db
        .query_row(
            "SELECT source_conversation_id FROM memory_records WHERE assistant_id=?1 AND id=?2",
            params![assistant, memory],
            |r| r.get(0),
        )
        .optional()?;
    let Some(source) = source else {
        bail!("memory is unavailable");
    };
    let mut conversations = {
        let mut statement = db.prepare(
            "SELECT DISTINCT r.conversation_id FROM memory_run_uses u
             JOIN runs r ON r.id=u.run_id WHERE u.assistant_id=?1 AND u.memory_id=?2",
        )?;
        let rows = statement.query_map(params![assistant, memory], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    if let Some(source) = source {
        conversations.push(source);
    }
    conversations.sort_unstable();
    conversations.dedup();
    let has_snapshots = table_exists(db, "run_prompt_snapshots")?;
    let has_kind = column_exists(db, "runs", "run_kind")?;
    let model_only = if has_kind {
        " AND run_kind='model'"
    } else {
        ""
    };
    let timestamp = crate::state::now();
    for conversation in conversations {
        crate::conversation_summary::invalidate(db, assistant, &conversation)?;
        crate::memory_candidates::invalidate(db, assistant, &conversation)?;
        crate::weixin_store::cancel_queued_for_conversation_in(db, &conversation)?;
        let through: i64 = db.query_row(
            "SELECT coalesce(max(rowid),0) FROM runs WHERE conversation_id=?1",
            [&conversation],
            |r| r.get(0),
        )?;
        db.execute(
            "INSERT INTO memory_context_watermarks(conversation_id,assistant_id,through_run_rowid,updated_at)
             VALUES(?1,?2,?3,?4) ON CONFLICT(conversation_id) DO UPDATE SET
             through_run_rowid=max(memory_context_watermarks.through_run_rowid,excluded.through_run_rowid),
             assistant_id=excluded.assistant_id,updated_at=excluded.updated_at",
            params![conversation, assistant, through, timestamp],
        )?;
        // Worker result persistence is conditional on status='running'. This
        // prevents an in-flight old response from becoming new history/outbox.
        db.execute(
            &format!(
                "UPDATE runs SET status='cancelled',error=?1,updated_at=?2
                 WHERE conversation_id=?3 AND status='running'{model_only}"
            ),
            params![INVALIDATED, timestamp, conversation],
        )?;
        db.execute(
            "UPDATE outbox SET status='cancelled',error=?1 WHERE status='queued'
                 AND run_id IN (SELECT id FROM runs WHERE conversation_id=?2 AND rowid<=?3)",
            params![INVALIDATED, conversation, through],
        )?;
        if has_snapshots {
            db.execute(
                "DELETE FROM run_prompt_snapshots WHERE run_id IN
                 (SELECT id FROM runs WHERE conversation_id=?1 AND rowid<=?2)",
                params![conversation, through],
            )?;
        }
    }
    Ok(())
}

fn table_exists(db: &Connection, table: &str) -> anyhow::Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [table],
        |r| r.get(0),
    )?)
}

fn column_exists(db: &Connection, table: &str, column: &str) -> anyhow::Result<bool> {
    // `table` is an internal constant, not an API-supplied identifier.
    let mut statement = db.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = statement.query_map([], |r| r.get::<_, String>(1))?;
    for name in rows {
        if name.context("read context schema")? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASSISTANT: &str = "assistant-one";

    fn open(path: &std::path::Path) -> Connection {
        let db = Connection::open(path).unwrap();
        db.execute_batch(include_str!("../schema.sql")).unwrap();
        db.execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS memory_records(
               id TEXT PRIMARY KEY,assistant_id TEXT NOT NULL,revision INTEGER NOT NULL,
               visibility TEXT NOT NULL,source_owner_id TEXT,source_conversation_id TEXT,deleted_at INTEGER);
             CREATE TABLE IF NOT EXISTS run_prompt_snapshots(run_id TEXT PRIMARY KEY,messages_json TEXT NOT NULL);
             INSERT OR IGNORE INTO settings(key,value) VALUES('assistant.identity.v1','assistant-one');",
        )
        .unwrap();
        initialize(&db).unwrap();
        db
    }

    fn conversation(db: &Connection, id: &str, owner: &str) {
        db.execute(
            "INSERT INTO conversations(id,owner_id,title,created_at) VALUES(?1,?2,'test',1)",
            params![id, owner],
        )
        .unwrap();
    }

    fn run(db: &Connection, id: &str, conversation: &str, owner: &str, status: &str) -> i64 {
        db.execute(
            "INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at)
             VALUES(?1,?2,?3,?1,'original question','{}',?4,1,1)",
            params![id, conversation, owner, status],
        )
        .unwrap();
        db.query_row("SELECT rowid FROM runs WHERE id=?1", [id], |r| r.get(0))
            .unwrap()
    }

    fn memory(
        db: &Connection,
        id: &str,
        visibility: &str,
        owner: Option<&str>,
        source: Option<&str>,
    ) {
        db.execute(
            "INSERT INTO memory_records(id,assistant_id,revision,visibility,source_owner_id,source_conversation_id)
             VALUES(?1,?2,1,?3,?4,?5)",
            params![id, ASSISTANT, visibility, owner, source],
        )
        .unwrap();
    }

    fn uses(db: &mut Connection, run: &str, memory: &str) {
        let tx = db.transaction().unwrap();
        record_usage(&tx, run, &[(memory.into(), 1)]).unwrap();
        tx.commit().unwrap();
    }

    fn text(db: &Connection, sql: &str, id: &str) -> String {
        db.query_row(sql, [id], |r| r.get(0)).unwrap()
    }

    #[test]
    fn shared_memory_invalidates_derived_history_and_stays_invalid_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("context.sqlite");
        let mut db = open(&path);
        conversation(&db, "web", "admin");
        conversation(&db, "qq", "qq-user");
        conversation(&db, "unrelated", "someone-else");
        memory(&db, "shared", "shared", None, None);
        run(&db, "web-used", "web", "admin", "running");
        uses(&mut db, "web-used", "shared");
        db.execute("UPDATE runs SET status='completed' WHERE id='web-used'", [])
            .unwrap();
        let web_floor = run(&db, "web-derived", "web", "admin", "completed");
        let qq_floor = run(&db, "qq-used", "qq", "qq-user", "running");
        uses(&mut db, "qq-used", "shared");
        run(&db, "other", "unrelated", "someone-else", "running");
        db.execute_batch(
            "INSERT INTO messages(id,conversation_id,run_id,role,content,created_at)
             VALUES('original','web','web-derived','assistant','original answer retained',1);
             INSERT INTO run_prompt_snapshots VALUES('web-derived','old snapshot');
             INSERT INTO run_prompt_snapshots VALUES('qq-used','old snapshot');
             INSERT INTO run_prompt_snapshots VALUES('other','unrelated snapshot');",
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        invalidate(&tx, ASSISTANT, "shared").unwrap();
        tx.commit().unwrap();
        assert_eq!(history_floor(&db, "web").unwrap(), web_floor);
        assert_eq!(history_floor(&db, "qq").unwrap(), qq_floor);
        assert_eq!(history_floor(&db, "unrelated").unwrap(), 0);
        assert_eq!(
            text(&db, "SELECT status FROM runs WHERE id=?1", "qq-used"),
            "cancelled"
        );
        assert_eq!(
            text(&db, "SELECT status FROM runs WHERE id=?1", "other"),
            "running"
        );
        assert_eq!(
            text(&db, "SELECT content FROM messages WHERE id=?1", "original"),
            "original answer retained"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM run_prompt_snapshots", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(db);
        let mut db = open(&path);
        assert_eq!(history_floor(&db, "web").unwrap(), web_floor);
        let next = run(&db, "web-next", "web", "admin", "queued");
        let eligible: Vec<String> = {
            let mut statement = db.prepare("SELECT id FROM runs WHERE conversation_id='web' AND (rowid>?1 OR id='web-next')").unwrap();
            let rows = statement
                .query_map([history_floor(&db, "web").unwrap()], |r| r.get(0))
                .unwrap();
            rows.collect::<rusqlite::Result<_>>().unwrap()
        };
        assert_eq!(eligible, ["web-next"]);
        // Repeated invalidation must protect conversations which copied an old
        // answer without directly retrieving the same memory a second time.
        let tx = db.transaction().unwrap();
        invalidate(&tx, ASSISTANT, "shared").unwrap();
        invalidate(&tx, ASSISTANT, "shared").unwrap();
        tx.commit().unwrap();
        assert_eq!(history_floor(&db, "web").unwrap(), next);
    }

    #[test]
    fn invalidation_cancels_queued_delivery_but_preserves_in_flight_and_sent_evidence() {
        let mut db = open(std::path::Path::new(":memory:"));
        conversation(&db, "source", "owner");
        memory(&db, "private", "private", Some("owner"), Some("source"));
        db.execute(
            "ALTER TABLE runs ADD COLUMN run_kind TEXT NOT NULL DEFAULT 'model'",
            [],
        )
        .unwrap();
        for status in ["queued", "sending", "sent"] {
            run(&db, status, "source", "owner", "completed");
            db.execute(
                "INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,deadline,created_at)
                 VALUES(?1,?1,'account','credential','{}','old answer',?1,100,1)",
                [status],
            )
            .unwrap();
        }
        run(&db, "old-show", "source", "owner", "completed");
        db.execute(
            "UPDATE runs SET run_kind='memory_command' WHERE id='old-show'",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,deadline,created_at) VALUES('old-show','old-show','account','credential','{}','previously inspected memory','queued',100,1)",[]).unwrap();
        run(&db, "control", "source", "owner", "running");
        db.execute(
            "UPDATE runs SET run_kind='memory_command' WHERE id='control'",
            [],
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        invalidate(&tx, ASSISTANT, "private").unwrap();
        tx.commit().unwrap();
        assert_eq!(
            text(&db, "SELECT status FROM outbox WHERE id=?1", "queued"),
            "cancelled"
        );
        assert_eq!(
            text(&db, "SELECT status FROM outbox WHERE id=?1", "sending"),
            "sending"
        );
        assert_eq!(
            text(&db, "SELECT status FROM outbox WHERE id=?1", "sent"),
            "sent"
        );
        assert_eq!(
            text(&db, "SELECT status FROM runs WHERE id=?1", "control"),
            "running"
        );
        assert_eq!(
            text(&db, "SELECT status FROM outbox WHERE id=?1", "old-show"),
            "cancelled"
        );
    }

    #[test]
    fn provenance_fails_closed_on_private_acl_stale_revision_and_deleted_memory() {
        let mut db = open(std::path::Path::new(":memory:"));
        conversation(&db, "mine", "owner");
        conversation(&db, "theirs", "other");
        memory(&db, "shared", "shared", None, None);
        memory(&db, "private", "private", Some("owner"), Some("mine"));
        run(&db, "mine-run", "mine", "owner", "running");
        run(&db, "their-run", "theirs", "other", "running");
        assert!(record_usage(&db, "mine-run", &[("shared".into(), 1)]).is_err());
        let tx = db.transaction().unwrap();
        assert!(record_usage(
            &tx,
            "their-run",
            &[("shared".into(), 1), ("private".into(), 1)]
        )
        .is_err());
        assert_eq!(
            tx.query_row("SELECT count(*) FROM memory_run_uses", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(record_usage(&tx, "mine-run", &[("private".into(), 2)]).is_err());
        record_usage(&tx, "mine-run", &[("private".into(), 1)]).unwrap();
        tx.execute(
            "UPDATE memory_records SET deleted_at=1 WHERE id='private'",
            [],
        )
        .unwrap();
        assert!(record_usage(&tx, "mine-run", &[("private".into(), 1)]).is_err());
        tx.execute(
            "UPDATE memory_records SET assistant_id='other-assistant' WHERE id='shared'",
            [],
        )
        .unwrap();
        assert!(record_usage(&tx, "mine-run", &[("shared".into(), 1)]).is_err());
        tx.execute("UPDATE runs SET status='cancelled' WHERE id='mine-run'", [])
            .unwrap();
        assert!(record_usage(&tx, "mine-run", &[]).is_err());
        tx.rollback().unwrap();
    }

    #[test]
    fn invalidation_and_mutation_roll_back_together_without_optional_prompt_table() {
        let mut db = open(std::path::Path::new(":memory:"));
        db.execute("DROP TABLE run_prompt_snapshots", []).unwrap();
        conversation(&db, "source", "owner");
        memory(&db, "memory", "private", Some("owner"), Some("source"));
        run(&db, "active", "source", "owner", "running");
        assert!(invalidate(&db, ASSISTANT, "memory").is_err());
        let tx = db.transaction().unwrap();
        tx.execute(
            "UPDATE memory_records SET deleted_at=1 WHERE id='memory'",
            [],
        )
        .unwrap();
        invalidate(&tx, ASSISTANT, "memory").unwrap();
        assert_eq!(
            text(&tx, "SELECT status FROM runs WHERE id=?1", "active"),
            "cancelled"
        );
        tx.rollback().unwrap();
        assert_eq!(history_floor(&db, "source").unwrap(), 0);
        assert_eq!(
            text(&db, "SELECT status FROM runs WHERE id=?1", "active"),
            "running"
        );
        assert_eq!(
            db.query_row(
                "SELECT deleted_at FROM memory_records WHERE id='memory'",
                [],
                |r| r.get::<_, Option<i64>>(0)
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn domain_mutations_invalidate_real_application_prompt_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let state = crate::state::AppState::open(crate::state::Options {
            data_dir: directory.path().to_owned(),
            origin: "http://127.0.0.1:18420".into(),
            allow_private_model: true,
            cookie_secure: false,
            web_dir: directory.path().join("web"),
        })
        .unwrap();
        let mut db = state.0.db.lock().unwrap();
        conversation(&db, "source", "owner");
        let remembered = crate::memory::create(
            &mut db,
            "admin",
            &crate::memory::CreateMemory {
                content: "old assistant memory".into(),
                visibility: crate::memory::Visibility::Shared,
                source_conversation_id: Some("source".into()),
                source_message_id: None,
                publish: true,
            },
        )
        .unwrap();
        let floor = run(&db, "live", "source", "owner", "running");
        {
            let tx = db.transaction().unwrap();
            record_usage(&tx, "live", &[(remembered.id.clone(), remembered.revision)]).unwrap();
            tx.execute(
                "INSERT INTO run_prompt_snapshots(run_id,messages_json,digest,compiler_version,created_at)
                 VALUES('live','old prompt with memory','digest','test',1)",
                [],
            )
            .unwrap();
            tx.commit().unwrap();
        }
        let corrected = crate::memory::update(
            &mut db,
            "admin",
            &remembered.id,
            remembered.revision,
            &crate::memory::UpdateMemory {
                content: "corrected assistant memory".into(),
                visibility: None,
                publish: false,
            },
        )
        .unwrap();
        assert_eq!(history_floor(&db, "source").unwrap(), floor);
        assert_eq!(
            text(&db, "SELECT status FROM runs WHERE id=?1", "live"),
            "cancelled"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM run_prompt_snapshots", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        crate::memory::forget(&mut db, "admin", &corrected.id, corrected.revision).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM memory_versions WHERE memory_id=?1 AND content IS NOT NULL",
                [&corrected.id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(history_floor(&db, "source").unwrap(), floor);
    }
}
