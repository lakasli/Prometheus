"""Read-only deployment gate for durable assistant feature semantics."""
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
        for table, capability in (("run_reply_plans", "structured_replies"), ("outbox_reply_parts", "structured_replies"), ("speech_assets", "local_piper_speech"), ("conversation_media_assets", "conversation_media")):
            if table in tables and db.execute(f"SELECT EXISTS(SELECT 1 FROM {table})").fetchone()[0] and int(capabilities.get(capability, 0)) < 1:
                raise RuntimeError(f"Target release lacks {capability} authorization or delivery semantics")
        if "qq_received_events" in tables and db.execute("SELECT EXISTS(SELECT 1 FROM qq_received_events WHERE status IN ('queued','processing'))").fetchone()[0] and int(capabilities.get("qq_durable_receive", 0)) < 1:
            raise RuntimeError("Target release lacks durable QQ reception of acknowledged events")
        # Completed batches still retain follower message IDs for deduplication.
        # Neither a settled Run nor an orphaned fragment makes an old reader safe.
        uses_qq_text_batches = any(
            table in tables and db.execute(f"SELECT EXISTS(SELECT 1 FROM {table})").fetchone()[0]
            for table in ("qq_text_batches", "qq_text_fragments")
        )
        if uses_qq_text_batches and int(capabilities.get("qq_text_batches", 0)) < 1:
            raise RuntimeError("Target release lacks QQ text batch ordering and follower message deduplication semantics")
        if "plugin_factory_versions" in tables and db.execute("SELECT EXISTS(SELECT 1 FROM plugin_factory_versions)").fetchone()[0] and int(capabilities.get("plugin_factory", 0)) < 1:
            raise RuntimeError("Target release lacks Component plugin approvals and active routes")
        for table in ("speech_assets", "conversation_media_assets"):
            if table in tables:
                columns = {r[1] for r in db.execute(f"PRAGMA table_info({table})")}
                if "source_run_id" in columns and db.execute(f"SELECT EXISTS(SELECT 1 FROM {table} WHERE source_run_id IS NOT NULL)").fetchone()[0] and int(capabilities.get("media_source_runs", 0)) < 1:
                    raise RuntimeError("Target release lacks media source-run cancellation semantics")
        uses_persona = False
        if "conversation_character_bindings" in tables:
            uses_persona = db.execute("SELECT EXISTS(SELECT 1 FROM conversation_character_bindings)").fetchone()[0]
        if "runs" in tables:
            columns = {r[1] for r in db.execute("PRAGMA table_info(runs)")}
            if "character_snapshot_json" in columns:
                uses_persona |= db.execute("SELECT EXISTS(SELECT 1 FROM runs WHERE character_snapshot_json IS NOT NULL)").fetchone()[0]
        if "settings" in tables:
            saved_personality = db.execute("SELECT EXISTS(SELECT 1 FROM settings WHERE key='assistant.personality.v1')").fetchone()[0]
            if saved_personality and int(capabilities.get("single_assistant_personality", 0)) < 1:
                raise RuntimeError("Target release lacks single assistant personality semantics; saved personality must not be ignored")
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
        uses_conversation_images = "conversation_images" in tables and db.execute("SELECT EXISTS(SELECT 1 FROM conversation_images)").fetchone()[0]
        if uses_conversation_images and int(capabilities.get("conversation_images", 0)) < 1:
            raise RuntimeError("Target release lacks conversation image provenance and invalidation support")
        uses_wasm = any(
            table in tables and db.execute(f"SELECT EXISTS(SELECT 1 FROM {table})").fetchone()[0]
            for table in ("wasm_workbench_jobs", "wasm_workbench_artifacts")
        )
        if uses_wasm and int(capabilities.get("pure_wasm_workbench", 0)) < 1:
            raise RuntimeError("Target release lacks pure WASM approval and revocation support")
        # Empty additive tables and the two untouched seeds permit rollback
        # before the first user action. Used snapshots and user management
        # changes must retain a reader that understands Skill semantics.
        uses_skills = "run_skills" in tables and db.execute("SELECT EXISTS(SELECT 1 FROM run_skills)").fetchone()[0]
        if "skills" in tables:
            columns = {r[1] for r in db.execute("PRAGMA table_info(skills)")}
            if {"revision", "builtin"} <= columns:
                uses_skills |= db.execute("SELECT EXISTS(SELECT 1 FROM skills WHERE revision>1 OR builtin=0)").fetchone()[0]
        for table in ("config_revisions", "runs"):
            if table not in tables:
                continue
            columns = {r[1] for r in db.execute(f"PRAGMA table_info({table})")}
            if "config_json" not in columns:
                continue
            for (text,) in db.execute(f"SELECT config_json FROM {table} WHERE config_json IS NOT NULL"):
                config = json.loads(text)
                if isinstance(config, dict):
                    if "reply" in config and int(capabilities.get("structured_replies", 0)) < 1:
                        raise RuntimeError("Target release cannot read structured reply configuration")
                    if "speech" in config and int(capabilities.get("local_piper_speech", 0)) < 1:
                        raise RuntimeError("Target release cannot read local speech configuration")
                model = config.get("model", {}) if isinstance(config, dict) else {}
                if isinstance(model, dict) and model.get("image_generation") is True:
                    uses_skills = True
                    break
        uses_triggers = any(
            table in tables and db.execute(f"SELECT EXISTS(SELECT 1 FROM {table})").fetchone()[0]
            for table in ("trigger_jobs", "trigger_occurrences", "trigger_execution_grants")
        )
        if "runs" in tables:
            columns = {r[1] for r in db.execute("PRAGMA table_info(runs)")}
            if "run_kind" in columns:
                uses_triggers |= db.execute("SELECT EXISTS(SELECT 1 FROM runs WHERE run_kind IN ('trigger_model','trigger_image'))").fetchone()[0]
        if uses_triggers and int(capabilities.get("task_triggers", 0)) < 1:
            raise RuntimeError("Target release lacks task trigger authorization, scheduling and isolated context support")
        if uses_skills and int(capabilities.get("skills", 0)) < 1:
            raise RuntimeError("Target release lacks Skill snapshots or image generation configuration support")


if __name__ == "__main__":
    try:
        check(*sys.argv[1:])
    except (RuntimeError, ValueError, sqlite3.Error, OSError) as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(2)
