#!/usr/bin/env python3
"""Isolated people/relationships acceptance; optional real model uses synthetic data.

--live-config-from copies only the authorized model configuration and credential
into a temporary instance. Production diary, people, channels and user data are
never copied or changed. Reports contain metadata, not prompts or credentials.
"""
import argparse
import hashlib
import http.server
import importlib.util
import json
import pathlib
import platform
import tempfile
import threading
import time
import uuid

spec = importlib.util.spec_from_file_location(
    "diary_smoke", pathlib.Path(__file__).with_name("diary-smoke.py"))
diary = importlib.util.module_from_spec(spec)
spec.loader.exec_module(diary)
memory = diary.memory
PREFIX = memory.PREFIX


class Provider(memory.Provider):
    def do_POST(self):
        size = int(self.headers.get("Content-Length", "0"))
        if not 0 < size <= 1024 * 1024:
            self.send_error(413)
            return
        payload = json.loads(self.rfile.read(size))
        with self.server.mock_state.condition:
            self.server.mock_state.requests.append(payload)
        system = payload["messages"][0]["content"]
        if system.startswith("为已核验人物整理带来源的待审核候选。"):
            content = json.dumps({"assertions": [{
                "field": "hobby", "value": "围棋", "statement_kind": "self_report",
                "reason": "合成甲明确说自己的爱好是围棋。"}], "edges": []}, ensure_ascii=False)
        elif system.startswith(("DSH_DIARY_GENERATOR_V2", "你为刘嘉卉整理有来源的日记。")):
            content = json.dumps({"facts": "收到一条合成资料。", "feelings": ""}, ensure_ascii=False)
        else:
            content = "已收到这条合成测试消息。"
        raw = json.dumps({"choices": [{"message": {"content": content}}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        try:
            self.wfile.write(raw)
        except (BrokenPipeError, ConnectionResetError):
            pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--web-dir", required=True)
    parser.add_argument("--report")
    parser.add_argument("--live-config-from")
    args = parser.parse_args()
    binary = str(pathlib.Path(args.binary).resolve(strict=True))
    web_dir = str(pathlib.Path(args.web_dir).resolve(strict=True))
    report = {"kind": "isolated-relationships-process-api", "status": "running",
              "started_at": memory.utc_now(), "platform": platform.system(),
              "architecture": platform.machine(),
              "binary_sha256": hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest(),
              "external_model_requested": bool(args.live_config_from),
              "external_model_passed": False, "production_data_modified": False,
              "real_channel_tested": False, "checks": []}
    mock = memory.MockState()
    provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    provider.daemon_threads = True
    provider.mock_state = mock
    threading.Thread(target=provider.serve_forever, daemon=True).start()

    def check(name, condition):
        if not condition:
            raise AssertionError(name)
        report["checks"].append(name)
        print("PASS " + name, flush=True)

    def evidence():
        return {"source_type": "manual", "source_id": None, "source_revision": None,
                "statement_kind": "self_report", "reason": "合成验收人物本人明确陈述"}

    failed = False
    try:
        with tempfile.TemporaryDirectory(prefix="dsh-relationships-acceptance-") as temporary:
            h = memory.Harness(binary, web_dir, temporary, mock)
            try:
                h.start()
                report["commit"] = h.commit
                for path in ["/people", "/edges", "/capabilities", "/affinity-policy", "/model-jobs"]:
                    h.request("GET", PREFIX + "/relationships" + path, expected=401)
                check("all people, edges, model jobs and policy reads require authentication", True)
                h.bootstrap()
                people = PREFIX + "/relationships/people"
                h.request("POST", people, {}, expected=403, with_csrf=False)
                h.request("POST", people, {}, expected=403, headers={"Origin": "https://invalid.example"})
                check("relationship mutations enforce CSRF and exact origin", True)
                conversation = h.conversation("Synthetic relationship owner A")
                conversation_b = h.conversation("Synthetic relationship owner B")
                # Only this disposable database gains a second synthetic source owner.
                with h.db() as db:
                    db.execute("UPDATE conversations SET owner_id='qq:synthetic:c2c:other' WHERE id=?", (conversation_b,))

                def create(name, source=conversation):
                    return h.request("POST", people, {"display_name": name, "visibility": "private",
                        "source_conversation_id": source, "publish": False,
                        "reason": "合成验收建立独立人物"}, expected=201)[0]

                a, b = create("合成甲"), create("合成甲", conversation_b)
                check("same display names retain separate stable person IDs and source owners",
                      a["id"] != b["id"] and a["source_owner_id"] != b["source_owner_id"])
                pa, pb = people + "/" + a["id"], people + "/" + b["id"]
                claim, _, _ = h.request("POST", pa + "/assertions", {
                    "field": "job", "value": "合成工程师", "evidence": evidence(),
                    "valid_from": None, "valid_until": None}, expected=201,
                    headers={"If-Match": str(a["revision"])})
                check("new profile assertions remain pending with explicit provenance", claim["status"] == "pending")
                h.request("POST", PREFIX + "/relationships/assertions/" + claim["id"] + "/review", {
                    "decision": "approve", "supersedes": [], "reason": "仅在合成账户中核验此断言"},
                    headers={"If-Match": str(claim["revision"])})
                current, _, _ = h.request("GET", pa)
                h.request("PATCH", pa, {"display_name": "stale", "reason": "合成过期写入"},
                          headers={"If-Match": "1"}, expected=409)
                check("approved profiles keep source revisions and reject stale person changes",
                      any(v["status"] == "approved" for v in current["assertions"]))

                h.request("POST", pa + "/identities", {"conversation_id": conversation,
                    "alias": "合成QQ昵称", "confirmed": True, "reason": "合成账户本人核验"},
                    headers={"If-Match": str(current["revision"])})
                current, _, _ = h.request("GET", pa)
                other, _, _ = h.request("GET", pb)
                group, _, _ = h.request("POST", pa + "/identity-group", {
                    "other_person_id": b["id"], "other_revision": other["revision"],
                    "confirmed": True, "reason": "合成双账号分别核验"},
                    headers={"If-Match": str(current["revision"])})
                check("cross-account identity groups map identity without moving source ownership",
                      len(group["members"]) == 2 and group["mapping_scope"] == "identity_only_no_acl_grant")
                current, _, _ = h.request("GET", pa)
                h.request("DELETE", pa + "/identity-group", {"confirmed": True,
                    "reason": "合成拆分恢复原身份来源"}, headers={"If-Match": str(current["revision"])})
                current, _, _ = h.request("GET", pa)
                check("cross-account mapping is explicitly reversible", current["identity_group"] is None)
                c = create("合成乙")
                edge, _, _ = h.request("POST", PREFIX + "/relationships/edges", {
                    "from_person_id": a["id"], "to_person_id": c["id"],
                    "relationship_type": "friend", "evidence": evidence()}, expected=201)
                check("directed relationship proposals remain pending", edge["status"] == "pending")
                h.request("POST", PREFIX + "/relationships/edges/" + edge["id"] + "/review", {
                    "decision": "approve", "supersedes": [], "reason": "仅合成账户人工核验关系"},
                    headers={"If-Match": str(edge["revision"])})
                graph, _, _ = h.request("GET", PREFIX + "/relationships/edges")
                check("relationship approval does not invent reciprocal edges",
                      len(graph["items"]) == 1 and graph["items"][0]["from_person_id"] == a["id"])
                check("contact authorization is separate and delivery remains blocked",
                      current["contact_permission"]["status"] == "not_granted" and
                      not current["contact_permission"]["delivery_available"])
                h.request("PUT", pa + "/contact-permission", {"status": "granted",
                    "conversation_id": conversation, "expires_at": None,
                    "evidence": "管理员不能代替接收人"}, expected=400, headers={"If-Match": "0"})

                policy, _, _ = h.request("GET", PREFIX + "/relationships/affinity-policy")
                policy_input = {k: policy[k] for k in ["initial", "minimum", "maximum", "daily_decay", "grace_seconds", "downtime_cap_seconds"]}
                policy_input.update(confirmed=True, reason="合成策略预演")
                preview, _, _ = h.request("POST", PREFIX + "/relationships/affinity-policy/preview", {
                    "policy": policy_input, "idle_seconds": 10 * 86400})
                policy_after, _, _ = h.request("GET", PREFIX + "/relationships/affinity-policy")
                check("affinity policy preview does not mutate policy or contact permission",
                      preview["preview_only"] and policy["revision"] == policy_after["revision"])
                if args.live_config_from:
                    diary.configure_live(h, args.live_config_from)
                else:
                    h.configure(provider.server_port)
                h.request("PUT", PREFIX + "/diary/policies", {"source_owner_id": h.owner,
                    "enabled": True, "allow_model": True, "timezone": "Asia/Shanghai"},
                    headers={"If-Match": "0"})
                note, _, _ = h.request("POST", PREFIX + "/diary/events", {
                    "content": "本条仅用于合成验收。合成甲明确说：我的爱好是围棋。",
                    "source_conversation_id": conversation, "visibility": "private"}, expected=201)
                current, _, _ = h.request("GET", pa)
                h.request("POST", PREFIX + "/task-triggers/clock/acknowledge", {
                    "observed_now": int(time.time()), "confirmed": True})
                job, _, _ = h.request("POST", PREFIX + "/relationships/model-jobs", {
                    "person_id": a["id"], "source_type": "diary_event", "source_id": note["id"],
                    "source_revision": note["revision"], "target_person_ids": [],
                    "confirmed": True, "idempotency_key": str(uuid.uuid4())}, expected=202,
                    headers={"If-Match": str(current["revision"])})
                deadline = time.monotonic() + 75
                while time.monotonic() < deadline:
                    job, _, _ = h.request("GET", PREFIX + "/relationships/model-jobs/" + job["id"])
                    if job["status"] in ("completed", "failed", "cancelled"):
                        break
                    time.sleep(.2)
                check("authorized selected-source model job completes with pending profile candidates",
                      job["status"] == "completed" and bool(job["result"].get("assertion_ids")))
                current, _, _ = h.request("GET", pa)
                generated = [v for v in current["assertions"] if v["source_id"] == note["id"]]
                check("model candidates retain selected source and never approve themselves",
                      generated and all(v["status"] == "pending" and v["source_revision"] == note["revision"] for v in generated))
                report["external_model_passed"] = bool(args.live_config_from)
                h.request("DELETE", PREFIX + "/diary/events/" + note["id"],
                          headers={"If-Match": str(note["revision"])})
                current, _, _ = h.request("GET", pa)
                check("source forgetting removes all dependent model-generated assertions",
                      not any(v["source_id"] == note["id"] for v in current["assertions"]))
                h.stop()
                h.start()
                h.login()
                restored, _, _ = h.request("GET", pa)
                check("person identity, mapping split and source tombstone survive restart",
                      restored["id"] == a["id"] and restored["identity_group"] is None and
                      not any(v["source_id"] == note["id"] for v in restored["assertions"]))
                report["status"] = "passed"
            finally:
                h.close()
    except Exception as error:
        failed = True
        report["status"] = "failed"
        report["error_type"] = type(error).__name__
        if isinstance(error, AssertionError):
            report["failed_check"] = str(error)
        print("FAIL relationships acceptance (" + type(error).__name__ + ")", flush=True)
    finally:
        provider.shutdown()
        report["finished_at"] = memory.utc_now()
        if args.report:
            pathlib.Path(args.report).write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
