"""Read-only deployment gate for durable assistant feature semantics."""
import argparse
import ast
import json
from contextlib import closing
import pathlib
import sqlite3
import subprocess
import sys
import tempfile


REST_COLUMNS = {
    "rest_meta": "id schema_version",
    "rest_policies": "assistant_id revision policy_json actor_ref updated_at",
    "assistant_rest_states": "assistant_id generation revision phase cycle_id wake_at drain_at consolidate_at logical_at clock_epoch mono_ms reason",
    "rest_control_grants": "assistant_id scope_ref owner_id conversation_id speaker_ref revision can_sleep can_wake valid_until actor_ref updated_at",
    "rest_operations": "id assistant_id actor_ref idempotency_key request_digest result_json created_at source_run_id",
    "rest_cycles": "id assistant_id generation trigger_kind trigger_ref source_run_id policy_revision policy_json wake_at drain_at consolidate_at run_cutoff event_cutoff entry_cutoff person_cutoff deletion_epoch calls reserved_tokens created_at finished_at partial error_code",
    "rest_cycle_sources": "cycle_id owner_id policy_revision allow_model policy_digest revocation_epoch revocation_watermark",
    "rest_cycle_refs": "cycle_id source_kind source_id source_json",
    "rest_produced_refs": "cycle_id source_kind source_id revision owner_id source_json job_id attempt_id job_fence stage parent_digest parent_refs_json",
    "rest_steps": "cycle_id ordinal kind status checkpoint_json result_digest error_code updated_at",
    "rest_ack_permits": "id cycle_id generation source_run_id scope_ref grant_revision kind payload_digest expires_at used_at",
    "consolidation_jobs": "id assistant_id cycle_id owner_id kind period_start period_end target_revision status fence attempts snapshot_digest uses_model checkpoint_json result_digest error_code",
    "rest_job_sources": "job_id source_kind source_id source_json",
    "rest_job_attempts": "id job_id cycle_id fence reserved_tokens started_at finished_at outcome",
    "rest_consolidation_profiles": "cycle_id config_revision config_digest provider model credential_ref",
    "rest_consolidation_targets": "job_id target_json input_digest",
    "rest_consolidation_blocks": "cycle_id kind scope_digest reason",
    "rest_management_requests": "assistant_id actor_id operation request_key request_digest result_json created_at",
    "rest_control_candidates": "run_id input_digest received_at status operation_id error_code finished_at",
    "rest_schedule_receipts": "policy_revision nominal_at status operation_id",
    "rest_deferred_presentations": "run_id raw_generation plan_json media_inflight updated_at self_plan_status self_plan_json self_plan_hash",
}

REST_DEFAULT_POLICY = {
    "natural_enabled": False, "schedule_enabled": False,
    "time_zone": "Asia/Shanghai", "sleep_local": None, "wake_local": None,
    "default_sleep_seconds": None, "max_sleep_seconds": 86400,
    "drain_seconds": 30, "consolidation_seconds": 300, "queue_limit": 128,
    "max_job_attempts": 3, "max_cycle_calls": 8,
    "max_cycle_reserved_tokens": 24000, "ack_seconds": 30,
}

PLAN_CACHE_COLUMNS = {"self_plan_status", "self_plan_json", "self_plan_hash"}


def _unique_rest_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("Duplicate Rest policy field")
        result[key] = value
    return result


