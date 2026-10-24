"""Read-only deployment gate for durable persona and memory semantics."""
import json
from contextlib import closing
import pathlib
import sqlite3
import sys


def check(data_dir, release_source):
    manifest = pathlib.Path(release_source) / "deploy/release-capabilities.json"
    capabilities = json.loads(manifest.read_text()) if manifest.is_file() else {}
    database = pathlib.Path(data_dir) / "dsh.db"
    if not database.is_file():
        return
    with closing(sqlite3.connect(database.resolve().as_uri() + "?mode=ro", uri=True)) as db:
        tables = {r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        version = db.execute("SELECT coalesce(max(version),0) FROM dsh_schema_migrations").fetchone()[0] if "dsh_schema_migrations" in tables else 0
        # Version 1 is additive for legacy work. A future schema requires its reader.
        if version > max(1, int(capabilities.get("schema_reader", 0))):
            raise RuntimeError("Target release cannot read this database schema")
        uses_persona = False
        if "conversation_character_bindings" in tables:
            uses_persona = db.execute("SELECT EXISTS(SELECT 1 FROM conversation_character_bindings)").fetchone()[0]
        if "runs" in tables:
            columns = {r[1] for r in db.execute("PRAGMA table_info(runs)")}
            if "character_snapshot_json" in columns:
                uses_persona |= db.execute("SELECT EXISTS(SELECT 1 FROM runs WHERE character_snapshot_json IS NOT NULL)").fetchone()[0]
        if "settings" in tables:
            uses_persona |= db.execute("SELECT EXISTS(SELECT 1 FROM settings WHERE key='character.default.v1' AND value NOT IN ('null',''))").fetchone()[0]
        if uses_persona and int(capabilities.get("character_snapshot", 0)) < 1:
            raise RuntimeError("Target release lacks character snapshots; persona bindings/work must not be downgraded silently")
        uses_memory = False
        if "memory_records" in tables:
            # Bodyless tombstones still require a reader that respects forgetting.
            uses_memory = db.execute("SELECT EXISTS(SELECT 1 FROM memory_records)").fetchone()[0]
        if "memory_context_watermarks" in tables:
            uses_memory |= db.execute("SELECT EXISTS(SELECT 1 FROM memory_context_watermarks)").fetchone()[0]
        if uses_memory and (
            int(capabilities.get("assistant_memory", 0)) < 1
            or int(capabilities.get("memory_context", 0)) < 1
        ):
            raise RuntimeError("Target release lacks assistant memory/context invalidation; memory and forgetting must not be bypassed")
        uses_summary = False
        if "conversation_summary_versions" in tables:
            uses_summary = db.execute("SELECT EXISTS(SELECT 1 FROM conversation_summary_versions)").fetchone()[0]
        if uses_summary and int(capabilities.get("conversation_summary", 0)) < 1:
            raise RuntimeError("Target release lacks conversation summary invalidation")
        uses_weixin = any(
            table in tables and db.execute(f"SELECT EXISTS(SELECT 1 FROM {table})").fetchone()[0]
            for table in ("weixin_account", "weixin_run_replies", "weixin_outbox")
        )
        if uses_weixin and (
            int(capabilities.get("weixin_ilink", 0)) < 1
            or int(capabilities.get("weixin_private_context", 0)) < 1
        ):
            raise RuntimeError("Target release lacks iLink account or private reply context support")
        uses_candidates = any(
            table in tables and db.execute(f"SELECT EXISTS(SELECT 1 FROM {table})").fetchone()[0]
            for table in ("memory_candidate_jobs", "memory_candidates")
        )
        if uses_candidates and int(capabilities.get("memory_candidates", 0)) < 1:
            raise RuntimeError("Target release lacks memory candidate review and invalidation support")
        uses_images = "image_analysis_jobs" in tables and db.execute("SELECT EXISTS(SELECT 1 FROM image_analysis_jobs)").fetchone()[0]
        if uses_images and int(capabilities.get("standalone_image_analysis", 0)) < 1:
            raise RuntimeError("Target release lacks standalone image analysis ownership and erasure support")
        uses_wasm = any(
            table in tables and db.execute(f"SELECT EXISTS(SELECT 1 FROM {table})").fetchone()[0]
            for table in ("wasm_workbench_jobs", "wasm_workbench_artifacts")
        )
        if uses_wasm and int(capabilities.get("pure_wasm_workbench", 0)) < 1:
            raise RuntimeError("Target release lacks pure WASM approval and revocation support")


if __name__ == "__main__":
    try:
        check(*sys.argv[1:])
    except (RuntimeError, ValueError, sqlite3.Error, OSError) as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(2)
