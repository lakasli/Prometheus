#!/usr/bin/env python3
"""One bounded real-model sleep selection in a disposable synthetic instance.

Only production's current model fields and referenced credential are read.
The external key stays in the one-request forwarder's memory. No production
history, personality, rest policy or channel account is copied or modified.
"""
import argparse
import copy
import hashlib
import http.server
import importlib.util
import json
import pathlib
import re
import sqlite3
import sys
import tempfile
import threading
import time
import uuid
from contextlib import closing

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    'rest_live_forwarder', pathlib.Path(__file__).with_name('conversation-state-live.py'))
live = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(live)
# Rest's existing host contract uses 1600, distinct from ordinary chat's 1024.
# The helper still enforces exact equality and a hard single-request cap.
live.MAX_OUTPUT_TOKENS = 1600
live.MAX_INPUT_BYTES = 22000
base, batch, API = live.base, live.batch, live.API


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--source-data-dir', required=True)
    parser.add_argument('--expected-commit', required=True)
    parser.add_argument('--report', required=True)
    args = parser.parse_args()
    if not re.fullmatch('[0-9a-f]{40}', args.expected_commit):
        parser.error('expected-commit must be a full release SHA')
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    source = pathlib.Path(args.source_data_dir).resolve(strict=True)
    output = pathlib.Path(args.report).resolve()
    if output.is_relative_to(source) or output.exists():
        parser.error('report must be a new file outside production data')
    report = dict(kind='one-request real-model sleep evidence selection', status='running',
                  started_at=base.utc_now(), checks=[], expected_commit=args.expected_commit,
                  binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                  script=live.script_source_metadata(script_path=__file__),
                  production_database_modified=False, production_history_read=False,
                  production_personality_read=False, real_qq_tested=False,
                  real_weixin_tested=False, request_limit=1, automatic_retries=0)
    forward = server = None
    stage = 'production model configuration read'

    def check(name, condition):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    try:
        with closing(sqlite3.connect((source / 'dsh.db').as_uri() + '?mode=ro', uri=True)) as db:
            db.execute('PRAGMA query_only=ON')
            model = json.loads(db.execute(
                "SELECT json_extract(c.config_json,'$.model') FROM config_revisions c "
                "JOIN settings s ON s.key='active_revision' AND c.revision=CAST(s.value AS INTEGER)"
            ).fetchone()[0])
        endpoint = live.coord.external_endpoint(model['base_url'])
        store = (source / 'secrets').resolve(strict=True)
        credential = (store / str(uuid.UUID(model['credential_ref']))).resolve(strict=True)
        if not credential.is_relative_to(store):
            raise ValueError('model credential path escaped its store')
        forward = live.ForwardState(endpoint, credential.read_text(encoding='utf-8'), 1)
        forward.case_id = 'REST_SINGLE_SOURCE'
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), live.Forwarder)
        server.daemon_threads = True
        server.state = forward
        threading.Thread(target=server.serve_forever, daemon=True).start()
        with tempfile.TemporaryDirectory(prefix='dsh-rest-real-') as temporary:
            h = base.Harness(str(binary), str(web), temporary, forward)
            forward.harness, forward.loopback_key = h, h.key
            try:
                h.start()
                h.bootstrap()
                check('temporary daemon matches the fixed published release', h.commit == args.expected_commit)
                h.configure(server.server_port)
                config = copy.deepcopy(h.applied)
                config['model']['model'] = model['model']
                config['model']['image_generation'] = False
                config['speech']['enabled'] = False
                config['assistant']['max_concurrent_runs'] = 1
                batch.apply_config(h, config)
                note_text = '明天准备整理合成资料，此事尚未完成。'
                conversation = h.conversation('Synthetic rest evidence')
                policy = dict(source_owner_id=h.owner, enabled=True, allow_model=True,
                              timezone='Asia/Shanghai')
                h.request('PUT', API + '/diary/policies', policy, headers={'If-Match': '0'})
                note = h.request('POST', API + '/diary/events',
                                 dict(content=note_text, source_conversation_id=conversation,
                                      happened_at=int(time.time()) - 86400, visibility='private'),
                                 expected=201)[0]

                def proof(payload):
                    if (payload.get('model') != model['model']
                            or set(payload) != {'model', 'messages', 'stream', 'max_tokens'}
                            or len(payload.get('messages', [])) != 2
                            or not payload['messages'][0]['content'].startswith('DSH_REST_EVIDENCE_SELECTOR_V1')):
                        raise AssertionError('only_the_authorized_rest_request_may_reach_provider')
                    body = json.loads(payload['messages'][1]['content'])
                    if body['sources'] != [{'source_id': note['id'], 'text': note_text}]:
                        raise AssertionError('only_the_single_synthetic_source_may_leave_host')
                    found = batch.rows(h,
                        'SELECT j.id,j.cycle_id,t.input_digest,a.reserved_tokens FROM consolidation_jobs j '
                        'JOIN rest_job_attempts a ON a.job_id=j.id AND a.fence=j.fence '
                        'JOIN rest_consolidation_targets t ON t.job_id=j.id '
                        "WHERE j.status='running' AND a.finished_at IS NULL")
                    digest = live.sha(live.canonical(payload['messages']))
                    if (len(found) != 1 or found[0]['input_digest'] != digest
                            or found[0]['reserved_tokens'] < len(live.canonical(payload)) + 1600):
                        raise AssertionError('actual_request_requires_one_frozen_leased_rest_job')
                    return dict(job_id=found[0]['id'], cycle_id=found[0]['cycle_id'],
                                input_digest=digest, synthetic_source_count=1,
                                canonical_full_request_sha256=live.sha(live.canonical(payload)),
                                reserved_tokens=found[0]['reserved_tokens'])

                forward.proof_lookup = proof
                caps = h.request('GET', API + '/task-triggers/capabilities')[0]
                h.request('POST', API + '/task-triggers/clock/acknowledge',
                          dict(observed_now=caps['now'], confirmed=True))
                policy_view = h.request('GET', API + '/rest/policy')[0]
                rest_policy = policy_view['policy']
                rest_policy.update(default_sleep_seconds=45, drain_seconds=0,
                                   consolidation_seconds=40, max_job_attempts=1,
                                   max_cycle_calls=1, max_cycle_reserved_tokens=24000)
                h.request('PUT', API + '/rest/policy', rest_policy,
                          headers={'If-Match': f'"{policy_view["revision"]}"',
                                   'Idempotency-Key': 'synthetic-one-request-rest-policy'})
                accepted = h.request('POST', API + '/rest/sleep', {},
                                     headers={'If-Match': '"0"', 'Idempotency-Key': 'synthetic-rest'})[0]
                cycle = accepted['state']['cycle_id']
                deadline = time.monotonic() + 38
                produced = []
                while time.monotonic() < deadline:
                    produced = batch.rows(h,
                        'SELECT e.id,e.revision,e.source_count,e.facts,e.generator FROM rest_produced_refs p '
                        'JOIN diary_entries e ON e.id=p.source_id AND e.revision=p.revision WHERE p.cycle_id=?',
                        (cycle,))
                    if produced or forward.failures:
                        break
                    time.sleep(.1)
                check('real model selection commits a canonical diary with complete source lineage',
                      len(produced) == 1 and produced[0]['source_count'] == 1
                      and note_text in produced[0]['facts']
                      and '不证明已完成或已送达' in produced[0]['facts']
                      and produced[0]['generator'].startswith('model:rest-quotes:'))
                check('exactly one bounded real HTTPS request has a frozen leased input proof',
                      forward.reserved == forward.forwarded == len(forward.records) == 1
                      and not forward.failures and forward.records[0]['http_status'] == 200)
                lease_deadline = time.monotonic() + 5
                while (batch.rows(h, 'SELECT 1 FROM rest_job_attempts WHERE finished_at IS NULL')
                       and time.monotonic() < lease_deadline):
                    time.sleep(.05)
                check('successful request releases its physical rest lease',
                      not batch.rows(h, 'SELECT 1 FROM rest_job_attempts WHERE finished_at IS NULL'))
                state = h.request('GET', API + '/rest')[0]['state']
                h.request('POST', API + '/rest/wake', {},
                          headers={'If-Match': f'"{state["generation"]}"', 'Idempotency-Key': 'cleanup-wake'})
                check('explicit wake remains independent of completed model work',
                      h.request('GET', API + '/rest')[0]['state']['phase'] == 'awake')
                report.update(status='passed', diary_revision=produced[0]['revision'],
                              diary_facts_sha256=hashlib.sha256(produced[0]['facts'].encode()).hexdigest())
            finally:
                h.close()
    except Exception as error:
        report.update(status='failed', failed_check=stage, error_type=type(error).__name__)
        print('FAIL one-request rest selection (' + type(error).__name__ + ')', flush=True)
    finally:
        if server is not None:
            server.shutdown()
            server.server_close()
        if forward is not None:
            report.update(requests_reserved=forward.reserved, requests_forwarded=forward.forwarded,
                          request_evidence=forward.records, failures=forward.failures)
            forward.key = None
        report.update(finished_at=base.utc_now(), passed=len(report['checks']))
        output.parent.mkdir(parents=True, exist_ok=True)
        with output.open('x', encoding='utf-8') as stream:
            json.dump(report, stream, ensure_ascii=False, indent=2)
            stream.write('\n')
    return 0 if report['status'] == 'passed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
