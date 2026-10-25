"""Exercise downgrade gates against isolated SQLite feature fixtures."""
import importlib.util
import json
import pathlib
import sqlite3
import tempfile
import unittest
from contextlib import contextmanager


spec = importlib.util.spec_from_file_location(
    "compatibility", pathlib.Path(__file__).resolve().parents[2] / "deploy/check-compatibility.py"
)
compatibility = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compatibility)


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


if __name__ == "__main__":
    unittest.main()