def check_rest(db, tables, reader):
    """An unused complete additive schema is reversible; durable decisions are not."""
    present = set(REST_COLUMNS) & tables
    named = {name for name in tables if name.startswith("rest_")}
    if not present and not named:
        return
    if present != set(REST_COLUMNS) or named - set(REST_COLUMNS):
        raise RuntimeError("Rest schema is incomplete or unknown; refusing to ignore partial rest state")
    for table, expected in REST_COLUMNS.items():
        columns = {row[1] for row in db.execute(f"PRAGMA table_info({table})")}
        base = set(expected.split())
        known_legacy_cache = table == "rest_deferred_presentations" and columns == base - PLAN_CACHE_COLUMNS
        if columns != base and not known_legacy_cache:
            raise RuntimeError("Rest schema columns are incomplete or unknown; a compatible reader is required")
    used = {table for table in REST_COLUMNS
            if db.execute(f"SELECT EXISTS(SELECT 1 FROM {table})").fetchone()[0]}
    if not used:
        return
    meta = db.execute("SELECT id,schema_version FROM rest_meta").fetchall()
    if len(meta) != 1 or meta[0][0] != 1 or type(meta[0][1]) is not int or meta[0][1] < 1:
        raise RuntimeError("Rest schema metadata is incomplete or invalid")
    version = meta[0][1]
    if version > max(1, reader):
        raise RuntimeError("Target release cannot read this Rest schema version")
    policies = db.execute("SELECT assistant_id,revision,policy_json,actor_ref FROM rest_policies").fetchall()
    states = db.execute("SELECT assistant_id,generation,revision,phase,cycle_id,wake_at,drain_at,consolidate_at,reason FROM assistant_rest_states").fetchall()
    if len(policies) != 1 or len(states) != 1 or not policies[0][0] or policies[0][0] != states[0][0]:
        raise RuntimeError("Rest initial state is incomplete; refusing semantic downgrade")
    try:
        policy = json.loads(policies[0][2], object_pairs_hook=_unique_rest_object)
    except (ValueError, TypeError) as error:
        raise RuntimeError("Rest policy is invalid; refusing semantic downgrade") from error
    routine_fields = {"anticipation_enabled", "reset_context_on_sleep"}
    if (not isinstance(policy, dict) or set(policy) - routine_fields != set(REST_DEFAULT_POLICY)
            or any(type(policy[key]) is not bool for key in routine_fields if key in policy)
            or any(type(policy[key]) is not type(value) for key, value in REST_DEFAULT_POLICY.items()
                   if value is not None)
            or any(policy[key] is not None and type(policy[key]) is not expected_type
                   for key, expected_type in (("sleep_local", str), ("wake_local", str),
                                              ("default_sleep_seconds", int)))):
        raise RuntimeError("Rest policy schema is incomplete or invalid")
    if set(policy) & routine_fields and reader < 2:
        raise RuntimeError("Target release lacks Rest routine policy reader")
    # Clock samples and logical timestamps alone do not turn a default-off
    # instance into used Rest. Every semantic auxiliary row does, even erased,
    # revoked, unknown, completed, or an unclaimed raw presentation cache.
    auxiliary = used - {"rest_meta", "rest_policies", "assistant_rest_states"}
    untouched_policy = (policies[0][1] == 1 and policies[0][3] == "initial_disabled"
                        and policy == REST_DEFAULT_POLICY)
    untouched_state = states[0][1:] == (0, 1, "awake", None, None, None, None, None)
    if reader < version and (auxiliary or not untouched_policy or not untouched_state):
        raise RuntimeError("Target release lacks Rest durable control, authorization, source, wake or delivery semantics")


