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


if __name__ == "__main__":
    unittest.main()
