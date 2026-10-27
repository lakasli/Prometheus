"""Exercise downgrade gates against isolated SQLite feature fixtures."""
import importlib.util
import json
import pathlib
import re
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from contextlib import contextmanager


spec = importlib.util.spec_from_file_location(
    "compatibility", pathlib.Path(__file__).resolve().parents[2] / "deploy/check-compatibility.py"
)
compatibility = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compatibility)


PHASE2_TABLES = {
    "conversation_state": (
        "conversation_state_heads", "conversation_state_entities",
        "conversation_state_sources", "conversation_state_observations",
        "conversation_state_uses", "conversation_state_snapshots",
        "conversation_state_source_tombstones",
    ),
    "commitments": (
        "conversation_commitments", "conversation_commitment_epochs",
        "conversation_commitment_events", "conversation_commitment_usage",
        "conversation_commitment_contexts", "conversation_commitment_sources",
        "conversation_commitment_idempotency",
    ),
    "context_budget": (
        "context_budget_policy_versions", "context_budget_policy_current",
        "context_budget_snapshots",
    ),
}


def rest_schema_sql():
    """Use actual Rust DDL, independently of the deployment column manifest."""
    root = pathlib.Path(__file__).resolve().parents[2] / "crates/dshd/src"
    statements = []
    for source in ("rest_store.rs", "rest_consolidation.rs", "rest_api.rs", "rest_runtime.rs"):
        text = (root / source).read_text(encoding="utf-8")
        statements.extend(re.findall(
            r"CREATE TABLE IF NOT EXISTS (?:rest_\w+|assistant_rest_states|consolidation_jobs)\(.*?\);",
            text, re.DOTALL,
        ))
    assert len(statements) == 21, "Rest schema changed: update the deployment reader contract"
    return "\n".join(statements)


REST_SEED_POLICY = {
    "natural_enabled": False, "schedule_enabled": False, "time_zone": "Asia/Shanghai",
    "sleep_local": None, "wake_local": None, "default_sleep_seconds": None,
    "max_sleep_seconds": 86400, "drain_seconds": 30, "consolidation_seconds": 300,
    "queue_limit": 128, "max_job_attempts": 3, "max_cycle_calls": 8,
    "max_cycle_reserved_tokens": 24000, "ack_seconds": 30,
}


def seed_rest(db):
    db.executescript(rest_schema_sql())
    db.execute("INSERT INTO rest_meta VALUES(1,1)")
    db.execute("INSERT INTO rest_policies VALUES('fixture-assistant',1,?,'initial_disabled',0)",
               (json.dumps(REST_SEED_POLICY),))
    db.execute("INSERT INTO assistant_rest_states VALUES('fixture-assistant',0,1,'awake',NULL,NULL,NULL,NULL,0,NULL,NULL,NULL)")


def rest_row(db, table, **overrides):
    """Populate one semantic row with synthetic metadata, never production data."""
    columns = list(db.execute(f"PRAGMA table_info({table})"))
    integers = {"policy_revision", "grant_revision", "seq", "ordinal", "revision",
                "generation", "job_fence", "stage", "fence", "attempts", "nominal_at"}
    values = {name: (1 if "INT" in kind.upper() or name in integers else "synthetic")
              for _, name, kind, *_ in columns}
    values.update({"assistant_id": "fixture-assistant", "status": "unknown",
                   "run_id": "synthetic-run", "operation": "wake", "kind": "diary_day"})
    values.update(overrides)
    names = [row[1] for row in columns]
    db.execute(f"INSERT INTO {table}({','.join(names)}) VALUES({','.join('?' for _ in names)})",
               [values[name] for name in names])


@contextmanager
def connect(path):
    db = sqlite3.connect(path)
    try:
        with db:
            yield db
    finally:
        db.close()


class CompatibilityTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="dsh-compat-")
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name)
        self.data = self.root / "data"
        self.data.mkdir()
        self.release = self.root / "release"
        (self.release / "deploy").mkdir(parents=True)
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE runs(id TEXT,character_snapshot_json TEXT); CREATE TABLE settings(key TEXT,value TEXT);")

    def test_legacy_database_can_use_legacy_release(self):
        compatibility.check(self.data, self.release)

    def test_sleep_context_boundary_requires_daily_life_reader_without_erasing_data(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE daily_life_events(id TEXT); CREATE TABLE sleep_context_boundaries(conversation_id TEXT); INSERT INTO sleep_context_boundaries VALUES('synthetic');")
        before=(self.data / "dsh.db").read_bytes()
        with self.assertRaisesRegex(RuntimeError,"daily routine"):
            compatibility.check(self.data,self.release)
        self.assertEqual(before,(self.data / "dsh.db").read_bytes())

    def test_rest_empty_complete_additive_schema_and_exact_default_seeds_allow_legacy(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript(rest_schema_sql())
        compatibility.check(self.data, self.release)
        with connect(self.data / "dsh.db") as db:
            seed_rest(db)
            # Host time sampling is bookkeeping, not a sleep/control decision.
            db.execute("UPDATE assistant_rest_states SET logical_at=1793000000,clock_epoch='synthetic-boot',mono_ms=654321")
        compatibility.check(self.data, self.release)

    def test_rest_default_json_whitespace_and_key_order_do_not_prevent_unused_rollback(self):
        with connect(self.data / "dsh.db") as db:
            seed_rest(db)
            db.execute("UPDATE rest_policies SET policy_json=?", (json.dumps(REST_SEED_POLICY, sort_keys=True, indent=2),))
        compatibility.check(self.data, self.release)

    def test_rest_saved_policy_requires_reader_even_when_unchanged_off_and_never_requested(self):
        with connect(self.data / "dsh.db") as db:
            seed_rest(db)
            db.execute("UPDATE rest_policies SET revision=2,actor_ref='admin-synthetic'")
        with self.assertRaisesRegex(RuntimeError, "Rest durable"):
            compatibility.check(self.data, self.release)
        (self.release / "deploy/release-capabilities.json").write_text(json.dumps({"rest": 1}))
        compatibility.check(self.data, self.release)

    def test_rest_nondefault_or_enabled_policy_needs_reader_without_revision_increment(self):
        with connect(self.data / "dsh.db") as db:
            seed_rest(db)
        for key, value in (("natural_enabled", True), ("schedule_enabled", True),
                           ("default_sleep_seconds", 300), ("max_cycle_calls", 4),
                           ("time_zone", "UTC"), ("sleep_local", "23:00")):
            with self.subTest(field=key):
                policy = {**REST_SEED_POLICY, key: value}
                with connect(self.data / "dsh.db") as db:
                    db.execute("UPDATE rest_policies SET policy_json=?", (json.dumps(policy),))
                with self.assertRaisesRegex(RuntimeError, "Rest durable"):
                    compatibility.check(self.data, self.release)

    def test_rest_every_semantic_auxiliary_row_requires_reader_even_if_erased_or_finished(self):
        with connect(self.data / "dsh.db") as db:
            seed_rest(db)
        manifest = self.release / "deploy/release-capabilities.json"
        for table in set(compatibility.REST_COLUMNS) - {"rest_meta", "rest_policies", "assistant_rest_states"}:
            with self.subTest(table=table):
                with connect(self.data / "dsh.db") as db:
                    rest_row(db, table, **({"status": "pending"} if table == "rest_control_candidates" else {}))
                with self.assertRaisesRegex(RuntimeError, "Rest durable"):
                    compatibility.check(self.data, self.release)
                manifest.write_text(json.dumps({"rest": 1}))
                compatibility.check(self.data, self.release)
                manifest.unlink()
                with connect(self.data / "dsh.db") as db:
                    db.execute(f"DELETE FROM {table}")
        compatibility.check(self.data, self.release)

    def test_rest_revoke_unknown_pending_and_cached_raw_result_never_become_unused(self):
        with connect(self.data / "dsh.db") as db:
            seed_rest(db)
        for table, values in (
            ("rest_control_grants", {"can_sleep": 0, "can_wake": 0, "valid_until": 1}),
            ("consolidation_jobs", {"status": "unknown", "result_digest": None}),
            ("rest_job_attempts", {"finished_at": None, "outcome": None}),
            ("rest_control_candidates", {"status": "pending", "finished_at": None}),
            ("rest_deferred_presentations", {"raw_generation": "private-synthetic-cache-canary", "media_inflight": 0}),
            ("rest_deferred_presentations", {"raw_generation": "private-synthetic-cache-canary", "media_inflight": 1}),
        ):
            with self.subTest(table=table, values=values):
                with connect(self.data / "dsh.db") as db:
                    rest_row(db, table, **values)
                with self.assertRaisesRegex(RuntimeError, "Rest durable") as captured:
                    compatibility.check(self.data, self.release)
                self.assertNotIn("private-synthetic-cache-canary", str(captured.exception))
                with connect(self.data / "dsh.db") as db:
                    db.execute(f"DELETE FROM {table}")

    def test_rest_generation_cycle_or_nonawake_state_requires_reader(self):
        with connect(self.data / "dsh.db") as db:
            seed_rest(db)
        for clause in ("generation=1", "revision=2", "phase='sleep_pending'", "phase='unknown'",
                       "cycle_id='synthetic-cycle'", "wake_at=1793000000", "reason='explicit_wake'"):
            with self.subTest(clause=clause):
                with connect(self.data / "dsh.db") as db:
                    db.execute("UPDATE assistant_rest_states SET generation=0,revision=1,phase='awake',cycle_id=NULL,wake_at=NULL,reason=NULL")
                    db.execute(f"UPDATE assistant_rest_states SET {clause}")
                with self.assertRaisesRegex(RuntimeError, "Rest durable"):
                    compatibility.check(self.data, self.release)

    def test_rest_future_schema_rejects_old_and_current_lower_readers(self):
        with connect(self.data / "dsh.db") as db:
            seed_rest(db)
            db.execute("UPDATE rest_meta SET schema_version=2")
        manifest = self.release / "deploy/release-capabilities.json"
        for reader in (0, 1):
            with self.subTest(reader=reader):
                manifest.write_text(json.dumps({"rest": reader}))
                with self.assertRaisesRegex(RuntimeError, "Rest schema version"):
                    compatibility.check(self.data, self.release)
        manifest.write_text(json.dumps({"rest": 2}))
        compatibility.check(self.data, self.release)

    def test_rest_partial_table_schema_fails_closed_for_both_readers_even_if_empty(self):
        with connect(self.data / "dsh.db") as db:
            seed_rest(db)
        manifest = self.release / "deploy/release-capabilities.json"
        for table in compatibility.REST_COLUMNS:
            with self.subTest(table=table):
                with connect(self.data / "dsh.db") as db:
                    db.execute(f"ALTER TABLE {table} RENAME TO missing_rest_table")
                for reader in (0, 1):
                    manifest.write_text(json.dumps({"rest": reader}))
                    with self.assertRaisesRegex(RuntimeError, "Rest schema is incomplete"):
                        compatibility.check(self.data, self.release)
                with connect(self.data / "dsh.db") as db:
                    db.execute(f"ALTER TABLE missing_rest_table RENAME TO {table}")

    def test_rest_missing_or_unknown_column_fails_closed_for_capable_reader(self):
        with connect(self.data / "dsh.db") as db:
            seed_rest(db)
            db.execute("ALTER TABLE rest_deferred_presentations DROP COLUMN media_inflight")
        manifest = self.release / "deploy/release-capabilities.json"
        manifest.write_text(json.dumps({"rest": 1}))
        with self.assertRaisesRegex(RuntimeError, "Rest schema columns"):
            compatibility.check(self.data, self.release)
        with connect(self.data / "dsh.db") as db:
            db.execute("ALTER TABLE rest_deferred_presentations ADD COLUMN media_inflight INTEGER")
            db.execute("ALTER TABLE rest_control_grants ADD COLUMN unknown_future_grant TEXT")
        with self.assertRaisesRegex(RuntimeError, "Rest schema columns"):
            compatibility.check(self.data, self.release)

    def test_rest_missing_seed_or_malformed_policy_never_counts_as_unused(self):
        with connect(self.data / "dsh.db") as db:
            seed_rest(db)
        duplicate = '{"natural_enabled":true,' + json.dumps(REST_SEED_POLICY)[1:]
        for policy in ("not-json", "{}", "[]", duplicate, json.dumps({**REST_SEED_POLICY, "natural_enabled": 0}),
                       json.dumps({**REST_SEED_POLICY, "default_sleep_seconds": False})):
            with self.subTest(policy=policy):
                with connect(self.data / "dsh.db") as db:
                    db.execute("UPDATE rest_policies SET policy_json=?", (policy,))
                with self.assertRaisesRegex(RuntimeError, "Rest policy"):
                    compatibility.check(self.data, self.release)
        with connect(self.data / "dsh.db") as db:
            db.execute("DELETE FROM rest_policies")
        with self.assertRaisesRegex(RuntimeError, "Rest initial state is incomplete"):
            compatibility.check(self.data, self.release)

    def test_rest_gate_is_read_only_and_manifest_advertises_reader(self):
        with connect(self.data / "dsh.db") as db:
            seed_rest(db)
            rest_row(db, "rest_control_candidates", status="pending")
        database = self.data / "dsh.db"
        before = database.read_bytes()
        with self.assertRaisesRegex(RuntimeError, "Rest durable"):
            compatibility.check(self.data, self.release)
        self.assertEqual(before, database.read_bytes())
        current_manifest = pathlib.Path(__file__).resolve().parents[2] / "deploy/release-capabilities.json"
        self.assertEqual(json.loads(current_manifest.read_text())["rest"], 2)

    def test_rest_cli_returns_exit_two_for_unsafe_downgrade_without_printing_cache(self):
        with connect(self.data / "dsh.db") as db:
            seed_rest(db)
            rest_row(db, "rest_deferred_presentations", raw_generation="private-cli-cache-canary")
        script = pathlib.Path(__file__).resolve().parents[2] / "deploy/check-compatibility.py"
        result = subprocess.run([sys.executable, "-S", "-B", str(script), str(self.data), str(self.release)],
                                capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 2)
        self.assertIn("Rest durable", result.stderr)
        self.assertNotIn("private-cli-cache-canary", result.stdout + result.stderr)
        (self.release / "deploy/release-capabilities.json").write_text(json.dumps({"rest": 1}))
        accepted = subprocess.run([sys.executable, "-S", "-B", str(script), str(self.data), str(self.release)],
                                  capture_output=True, text=True, check=False)
        self.assertEqual(accepted.returncode, 0)

    def test_conversation_semantics_cannot_be_lost_on_downgrade(self):
        for feature, required, table in (
            ("conversation_identity", 2, "run_conversation_bindings"),
            ("delivery_context", 1, "delivery_context_snapshots"),
            ("turn_control", 1, "run_publication_decisions"),
        ):
            with self.subTest(feature=feature):
                with connect(self.data / "dsh.db") as db:
                    db.execute(f"CREATE TABLE {table}(id TEXT)")
                compatibility.check(self.data, self.release)
                with connect(self.data / "dsh.db") as db:
                    db.execute(f"INSERT INTO {table} VALUES('durable-decision')")
                with self.assertRaisesRegex(RuntimeError, feature):
                    compatibility.check(self.data, self.release)
                manifest = self.release / "deploy/release-capabilities.json"
                manifest.write_text(json.dumps({feature: required}))
                compatibility.check(self.data, self.release)
                with connect(self.data / "dsh.db") as db:
                    db.execute(f"DROP TABLE {table}")
                manifest.unlink()

    def test_delivery_bookkeeping_alone_does_not_prevent_safe_unused_rollback(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE delivery_context_revisions(run_id TEXT,revision INTEGER); INSERT INTO delivery_context_revisions VALUES('old-outbox',1); CREATE TABLE diary_capture_migrations(key TEXT,value INTEGER); INSERT INTO diary_capture_migrations VALUES('receipt_projection_v1',1);")
        compatibility.check(self.data, self.release)

    def test_all_empty_phase2_tables_allow_unused_rollback(self):
        with connect(self.data / "dsh.db") as db:
            for tables in PHASE2_TABLES.values():
                for table in tables:
                    db.execute(f"CREATE TABLE {table}(id TEXT)")
        before = (self.data / "dsh.db").read_bytes()
        compatibility.check(self.data, self.release)
        self.assertEqual((self.data / "dsh.db").read_bytes(), before)

    def test_each_phase2_durable_table_independently_requires_its_reader(self):
        manifest = self.release / "deploy/release-capabilities.json"
        for feature, tables in PHASE2_TABLES.items():
            for table in tables:
                with self.subTest(feature=feature, table=table):
                    with connect(self.data / "dsh.db") as db:
                        db.execute(f"CREATE TABLE {table}(id TEXT,body TEXT)")
                        # Bodyless/orphaned records still retain revocation,
                        # revision or replay semantics after source deletion.
                        db.execute(f"INSERT INTO {table} VALUES('retained-proof',NULL)")
                    before = (self.data / "dsh.db").read_bytes()
                    for capabilities in ({}, {feature: 0}, {
                        other: 1 for other in PHASE2_TABLES if other != feature
                    }):
                        manifest.write_text(json.dumps(capabilities))
                        with self.assertRaisesRegex(RuntimeError, feature):
                            compatibility.check(self.data, self.release)
                    manifest.write_text(json.dumps({feature: 1}))
                    compatibility.check(self.data, self.release)
                    self.assertEqual((self.data / "dsh.db").read_bytes(), before)
                    with connect(self.data / "dsh.db") as db:
                        db.execute(f"DROP TABLE {table}")
                    manifest.unlink()

    def test_source_tombstone_alone_blocks_reader_without_state_semantics(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE conversation_state_source_tombstones(source_kind TEXT,source_id TEXT,through_revision INTEGER,invalidated_at INTEGER); INSERT INTO conversation_state_source_tombstones VALUES('run_input','deleted-source',7,123);")
        manifest = self.release / "deploy/release-capabilities.json"
        manifest.write_text(json.dumps({"schema_reader": 10, "memory_context": 1}))
        with self.assertRaisesRegex(RuntimeError, "conversation_state"):
            compatibility.check(self.data, self.release)

    def test_budget_policy_without_prompt_snapshot_still_blocks_old_worker(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE context_budget_policy_versions(revision INTEGER,policy_json TEXT); INSERT INTO context_budget_policy_versions VALUES(1,'{}'); CREATE TABLE context_budget_policy_current(revision INTEGER); INSERT INTO context_budget_policy_current VALUES(1); CREATE TABLE context_budget_snapshots(run_id TEXT);")
        with self.assertRaisesRegex(RuntimeError, "context_budget"):
            compatibility.check(self.data, self.release)
        (self.release / "deploy/release-capabilities.json").write_text(json.dumps({"context_budget": 1}))
        compatibility.check(self.data, self.release)

    def test_current_manifest_advertises_all_phase2_readers(self):
        manifest = pathlib.Path(__file__).resolve().parents[2] / "deploy/release-capabilities.json"
        capabilities = json.loads(manifest.read_text())
        for feature in PHASE2_TABLES:
            self.assertGreaterEqual(capabilities.get(feature, 0), 1, feature)

    def test_knowledge_and_schedule_state_requires_its_reader(self):
        for feature, table in (("diary", "diary_events"),
                               ("relationships", "relationship_people"),
                               ("schedule", "schedule_activities")):
            with self.subTest(feature=feature):
                with connect(self.data / "dsh.db") as db:
                    db.execute(f"CREATE TABLE {table}(id TEXT)")
                compatibility.check(self.data, self.release)
                with connect(self.data / "dsh.db") as db:
                    db.execute(f"INSERT INTO {table} VALUES('tombstone')")
                with self.assertRaisesRegex(RuntimeError, feature):
                    compatibility.check(self.data, self.release)
                manifest = self.release / "deploy/release-capabilities.json"
                manifest.write_text(json.dumps({feature: 1}))
                compatibility.check(self.data, self.release)
                with connect(self.data / "dsh.db") as db:
                    db.execute(f"DROP TABLE {table}")
                manifest.unlink()

    def test_hierarchy_fragments_require_the_v2_diary_reader_even_before_first_use(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE diary_meta(schema_version INTEGER); INSERT INTO diary_meta VALUES(1);")
        manifest = self.release / "deploy/release-capabilities.json"
        manifest.write_text(json.dumps({"diary": 1}))
        compatibility.check(self.data, self.release)
        with connect(self.data / "dsh.db") as db:
            db.execute("UPDATE diary_meta SET schema_version=2")
        with self.assertRaisesRegex(RuntimeError, "diary schema version"):
            compatibility.check(self.data, self.release)
        manifest.write_text(json.dumps({"diary": 2}))
        compatibility.check(self.data, self.release)

    def test_saved_single_personality_requires_compatible_release(self):
        with connect(self.data / "dsh.db") as db:
            db.execute("INSERT INTO settings VALUES('assistant.personality.v1','{}')")
        with self.assertRaisesRegex(RuntimeError, "single assistant personality"):
            compatibility.check(self.data, self.release)
        (self.release / "deploy/release-capabilities.json").write_text(json.dumps({"single_assistant_personality":1}))
        compatibility.check(self.data, self.release)

    def test_plugin_registry_approval_and_route_state_cannot_silently_disappear_on_rollback(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE plugin_factory_versions(id TEXT); INSERT INTO plugin_factory_versions VALUES('approved-version');")
        with self.assertRaisesRegex(RuntimeError,"Component plugin approvals"):
            compatibility.check(self.data,self.release)
        (self.release / "deploy/release-capabilities.json").write_text(json.dumps({"plugin_factory":1}))
        compatibility.check(self.data,self.release)

    def test_acknowledged_qq_receive_queue_requires_its_own_reader_capability(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE qq_received_events(status TEXT); INSERT INTO qq_received_events VALUES('queued');")
        with self.assertRaisesRegex(RuntimeError, "durable QQ reception"):
            compatibility.check(self.data, self.release)
        (self.release / "deploy/release-capabilities.json").write_text(json.dumps({"qq_durable_receive":1}))
        compatibility.check(self.data, self.release)

    def test_generated_asset_source_cancellation_cannot_be_downgraded(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE conversation_media_assets(source_run_id TEXT); INSERT INTO conversation_media_assets VALUES('running-source');")
        manifest=self.release / "deploy/release-capabilities.json"
        manifest.write_text(json.dumps({"conversation_media":1}))
        with self.assertRaisesRegex(RuntimeError, "source-run cancellation"):
            compatibility.check(self.data, self.release)
        manifest.write_text(json.dumps({"conversation_media":1,"media_source_runs":1}))
        compatibility.check(self.data, self.release)

    def test_empty_qq_text_batch_tables_allow_prebatch_rollback(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE qq_text_batches(run_id TEXT,status TEXT); CREATE TABLE qq_text_fragments(run_id TEXT,message_id TEXT);")
        compatibility.check(self.data, self.release)

    def test_completed_qq_text_batch_still_requires_follower_deduplication_reader(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE qq_text_batches(run_id TEXT,status TEXT); INSERT INTO qq_text_batches VALUES('finished-run','completed');")
        manifest = self.release / "deploy/release-capabilities.json"
        manifest.write_text(json.dumps({"schema_reader": 10, "qq_durable_receive": 1}))
        with self.assertRaisesRegex(RuntimeError, "QQ text batch.*deduplication"):
            compatibility.check(self.data, self.release)

    def test_qq_text_batches_and_fragments_allow_the_feature_capable_reader(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE qq_text_batches(run_id TEXT,status TEXT); CREATE TABLE qq_text_fragments(run_id TEXT,message_id TEXT); INSERT INTO qq_text_batches VALUES('merged-run','queued'); INSERT INTO qq_text_fragments VALUES('merged-run','follower-message');")
        manifest = self.release / "deploy/release-capabilities.json"
        manifest.write_text(json.dumps({"qq_text_batches": 1}))
        compatibility.check(self.data, self.release)

    def test_residual_qq_text_fragment_alone_blocks_old_reader(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE qq_text_fragments(run_id TEXT,message_id TEXT); INSERT INTO qq_text_fragments VALUES('historical-run','follower-message');")
        with self.assertRaisesRegex(RuntimeError, "QQ text batch.*deduplication"):
            compatibility.check(self.data, self.release)
        manifest = self.release / "deploy/release-capabilities.json"
        manifest.write_text(json.dumps({"qq_text_batches": 1}))
        compatibility.check(self.data, self.release)

    def test_persona_work_blocks_legacy_but_allows_capable_release(self):
        with connect(self.data / "dsh.db") as db:
            db.execute("INSERT INTO runs VALUES('fixture','{}')")
        with self.assertRaisesRegex(RuntimeError, "character snapshots"):
            compatibility.check(self.data, self.release)
        (self.release / "deploy/release-capabilities.json").write_text(json.dumps({"schema_reader": 1, "character_snapshot": 1}))
        compatibility.check(self.data, self.release)

    def test_binding_without_queued_work_still_blocks_semantic_downgrade(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE conversation_character_bindings(conversation_id TEXT); INSERT INTO conversation_character_bindings VALUES('fixture');")
        with self.assertRaises(RuntimeError):
            compatibility.check(self.data, self.release)

    def test_future_schema_requires_compatible_reader(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE dsh_schema_migrations(version INTEGER); INSERT INTO dsh_schema_migrations VALUES(2);")
        (self.release / "deploy/release-capabilities.json").write_text(json.dumps({"schema_reader": 1, "character_snapshot": 1}))
        with self.assertRaisesRegex(RuntimeError, "database schema"):
            compatibility.check(self.data, self.release)

    def test_empty_additive_memory_tables_allow_pre_memory_rollback(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE memory_records(id TEXT); CREATE TABLE memory_context_watermarks(conversation_id TEXT);")
        compatibility.check(self.data, self.release)

    def test_memory_tombstone_requires_memory_and_context_readers(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE memory_records(id TEXT,content TEXT); INSERT INTO memory_records VALUES('forgotten',NULL);")
        manifest = self.release / "deploy/release-capabilities.json"
        manifest.write_text(json.dumps({"schema_reader": 2, "assistant_memory": 1}))
        with self.assertRaisesRegex(RuntimeError, "assistant memory"):
            compatibility.check(self.data, self.release)
        manifest.write_text(json.dumps({"schema_reader": 2, "assistant_memory": 1, "memory_context": 1}))
        compatibility.check(self.data, self.release)

    def test_context_watermark_without_record_still_blocks_old_reader(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE memory_context_watermarks(conversation_id TEXT); INSERT INTO memory_context_watermarks VALUES('history');")
        with self.assertRaisesRegex(RuntimeError, "assistant memory"):
            compatibility.check(self.data, self.release)

    def test_used_summary_metadata_requires_summary_reader_even_after_clear(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE conversation_summary_versions(revision INTEGER); INSERT INTO conversation_summary_versions VALUES(1);")
        manifest = self.release / "deploy/release-capabilities.json"
        manifest.write_text(json.dumps({"schema_reader": 3}))
        with self.assertRaisesRegex(RuntimeError, "conversation summary"):
            compatibility.check(self.data, self.release)
        manifest.write_text(json.dumps({"schema_reader": 3, "conversation_summary": 1}))
        compatibility.check(self.data, self.release)

    def test_empty_weixin_tables_do_not_block_first_upgrade_rollback(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE weixin_account(account_id TEXT); CREATE TABLE weixin_outbox(run_id TEXT);")
        compatibility.check(self.data, self.release)

    def test_weixin_account_or_reply_requires_both_channel_and_private_context_reader(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE weixin_run_replies(run_id TEXT); INSERT INTO weixin_run_replies VALUES('synthetic');")
        manifest = self.release / "deploy/release-capabilities.json"
        manifest.write_text(json.dumps({"schema_reader": 4, "weixin_ilink": 1}))
        with self.assertRaisesRegex(RuntimeError, "private reply context"):
            compatibility.check(self.data, self.release)
        manifest.write_text(json.dumps({"schema_reader": 4, "weixin_ilink": 1, "weixin_private_context": 1}))
        compatibility.check(self.data, self.release)

    def test_empty_candidate_tables_allow_rollback_before_first_job(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE memory_candidate_jobs(id TEXT); CREATE TABLE memory_candidates(id TEXT);")
        compatibility.check(self.data, self.release)

    def test_cleared_candidate_job_still_requires_review_and_invalidation_reader(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE memory_candidate_jobs(id TEXT,status TEXT); INSERT INTO memory_candidate_jobs VALUES('synthetic','cancelled');")
        manifest = self.release / "deploy/release-capabilities.json"
        manifest.write_text(json.dumps({"schema_reader": 5}))
        with self.assertRaisesRegex(RuntimeError, "candidate review"):
            compatibility.check(self.data, self.release)
        manifest.write_text(json.dumps({"schema_reader": 5, "memory_candidates": 1}))
        compatibility.check(self.data, self.release)

    def test_empty_image_analysis_table_allows_unused_upgrade_rollback(self):
        with connect(self.data / "dsh.db") as db:
            db.execute("CREATE TABLE image_analysis_jobs(id TEXT)")
        compatibility.check(self.data, self.release)

    def test_image_analysis_tombstone_requires_ownership_and_erasure_reader(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE image_analysis_jobs(id TEXT,status TEXT); INSERT INTO image_analysis_jobs VALUES('synthetic','deleted');")
        manifest = self.release / "deploy/release-capabilities.json"
        manifest.write_text(json.dumps({"schema_reader": 6}))
        with self.assertRaisesRegex(RuntimeError, "image analysis ownership"):
            compatibility.check(self.data, self.release)
        manifest.write_text(json.dumps({"schema_reader": 6, "standalone_image_analysis": 1}))
        compatibility.check(self.data, self.release)

    def test_empty_wasm_tables_and_resource_probes_allow_unused_upgrade_rollback(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE wasm_workbench_jobs(id TEXT); CREATE TABLE wasm_workbench_artifacts(id TEXT); CREATE TABLE wasm_workbench_units(unit_name TEXT); INSERT INTO wasm_workbench_units VALUES('probe-only');")
        compatibility.check(self.data, self.release)

    def test_wasm_job_or_revoked_artifact_retains_approval_reader_requirement(self):
        for table in ("wasm_workbench_jobs", "wasm_workbench_artifacts"):
            with self.subTest(table=table):
                with connect(self.data / "dsh.db") as db:
                    db.execute(f"CREATE TABLE {table}(id TEXT,status TEXT)")
                    db.execute(f"INSERT INTO {table} VALUES('synthetic','revoked')")
                manifest = self.release / "deploy/release-capabilities.json"
                manifest.write_text(json.dumps({"schema_reader": 7}))
                with self.assertRaisesRegex(RuntimeError, "WASM approval"):
                    compatibility.check(self.data, self.release)
                manifest.write_text(json.dumps({"schema_reader": 7, "pure_wasm_workbench": 1}))
                compatibility.check(self.data, self.release)
                with connect(self.data / "dsh.db") as db:
                    db.execute(f"DROP TABLE {table}")

    def test_empty_conversation_images_allow_unused_upgrade_rollback(self):
        with connect(self.data / "dsh.db") as db:
            db.execute("CREATE TABLE conversation_images(run_id TEXT)")
        compatibility.check(self.data, self.release)

    def test_invalidated_conversation_image_still_requires_provenance_reader(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE conversation_images(run_id TEXT,status TEXT,image BLOB,description TEXT); INSERT INTO conversation_images VALUES('synthetic','invalidated',NULL,NULL);")
        manifest = self.release / "deploy/release-capabilities.json"
        manifest.write_text(json.dumps({"schema_reader": 8}))
        with self.assertRaisesRegex(RuntimeError, "[Cc]onversation image"):
            compatibility.check(self.data, self.release)
        manifest.write_text(json.dumps({"schema_reader": 8, "conversation_images": 1}))
        compatibility.check(self.data, self.release)

    def test_untouched_skill_seeds_and_empty_snapshots_allow_rollback(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE skills(id TEXT,revision INTEGER,builtin INTEGER); CREATE TABLE run_skills(run_id TEXT,snapshot_json TEXT); INSERT INTO skills VALUES('drawing',1,1),('skill-creator',1,1);")
        compatibility.check(self.data, self.release)

    def test_used_skill_snapshot_requires_capable_release_even_after_skill_deletion(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE run_skills(run_id TEXT,snapshot_json TEXT); INSERT INTO run_skills VALUES('synthetic','{}');")
        with self.assertRaisesRegex(RuntimeError, "Skill snapshots"):
            compatibility.check(self.data, self.release)
        (self.release / "deploy/release-capabilities.json").write_text(json.dumps({"skills": 1}))
        compatibility.check(self.data, self.release)

    def test_custom_or_modified_builtin_skill_requires_capable_release(self):
        with connect(self.data / "dsh.db") as db:
            db.execute("CREATE TABLE skills(id TEXT,revision INTEGER,builtin INTEGER)")
        for revision, builtin in ((1, 0), (2, 1)):
            with self.subTest(revision=revision, builtin=builtin):
                with connect(self.data / "dsh.db") as db:
                    db.execute("INSERT INTO skills VALUES('synthetic',?,?)", (revision, builtin))
                with self.assertRaisesRegex(RuntimeError, "Skill snapshots"):
                    compatibility.check(self.data, self.release)
                manifest = self.release / "deploy/release-capabilities.json"
                manifest.write_text(json.dumps({"skills": 1}))
                compatibility.check(self.data, self.release)
                manifest.unlink()
                with connect(self.data / "dsh.db") as db:
                    db.execute("DELETE FROM skills")

    def test_native_image_config_in_revision_or_run_requires_capable_release(self):
        with connect(self.data / "dsh.db") as db:
            db.execute("CREATE TABLE config_revisions(revision INTEGER,config_json TEXT)")
            db.execute("ALTER TABLE runs ADD COLUMN config_json TEXT")
        for table in ("config_revisions", "runs"):
            with self.subTest(table=table):
                for value in ({"model": {}}, {"model": {"image_generation": False}}):
                    with connect(self.data / "dsh.db") as db:
                        db.execute(f"INSERT INTO {table}(config_json) VALUES(?)", (json.dumps(value),))
                    compatibility.check(self.data, self.release)
                with connect(self.data / "dsh.db") as db:
                    db.execute(f"INSERT INTO {table}(config_json) VALUES(?)", (json.dumps({"model": {"image_generation": True}}),))
                with self.assertRaisesRegex(RuntimeError, "image generation configuration"):
                    compatibility.check(self.data, self.release)
                manifest = self.release / "deploy/release-capabilities.json"
                manifest.write_text(json.dumps({"skills": 1}))
                compatibility.check(self.data, self.release)
                manifest.unlink()
                with connect(self.data / "dsh.db") as db:
                    db.execute(f"DELETE FROM {table}")


    def test_empty_trigger_tables_allow_pretrigger_rollback(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE trigger_jobs(id TEXT); CREATE TABLE trigger_occurrences(id TEXT); CREATE TABLE trigger_execution_grants(id TEXT); CREATE TABLE trigger_events(id TEXT); INSERT INTO trigger_events VALUES('bootstrap-only');")
        compatibility.check(self.data, self.release)

    def test_trigger_definitions_require_scheduler_aware_reader(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("CREATE TABLE trigger_jobs(id TEXT); INSERT INTO trigger_jobs VALUES('disabled-draft');")
        with self.assertRaisesRegex(RuntimeError, "task trigger"):
            compatibility.check(self.data, self.release)
        (self.release / "deploy/release-capabilities.json").write_text(json.dumps({"task_triggers": 1}))
        compatibility.check(self.data, self.release)

    def test_trigger_runs_cannot_downgrade_even_without_definition(self):
        with connect(self.data / "dsh.db") as db:
            db.executescript("ALTER TABLE runs ADD COLUMN run_kind TEXT; INSERT INTO runs(id,run_kind) VALUES('synthetic','trigger_model');")
        with self.assertRaisesRegex(RuntimeError, "isolated context"):
            compatibility.check(self.data, self.release)
        (self.release / "deploy/release-capabilities.json").write_text(json.dumps({"task_triggers": 1}))
        compatibility.check(self.data, self.release)


if __name__ == "__main__":
    unittest.main()
