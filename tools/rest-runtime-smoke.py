#!/usr/bin/env python3
"""Real daemon rest/restart acceptance with signed synthetic QQ and loopback HTTP.

Uses only a new temporary data directory. The account pacing fence is installed
before enabling the synthetic QQ account. No real model or platform is called.
Linux proves recovery with a stable boot clock; Windows separately proves the
documented early wake when process clock continuity cannot be established.
"""
import argparse
import copy
import datetime
import hashlib
import http.server
import importlib.util
import json
import pathlib
import platform
import secrets
import sys
import tempfile
import threading
import time

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    'rest_coordination_harness', pathlib.Path(__file__).with_name('conversation-coordination-smoke.py'))
coord = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(coord)
base, batch = coord.base, coord.batch
API = base.PREFIX


def wait(check, description, timeout=25):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.05)
    raise TimeoutError(description)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report', required=True)
    parser.add_argument('--expected-commit')
    parser.add_argument('--openssl')
    args = parser.parse_args()
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    report = dict(kind='isolated rest runtime and original-deadline restart acceptance',
                  started_at=base.utc_now(), status='running', checks=[],
                  architecture=platform.machine(), platform=platform.system(),
                  binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                  production_data_accessed=False, real_external_model_tested=False,
                  real_qq_tested=False, qq_outbound_http_tested=False,
                  client_delivery_tested=False, synthetic_pacing_fence=True)
    stage = 'setup'

    def check(name, condition):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    mock = coord.ModelState()
    provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), coord.Provider)
    provider.daemon_threads = True
    provider.mock = mock
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-rest-acceptance-') as temporary:
            h = base.Harness(str(binary), str(web), temporary, mock)
            try:
                h.start()
                report['commit'] = h.commit
                check('daemon identity matches the expected release',
                      args.expected_commit is None or h.commit == args.expected_commit)
                for route in ['/rest', '/rest/policy', '/rest/cycles', '/rest/cycles/current']:
                    h.request('GET', API + route, expected=401, authenticated=False)
                check('rest state policy and history require authentication', True)
                h.bootstrap()

                def get(route='/rest'):
                    return h.request('GET', API + route)[0]

                def write(route, body, revision, key, expected=200):
                    return h.request('POST', API + route, body, expected=expected,
                                     headers={'If-Match': f'"{revision}"', 'Idempotency-Key': key})[0]

                initial = get()
                policy = get('/rest/policy')
                check('factory rest is awake with natural and scheduled sleep disabled',
                      initial['state']['phase'] == 'awake' and policy['revision'] == 1
                      and not policy['policy']['natural_enabled']
                      and not policy['policy']['schedule_enabled']
                      and policy['policy']['default_sleep_seconds'] is None)
                h.request('POST', API + '/rest/sleep', {}, expected=403, with_csrf=False)
                h.request('POST', API + '/rest/wake', {}, expected=403,
                          headers={'Origin': 'https://invalid.example'})
                h.request('POST', API + '/rest/sleep', {}, expected=428)
                check('controls enforce CSRF origin and exact version preconditions', True)
                h.configure(provider.server_port)
                caps = get('/task-triggers/capabilities')
                h.request('POST', API + '/task-triggers/clock/acknowledge',
                          {'confirmed': True, 'observed_now': caps['now']})
                check('only the isolated host clock is acknowledged',
                      get()['clock']['trusted_for_new_sleep'] is True)
                secret = secrets.token_urlsafe(32)
                signer = batch.Signer(batch.openssl_path(args.openssl), temporary, secret)
                credential = h.request('POST', API + '/secrets',
                                       {'kind': 'qq_app_secret', 'value': secret})[0]
                with h.db() as db:
                    db.execute('INSERT INTO reply_account_pacing(account_id,next_ms) VALUES(?,?)',
                               (batch.ACCOUNT, batch.PACING_FENCE_MS))
                config = copy.deepcopy(h.applied)
                config['qq'] = dict(enabled=True, app_id=batch.ACCOUNT,
                                    credential_ref=credential['id'], transport='webhook')
                config['speech']['enabled'] = False
                batch.apply_config(h, config)
                check('synthetic QQ is enabled only after durable outbound fencing', True)

                p = copy.deepcopy(policy['policy'])
                p.update(default_sleep_seconds=14, drain_seconds=0, consolidation_seconds=0)
                headers = {'If-Match': '"1"', 'Idempotency-Key': 'rest-policy-v2'}
                saved = h.request('PUT', API + '/rest/policy', p, headers=headers)[0]
                replay = h.request('PUT', API + '/rest/policy', p, headers=headers)[0]
                h.request('PUT', API + '/rest/policy', p, expected=409,
                          headers={'If-Match': '"1"', 'Idempotency-Key': 'stale-policy'})
                check('policy writes are versioned and same-key replays do not change revision',
                      saved['revision'] == replay['revision'] == 2 and replay['idempotent_replay'])

                conversation = h.conversation('Synthetic held rest request')
                question = '合成测试，只回复休息前的原始结果。'
                mock.hold_question = question
                held = h.submit(conversation, question)
                wait(mock.hold_received.is_set, 'held model request was not observed')
                frozen = coord.snapshot(h, held)
                sleep = write('/rest/sleep', {}, 0, 'manual-sleep')
                cycle, wake_at = sleep['state']['cycle_id'], sleep['state']['wake_at']
                check('sleep persists a bounded original deadline and retains the actual HTTP lease',
                      sleep['state']['generation'] == 1 and wake_at is not None
                      and len(batch.rows(h, 'SELECT 1 FROM native_worker_leases WHERE run_id=?', (held,))) == 1)
                replay = write('/rest/sleep', {}, 0, 'manual-sleep')
                check('sleep replay never extends the deadline',
                      replay['operation']['wake_at'] == wake_at and replay['idempotent_replay'])
                write('/rest/wake', {}, 0, 'stale-wake', expected=409)
                incoming = coord.receive(h, signer, 'rest_synthetic_a', '睡眠期间的合成入站。')
                check('signed QQ is accepted and durably queued while the instance rests',
                      batch.rows(h, 'SELECT status FROM runs WHERE id=?', (incoming,))[0]['status'] == 'queued')
                mock.hold_release.set()
                wait(lambda: batch.rows(h, 'SELECT 1 FROM rest_deferred_presentations WHERE run_id=?', (held,))
                     and not batch.rows(h, 'SELECT 1 FROM native_worker_leases WHERE run_id=?', (held,)),
                     'held generation did not settle into its durable cache')
                check('finished generation is cached without publication or a replacement request',
                      batch.rows(h, 'SELECT status FROM runs WHERE id=?', (held,))[0]['status'] == 'queued'
                      and len(mock.requests) == 1
                      and not batch.rows(h, 'SELECT 1 FROM outbox WHERE run_id IN (?,?)', (held, incoming))
                      and coord.snapshot(h, held) == frozen)
                h.request('GET', '/health/ready')
                check('management readiness and reception remain available during sleep',
                      get()['state']['phase'] in ('sleep_pending', 'consolidating', 'sleeping'))
                h.stop()
                h.start()
                h.login()
                after = get()
                history = get('/rest/cycles/' + cycle)
                check('process restart preserves the original cycle and wake deadline',
                      history['wake_at'] == wake_at and history['generation'] == 1)
                if platform.system() == 'Linux':
                    check('Linux boot-clock continuity keeps the original sleep after restart',
                          after['state']['phase'] != 'awake' and after['state']['wake_at'] == wake_at)
                else:
                    check('unverifiable process clock wakes early without extending the sleep',
                          after['state']['phase'] in ('waking', 'awake'))
                wait(lambda: get()['state']['phase'] == 'awake', 'original deadline did not wake the instance')
                held_result, incoming_result = h.wait_run(held), h.wait_run(incoming, via_api=False)
                check('queued inputs resume after wake and cached generation is used exactly once',
                      held_result['status'] == incoming_result['status'] == 'completed'
                      and len([r for r in mock.requests if mock.question(r) == question]) == 1
                      and coord.snapshot(h, held) == frozen
                      and not batch.rows(h, 'SELECT 1 FROM rest_deferred_presentations'))
                check('Linux automatic wake does not wait for a replacement full sleep interval',
                      platform.system() != 'Linux' or get()['state']['logical_at'] >= wake_at)

                # Direct control is authorized independently for this exact QQ
                # source. Imported/quoted requests and other senders cannot use it.
                # A manual clock acknowledgement is process scoped on Windows;
                # restart deliberately does not inherit it for a new sleep.
                caps = get('/task-triggers/capabilities')
                h.request('POST', API + '/task-triggers/clock/acknowledge',
                          {'confirmed': True, 'observed_now': caps['now']})
                grant_route = '/rest/control-grants/' + incoming
                grant = get(grant_route)
                permissions = dict(can_sleep=True, can_wake=True, valid_until=int(time.time()) + 3600)
                granted = h.request('PUT', API + grant_route, permissions,
                                    headers={'If-Match': f'"{grant["revision"]}"',
                                             'Idempotency-Key': 'source-rest-grant'})[0]
                current_policy = get('/rest/policy')
                p = current_policy['policy']
                p.update(natural_enabled=True, drain_seconds=2, default_sleep_seconds=30)
                h.request('PUT', API + '/rest/policy', p,
                          headers={'If-Match': f'"{current_policy["revision"]}"',
                                   'Idempotency-Key': 'enable-synthetic-control'})
                count = len(mock.requests)
                control = coord.receive(h, signer, 'rest_synthetic_a', '快去睡觉啦！')
                result = h.wait_run(control, via_api=False)
                control_state = get()['state']
                report['direct_control_diagnostic'] = {
                    'run_status': result['status'], 'run_error': result.get('error'),
                    'phase': control_state['phase'], 'generation': control_state['generation'],
                    'requests_before': count, 'requests_after': len(mock.requests),
                    'candidate': batch.rows(h, 'SELECT status,error_code FROM rest_control_candidates WHERE run_id=?', (control,)),
                    'clock': get()['clock'],
                }
                check('authorized direct sleep is handled by the host without a model request',
                      result['status'] == 'completed' and len(mock.requests) == count
                      and control_state['phase'] != 'awake'
                      and batch.rows(h, 'SELECT status FROM rest_control_candidates WHERE run_id=?', (control,))[0]['status'] == 'completed')
                local_wake = datetime.datetime.fromtimestamp(
                    control_state['wake_at'], datetime.timezone(datetime.timedelta(hours=8)))
                check('direct sleep confirmation states the exact host deadline and IANA timezone',
                      local_wake.strftime('%Y-%m-%d %H:%M:%S') in result['response']
                      and 'Asia/Shanghai' in result['response'])
                denied = coord.receive(h, signer, 'rest_synthetic_b', '醒醒')
                h.wait_run(denied, via_api=False)
                check('a different verified sender cannot wake the global instance',
                      get()['state']['phase'] != 'awake'
                      and batch.rows(h, 'SELECT status FROM rest_control_candidates WHERE run_id=?', (denied,))[0]['status'] == 'rejected')
                permissions.update(can_sleep=False, can_wake=False)
                h.request('PUT', API + grant_route, permissions,
                          headers={'If-Match': f'"{granted["revision"]}"',
                                   'Idempotency-Key': 'revoke-source-control'})
                revoked = coord.receive(h, signer, 'rest_synthetic_a', '醒醒')
                h.wait_run(revoked, via_api=False)
                check('source control revocation is effective before the next request',
                      get()['state']['phase'] != 'awake'
                      and batch.rows(h, 'SELECT status FROM rest_control_candidates WHERE run_id=?', (revoked,))[0]['status'] == 'rejected')
                generation = get()['state']['generation']
                write('/rest/wake', {}, generation, 'cleanup-explicit-wake')
                wait(lambda: get()['state']['phase'] == 'awake', 'explicit wake did not commit')
                check('management wake remains available after source revocation', True)
                check('no real QQ transport or sender claim escaped the durable fence',
                      not batch.rows(h, 'SELECT 1 FROM qq_api_calls')
                      and not batch.rows(h, "SELECT 1 FROM outbox WHERE status IN ('sending','sent','unknown')")
                      and not batch.rows(h, 'SELECT 1 FROM native_worker_leases')
                      and batch.rows(h, 'SELECT next_ms FROM reply_account_pacing WHERE account_id=?',
                                    (batch.ACCOUNT,))[0]['next_ms'] == batch.PACING_FENCE_MS)
                report.update(status='passed', model_requests=len(mock.requests),
                              restart_original_wake_at=wake_at,
                              linux_original_deadline_tested=platform.system() == 'Linux')
            finally:
                h.close()
    except Exception as error:
        report.update(status='failed', failed_check=stage, error_type=type(error).__name__)
        # Exception messages are deliberately limited to route/timeout metadata.
        if isinstance(error, (AssertionError, TimeoutError)):
            report['diagnostic'] = str(error)[:400]
        print('FAIL rest runtime acceptance (' + type(error).__name__ + ')', flush=True)
    finally:
        mock.hold_release.set()
        provider.shutdown()
        provider.server_close()
        report.update(finished_at=base.utc_now(), passed=len(report['checks']))
        output = pathlib.Path(args.report).resolve()
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    return 0 if report['status'] == 'passed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