def check(data_dir, release_source):
    manifest = pathlib.Path(release_source) / "deploy/release-capabilities.json"
    capabilities = json.loads(manifest.read_text()) if manifest.is_file() else {}
    database = pathlib.Path(data_dir) / "dsh.db"
    if not database.is_file():
        return
    with closing(sqlite3.connect(database.resolve().as_uri() + "?mode=ro", uri=True)) as db:
        tables = {r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        if "rest_deferred_presentations" in tables:
            columns = {r[1] for r in db.execute("PRAGMA table_info(rest_deferred_presentations)")}
            if "self_plan_status" in columns and db.execute("SELECT EXISTS(SELECT 1 FROM rest_deferred_presentations WHERE self_plan_status IS NOT NULL)").fetchone()[0] and int(capabilities.get("daily_life", 0)) < 2:
                raise RuntimeError("Target release cannot read generic personal plan checkpoints")
        if "daily_life_events" in tables:
            used = db.execute("SELECT EXISTS(SELECT 1 FROM daily_life_events)").fetchone()[0]
            used = used or ("sleep_context_boundaries" in tables and db.execute("SELECT EXISTS(SELECT 1 FROM sleep_context_boundaries)").fetchone()[0])
            policy_row = db.execute("SELECT value FROM settings WHERE key='schedule.personal.v1'").fetchone()
            used = used or bool(policy_row)
            if "rest_policies" in tables:
                row = db.execute("SELECT policy_json FROM rest_policies LIMIT 1").fetchone()
                policy = json.loads(row[0]) if row else {}
                used = used or policy.get("anticipation_enabled") or policy.get("reset_context_on_sleep")
            if used and int(capabilities.get("daily_life", 0)) < 1:
                raise RuntimeError("Target release lacks daily routine decisions and sleep context boundaries")
            generic = bool(policy_row and json.loads(policy_row[0]).get("format") == "text_v1")
            generic = generic or ("daily_life_texts" in tables and db.execute("SELECT EXISTS(SELECT 1 FROM daily_life_texts)").fetchone()[0])
            if "rest_deferred_presentations" in tables:
                columns = {r[1] for r in db.execute("PRAGMA table_info(rest_deferred_presentations)")}
                generic = generic or ("self_plan_status" in columns and db.execute("SELECT EXISTS(SELECT 1 FROM rest_deferred_presentations WHERE self_plan_status IS NOT NULL)").fetchone()[0])
            if generic and int(capabilities.get("daily_life", 0)) < 2:
                raise RuntimeError("Target release cannot execute generic personal text activities")
        check_rest(db, tables, int(capabilities.get("rest", 0)))
        version = db.execute("SELECT coalesce(max(version),0) FROM dsh_schema_migrations").fetchone()[0] if "dsh_schema_migrations" in tables else 0
        # Version 1 is additive for legacy work. A future schema requires its reader.
        if version > max(1, int(capabilities.get("schema_reader", 0))):
            raise RuntimeError("Target release cannot read this database schema")
        # v2 readers erase partial hierarchy reductions together with sources.
        # A v1 program cannot safely maintain those derived private fragments.
        if "diary_meta" in tables:
            diary_version = db.execute("SELECT coalesce(max(schema_version),0) FROM diary_meta").fetchone()[0]
            if diary_version > int(capabilities.get("diary", 0)):
                raise RuntimeError("Target release cannot read this diary schema version or erase hierarchy fragments")
        # A bodyless tombstone, revoked identity, or a completed occurrence is
        # still durable state. Older readers must not bypass its semantics.
        for feature, feature_tables in (
            ("diary", ("diary_events", "diary_entries", "diary_source_policies", "diary_source_revocations", "diary_model_leases")),
            ("relationships", ("relationship_people", "relationship_assertions", "relationship_edges", "relationship_source_tombstones", "relationship_model_jobs", "relationship_identity_group_members")),
            ("schedule", ("schedule_activities", "schedule_occurrences", "schedule_autonomy_grants", "schedule_model_budgets", "schedule_model_leases", "schedule_activity_budgets", "schedule_resources", "schedule_reading_progress", "schedule_admin_approvals", "schedule_planning_attempts")),
        ):
            used = any(table in tables and db.execute(f"SELECT EXISTS(SELECT 1 FROM {table})").fetchone()[0]
                       for table in feature_tables)
            if used and int(capabilities.get(feature, 0)) < 1:
                raise RuntimeError(f"Target release lacks {feature} source, revision and deletion semantics")
        if "relationship_affinity_policies" in tables and int(capabilities.get("relationships", 0)) < 1:
            if db.execute("SELECT EXISTS(SELECT 1 FROM relationship_affinity_policies WHERE revision>1)").fetchone()[0]:
                raise RuntimeError("Target release lacks relationships versioned affinity policy semantics")
        for feature, required, feature_tables in (
            ("conversation_identity", 2, ("run_conversation_bindings", "conversation_message_identities", "run_conversation_contexts")),
            ("delivery_context", 1, ("delivery_context_snapshots", "conversation_summary_delivery_sources", "conversation_summary_delivery_versions", "diary_capture_delivery_state", "diary_capture_dirty")),
            ("turn_control", 1, ("conversation_inputs", "run_publication_decisions", "conversation_corrections")),
            # Empty additive tables permit an unused upgrade to roll back.
            # Any retained row matters, including erased entities, source
            # tombstones, usage proofs, epochs and idempotency results.
            ("conversation_state", 1, ("conversation_state_heads", "conversation_state_entities", "conversation_state_sources", "conversation_state_observations", "conversation_state_uses", "conversation_state_snapshots", "conversation_state_source_tombstones")),
            ("commitments", 1, ("conversation_commitments", "conversation_commitment_epochs", "conversation_commitment_events", "conversation_commitment_usage", "conversation_commitment_contexts", "conversation_commitment_sources", "conversation_commitment_idempotency")),
            # A saved policy already constrains the worker even before the
            # first compiled Prompt; an old worker must not ignore it.
            ("context_budget", 1, ("context_budget_policy_versions", "context_budget_policy_current", "context_budget_snapshots")),
        ):
            used = any(table in tables and db.execute(f"SELECT EXISTS(SELECT 1 FROM {table})").fetchone()[0]
                       for table in feature_tables)
            if used and int(capabilities.get(feature, 0)) < required:
                raise RuntimeError(f"Target release lacks {feature} durable source, context or execution semantics")
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


def check_previous(data_dir, release_source, previous_gate):
    """Both gates must pass; strip only known additive columns in a private copy.

    The canonical gate checks the untouched database first, including reader2
    semantics. A legacy gate with exactly our base Rest column contract then
    checks all its original policies against the projected copy. Unknown or
    partial columns never enter this path, and production data is never edited.
    """
    check(data_dir, release_source)
    previous_gate = pathlib.Path(previous_gate).resolve(strict=True)
    declarations = {}
    for node in ast.parse(previous_gate.read_text(encoding="utf-8")).body:
        if isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id == "REST_COLUMNS" for t in node.targets):
            declarations = ast.literal_eval(node.value)
    database = pathlib.Path(data_dir) / "dsh.db"
    project = False
    legacy_contract = {**REST_COLUMNS, "rest_deferred_presentations": "run_id raw_generation plan_json media_inflight updated_at"}
    if declarations == legacy_contract and database.is_file():
        with closing(sqlite3.connect(database.resolve().as_uri() + "?mode=ro", uri=True)) as db:
            columns = {row[1] for row in db.execute("PRAGMA table_info(rest_deferred_presentations)")}
        project = columns == set(REST_COLUMNS["rest_deferred_presentations"].split())

    def invoke(directory):
        result = subprocess.run([sys.executable, str(previous_gate), str(directory), str(release_source)], capture_output=True, text=True, timeout=30)
        if result.returncode:
            raise RuntimeError("Previous release compatibility check rejected the target")

    if not project:
        invoke(data_dir)
        return
    with tempfile.TemporaryDirectory(prefix="dsh-legacy-gate-") as folder:
        projected = pathlib.Path(folder) / "dsh.db"
        with closing(sqlite3.connect(database.resolve().as_uri() + "?mode=ro", uri=True)) as original, closing(sqlite3.connect(projected)) as copy:
            original.backup(copy)
            projected.chmod(0o600)
            for column in sorted(PLAN_CACHE_COLUMNS):
                copy.execute(f"ALTER TABLE rest_deferred_presentations DROP COLUMN {column}")
            copy.commit()
        invoke(folder)
    print("Previous gate passed against the known additive-column projection; production database unchanged")


if __name__ == "__main__":
    try:
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument("data_dir")
        parser.add_argument("release_source")
        parser.add_argument("--previous-gate")
        args = parser.parse_args()
        if args.previous_gate:
            check_previous(args.data_dir, args.release_source, args.previous_gate)
        else:
            check(args.data_dir, args.release_source)
    except (RuntimeError, ValueError, sqlite3.Error, OSError, SyntaxError, subprocess.TimeoutExpired) as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(2)
