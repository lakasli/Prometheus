#!/usr/bin/env python3
"""Isolated Linux acceptance: mock model, actual systemd/Wasmi worker.

No production data is read. Test cases are synthetic. The daemon's real cgroup
probe must succeed; unavailable platforms are reported unavailable, never passed.
Only metadata and check names are reported, never code, model bodies or secrets.
"""
import argparse
import hashlib
import http.server
import importlib.util
import json
import pathlib
import platform
import secrets
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from contextlib import closing

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location('wasm_acceptance_helpers', pathlib.Path(__file__).with_name('memory-candidate-smoke.py'))
helpers = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(helpers)
PREFIX = helpers.PREFIX
ROOT = PREFIX + '/wasm-workbench'
TERMINAL = {'completed', 'failed', 'cancelled', 'interrupted', 'expired', 'deleted'}
WAT = '(module (func (export "calc") (param i32 i32) (result i32) local.get 0 local.get 1 i32.add))'
CASES = [{'a': 2, 'b': 3, 'expected': 5}, {'a': -3, 'b': 1, 'expected': -2}]


class Mock:
    def __init__(self):
        self.lock = threading.Lock()
        self.requests = []
        self.plans = {}
        self.hold_release = threading.Event()
        self.hold_received = threading.Event()
        self.hold_responded = threading.Event()

    def arm(self, spec, mode='normal', held=False):
        self.plans[spec] = (mode, held)
        if held:
            self.hold_release.clear()
            self.hold_received.clear()
            self.hold_responded.clear()

    def count(self):
        with self.lock:
            return len(self.requests)

    def respond(self, payload):
        system = payload['messages'][0]['content']
        if not system.startswith('DSH_PURE_WAT_WORKBENCH_V1'):
            return 'SYNTHETIC-CHAT-ACK'
        source = json.loads(payload['messages'][-1]['content'])
        if set(source) != {'spec', 'tests'}:
            raise AssertionError('generation source envelope differs from contract')
        mode, held = self.plans.get(source['spec'], ('normal', False))
        with self.lock:
            self.requests.append(payload)
        if held:
            self.hold_received.set()
            self.hold_release.wait(timeout=25)
        try:
            wat = WAT
            if mode == 'invalid_wat':
                wat = '(module invalid syntax'
            elif mode == 'imports':
                wat = '(module (import "env" "x" (func)) (func (export "calc") (param i32 i32) (result i32) i32.const 0))'
            elif mode == 'memory':
                wat = '(module (memory 1) (func (export "calc") (param i32 i32) (result i32) i32.const 0))'
            elif mode == 'fuel':
                wat = '(module (func (export "calc") (param i32 i32) (result i32) (loop br 0) i32.const 0))'
            result = json.dumps({'wat': wat})
            if mode == 'extra':
                result = json.dumps({'wat': wat, 'tools': []})
            elif mode == 'fenced':
                result = '```json\n' + result + '\n```'
            return result
        finally:
            if held:
                self.hold_responded.set()


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        size = int(self.headers.get('Content-Length', '0'))
        if not 0 < size <= 1024 * 1024:
            self.send_error(413)
            return
        content = self.server.mock.respond(json.loads(self.rfile.read(size)))
        raw = json.dumps({'choices': [{'message': {'role': 'assistant', 'content': content}}]}).encode()
        try:
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass


def generation(spec=None, client=None):
    return {'client_request_id': client or str(uuid.uuid4()), 'name': '合成整数加法',
            'spec': spec or '返回两个i32整数的回绕加法。合成验收 ' + secrets.token_hex(5),
            'tests': [dict(value) for value in CASES]}


def job_path(identifier):
    return ROOT + '/jobs/' + identifier


def artifact_path(identifier):
    return ROOT + '/artifacts/' + identifier


def job_row(harness, identifier):
    with harness.db() as database:
        return dict(database.execute('SELECT * FROM wasm_workbench_jobs WHERE id=?', (identifier,)).fetchone())


def wait_job(harness, identifier):
    deadline = time.monotonic() + 38
    while time.monotonic() < deadline:
        value, _, headers = harness.call('GET', job_path(identifier))
        if headers.get('ETag') != '"' + str(value['revision']) + '"':
            raise AssertionError('job revision ETag differs from contract')
        if value['status'] in TERMINAL:
            return value
        time.sleep(0.03)
    raise TimeoutError('bounded workbench job did not finish')


def wait_released(harness, identifier):
    deadline = time.monotonic() + 9
    while time.monotonic() < deadline:
        row = job_row(harness, identifier)
        model_done = row['model_started_at'] is None or row['model_finished_at'] is not None
        worker_done = row['worker_started_at'] is None or row['worker_finished_at'] is not None
        if model_done and worker_done:
            return row
        time.sleep(0.03)
    raise TimeoutError('model or worker exit remains unconfirmed')


def approve(harness, artifact):
    return harness.call('POST', artifact_path(artifact['id']) + '/approve',
                        {'wasm_sha256': artifact['wasm_sha256'], 'tests_sha256': artifact['tests_sha256']},
                        headers={'If-Match': '"' + str(artifact['revision']) + '"'})[0]


def execute(harness, artifact, a=17, b=-9, client=None, expected=202):
    return harness.call('POST', artifact_path(artifact['id']) + '/execute',
                        {'client_request_id': client or str(uuid.uuid4()), 'a': a, 'b': b,
                         'wasm_sha256': artifact['wasm_sha256']}, expected=expected,
                        headers={'If-Match': '"' + str(artifact['revision']) + '"'})[0]


def unrelated_counts(harness):
    with harness.db() as database:
        return {table: database.execute('SELECT count(*) FROM ' + table).fetchone()[0]
                for table in ['runs', 'messages', 'memory_records', 'summary_jobs', 'memory_candidate_jobs', 'image_analysis_jobs']}


def cleanup_units(harness):
    """Stop only exact UUID units recorded in this private fixture database."""
    harness.stop()
    if not harness.database.is_file():
        return
    with harness.db() as database:
        units = [row[0] for row in database.execute('SELECT unit_name FROM wasm_workbench_units WHERE finished_at IS NULL')]
    for unit in units:
        if not unit.startswith('dsh-wasm-') or not unit.endswith('.service'):
            raise AssertionError('fixture contains an invalid exact worker unit')
        raw = unit[len('dsh-wasm-'):-len('.service')]
        parsed = uuid.UUID(raw)
        if parsed.version != 4 or str(parsed) != raw:
            raise AssertionError('fixture unit is not a canonical UUID4 unit')
        if platform.system() != 'Linux':
            raise AssertionError('unexpected unconfirmed worker outside Linux')
        subprocess.run(['systemctl', '--user', 'stop', unit], stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL, check=False, timeout=4)
        status = subprocess.run(['systemctl', '--user', 'show', unit, '--property=ActiveState', '--value'],
                                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, check=False, timeout=4)
        if status.stdout.strip() not in {b'inactive', b'failed', b''}:
            raise AssertionError('fixture worker cleanup did not confirm inactivity')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report', required=True)
    args = parser.parse_args()
    binary, web = pathlib.Path(args.binary).resolve(strict=True), pathlib.Path(args.web_dir).resolve(strict=True)
    with binary.open('rb') as handle:
        binary_hash = hashlib.file_digest(handle, 'sha256').hexdigest()
    report = {'kind': 'WAT workbench mock-model actual-systemd-worker acceptance',
              'started_at': helpers.acceptance.utc_now(), 'commit': None, 'deployment_sha': None,
              'binary_sha256': binary_hash, 'platform': platform.system(), 'architecture': platform.machine(),
              'status': 'running', 'model_is_mock': True, 'actual_systemd_worker_tested': False,
              'real_external_model_tested': False, 'resource_stress_probe_tested': False,
              'production_data_accessed': False, 'real_user_content_accessed': False,
              'chat_tool_registration_tested': False, 'checks': []}
    stage, exit_code = 'setup', 0

    def check(name, condition):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    mock = Mock()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    server.daemon_threads = True
    server.mock = mock
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-wasm-workbench-smoke-') as temporary:
            harness = helpers.Harness(str(binary), str(web), temporary, mock)
            try:
                harness.start()
                report['commit'] = report['deployment_sha'] = harness.commit
                harness.call('GET', ROOT, authenticated=False, expected=401)
                harness.bootstrap()
                empty, _, _ = harness.call('GET', ROOT)
                if not empty['available']:
                    report.update(status='unavailable', availability_code=empty.get('availability_code'))
                    exit_code = 2
                else:
                    harness.configure(server.server_port)
                    configured, _, _ = harness.call('GET', ROOT)
                    baseline = unrelated_counts(harness)
                    with harness.db() as database:
                        initial_reader = database.execute('SELECT max(version) FROM dsh_schema_migrations').fetchone()[0]
                    check('real worker resource gate is available and empty tables retain the previous reader',
                          configured['available'] and configured['generation_available'] and initial_reader < 7
                          and configured['engine_version'] == 'wasmi-2.0.0' and configured['policy_version'] == 'pure-i32-v1')
                    request = generation()
                    harness.call('POST', ROOT + '/generate', request, authenticated=False, expected=401)
                    harness.call('POST', ROOT + '/generate', request, with_csrf=False, expected=403)
                    harness.call('POST', ROOT + '/generate', request, headers={'Origin': 'https://example.invalid'}, expected=403)
                    for invalid in [dict(request, client_request_id='bad'), dict(request, tests=[CASES[0]]),
                                    dict(request, tests=[CASES[0], CASES[0]]), dict(request, spec='password = SyntheticPrivateSecret987654321'),
                                    dict(request, spec='文' * 1334), dict(request, name='bad\0name')]:
                        harness.call('POST', ROOT + '/generate', invalid, expected=400)
                    harness.call('POST', ROOT + '/generate', dict(request, tools=[]), expected=422)
                    check('auth Origin CSRF source bounds distinct cases secrets and unknown fields reject before model work', mock.count() == 0)
                    first, _, _ = harness.call('POST', ROOT + '/generate', request, expected=202)
                    completed = wait_job(harness, first['id'])
                    released = wait_released(harness, first['id'])
                    check('mock-generated WAT compiles and all user cases pass inside the actual resource-gated worker',
                          completed['status'] == 'completed' and all(item['passed'] for item in completed['test_results'])
                          and released['worker_started_at'] is not None and released['worker_finished_at'] is not None)
                    artifact, _, _ = harness.call('GET', artifact_path(completed['artifact_id']))
                    with harness.db() as database:
                        stored = database.execute('SELECT wasm,wasm_sha256,tests_json,tests_sha256 FROM wasm_workbench_artifacts WHERE id=?', (artifact['id'],)).fetchone()
                        reader = database.execute('SELECT max(version) FROM dsh_schema_migrations').fetchone()[0]
                    check('private immutable artifact hashes bind actual wasm tests engine and reader seven',
                          artifact['status'] == 'ready' and hashlib.sha256(stored[0]).hexdigest() == stored[1]
                          and json.loads(stored[2]) == CASES and stored[3] == artifact['tests_sha256'] and reader == 7
                          and 'wasm_base64' not in json.dumps(artifact) and 'config_json' not in json.dumps(artifact))
                    repeat, _, _ = harness.call('POST', ROOT + '/generate', request, expected=202)
                    harness.call('POST', ROOT + '/generate', dict(request, spec=request['spec'] + ' changed'), expected=409)
                    check('identical generation UUID returns one existing job and never repeats model or worker', repeat['id'] == first['id'] and mock.count() == 1)
                    with helpers.other_admin(harness):
                        harness.call('GET', job_path(first['id']), expected=404)
                        harness.call('GET', artifact_path(artifact['id']), expected=404)
                        page, _, _ = harness.call('GET', ROOT)
                        check('a second authenticated administrator cannot inspect another owners code or jobs', page['jobs'] == [] and page['artifacts'] == [])
                    execute(harness, artifact, expected=409)
                    approve_path = artifact_path(artifact['id']) + '/approve'
                    approval_body = {'wasm_sha256': artifact['wasm_sha256'], 'tests_sha256': artifact['tests_sha256']}
                    harness.call('POST', approve_path, approval_body, expected=428)
                    harness.call('POST', approve_path, approval_body, headers={'If-Match': '"0"'}, expected=400)
                    harness.call('POST', approve_path, dict(approval_body, tests_sha256='wrong'), headers={'If-Match': '"1"'}, expected=409)
                    approved = approve(harness, artifact)
                    harness.call('POST', approve_path, approval_body, headers={'If-Match': '"1"'}, expected=409)
                    execution_client = str(uuid.uuid4())
                    execution = execute(harness, approved, client=execution_client)
                    actual = wait_job(harness, execution['id'])
                    wait_released(harness, execution['id'])
                    duplicate = execute(harness, approved, client=execution_client)
                    execute(harness, approved, a=18, client=execution_client, expected=409)
                    check('explicit hash-bound approval allows one actual pure-i32 execution with stable UUID idempotency',
                          actual['status'] == 'completed' and actual['result'] == 8 and duplicate['id'] == execution['id'])
                    report['actual_systemd_worker_tested'] = True

                    for mode in ['extra', 'fenced', 'invalid_wat', 'imports', 'memory', 'fuel', 'wrong_tests']:
                        invalid = generation('合成拒绝模式 ' + mode + ' ' + secrets.token_hex(4))
                        if mode == 'wrong_tests':
                            invalid['tests'][0]['expected'] = 999
                        mock.arm(invalid['spec'], mode)
                        before = mock.count()
                        failed_job, _, _ = harness.call('POST', ROOT + '/generate', invalid, expected=202)
                        failed = wait_job(harness, failed_job['id'])
                        wait_released(harness, failed_job['id'])
                        check('single-attempt generation ' + mode + ' cannot produce a ready artifact',
                              failed['status'] == 'failed' and not failed.get('artifact_id') and bool(failed.get('error_code'))
                              and job_row(harness, failed['id'])['spec'] is None and mock.count() == before + 1)

                    held = generation('合成持有模型取消与排队撤销 ' + secrets.token_hex(5))
                    mock.arm(held['spec'], held=True)
                    with helpers.other_admin(harness):
                        foreign_owner = harness.owner
                        holding, _, _ = harness.call('POST', ROOT + '/generate', held, expected=202)
                        if not mock.hold_received.wait(timeout=10):
                            raise AssertionError('held generation model request was not observed')
                        current, _, _ = harness.call('GET', job_path(holding['id']))
                        harness.call('POST', ROOT + '/generate', generation(), expected=409)
                        with helpers.other_admin(harness):
                            second, _, _ = harness.call('POST', ROOT + '/generate', generation(), expected=202)
                            with helpers.other_admin(harness):
                                harness.call('POST', ROOT + '/generate', generation(), expected=409)
                            harness.call('DELETE', job_path(second['id']), headers={'If-Match': '"' + str(second['revision']) + '"'})
                        harness.call('DELETE', job_path(holding['id']), expected=428)
                        harness.call('DELETE', job_path(holding['id']), headers={'If-Match': '"' + str(current['revision']) + '"'})
                        stopped = wait_released(harness, holding['id'])
                        mock.hold_release.set()
                        mock.hold_responded.wait(timeout=5)
                        check('owner and global queues bound work and cancellation waits for model transport exit without retry',
                              stopped['owner_id'] == foreign_owner and stopped['status'] == 'cancelled'
                              and stopped['model_started_at'] is not None and stopped['model_finished_at'] is not None
                              and stopped['worker_started_at'] is None)

                    pending = execute(harness, approved)
                    # Cancel/revoke may beat launch or meet the real worker in
                    # flight. Either way no subsequently committed result is valid.
                    before_revoke = job_row(harness, pending['id'])
                    revoked, _, _ = harness.call('POST', artifact_path(approved['id']) + '/revoke', {},
                        headers={'If-Match': '"' + str(approved['revision']) + '"'})
                    after = wait_job(harness, pending['id'])
                    wait_released(harness, pending['id'])
                    execute(harness, revoked, expected=409)
                    history, _, _ = harness.call('GET', job_path(execution['id']))
                    check('revocation stops new authorization and late active results while preserving earlier private history',
                          revoked['status'] == 'revoked' and history['result'] == 8
                          and (after['status'] == 'cancelled' and not after.get('result') or after['status'] == 'completed' and after.get('result') == 8))
                    report['revoked_active_worker_tested'] = after['status'] == 'cancelled' and before_revoke['worker_started_at'] is not None and before_revoke['worker_finished_at'] is None
                    with harness.db() as database:
                        database.execute('UPDATE wasm_workbench_jobs SET expires_at=0 WHERE id=?', (execution['id'],))
                    expired, _, _ = harness.call('GET', job_path(execution['id']))
                    private = job_row(harness, execution['id'])
                    check('owner execution TTL clears arguments and result while keeping immutable private code',
                          expired['status'] == 'expired' and not expired.get('result') and private['args_json'] is None)
                    deleted, _, _ = harness.call('DELETE', job_path(execution['id']), headers={'If-Match': '"' + str(expired['revision']) + '"'})
                    harness.call('DELETE', job_path(execution['id']), headers={'If-Match': '"' + str(expired['revision']) + '"'}, expected=409)
                    check('explicit deletion keeps idempotency metadata and never creates a fresh execution', deleted['status'] == 'deleted')
                    check('standalone factory does not write chat memory image or candidate data', unrelated_counts(harness) == baseline)
                    with harness.db() as database:
                        sample = dict(database.execute('SELECT * FROM wasm_workbench_jobs LIMIT 1').fetchone())
                        total = database.execute('SELECT count(*) FROM wasm_workbench_jobs').fetchone()[0]
                        for _ in range(1000 - total):
                            row = dict(sample, id=str(uuid.uuid4()), client_request_id=str(uuid.uuid4()), status='deleted',
                                       spec=None, tests_json=None, args_json=None, result=None, test_results_json=None,
                                       unit_name=None, worker_started_at=None, worker_finished_at=None,
                                       model_started_at=None, model_finished_at=None)
                            fields = list(row)
                            database.execute('INSERT INTO wasm_workbench_jobs(' + ','.join(fields) + ') VALUES(' + ','.join('?' for _ in fields) + ')', tuple(row[field] for field in fields))
                    capacity, _, _ = harness.call('POST', ROOT + '/generate', generation(), expected=409)
                    check('thousand-job hard ceiling rejects new work without deleting historical metadata', capacity['error']['code'] == 'wasm_workbench_capacity')
                    report['status'] = 'passed'
            finally:
                mock.hold_release.set()
                try:
                    cleanup_units(harness)
                finally:
                    harness.close()
    except Exception as error:
        exit_code = 1
        report.update(status='failed', failure_after_check=stage, error_type=type(error).__name__)
        trace = error.__traceback__
        while trace:
            if trace.tb_frame.f_code.co_filename == __file__:
                report['failure_line'] = trace.tb_lineno
            trace = trace.tb_next
        if isinstance(error, helpers.ResponseStatusError):
            report.update(actual_http_status=error.actual, expected_http_statuses=error.expected)
        print('FAIL isolated WAT workbench; inspect sanitized metadata', flush=True)
    finally:
        mock.hold_release.set()
        server.shutdown()
        server.server_close()
        report.update(finished_at=helpers.acceptance.utc_now(), passed=len(report['checks']), mock_model_requests=mock.count())
        path = pathlib.Path(args.report).resolve()
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps({'status': report['status'], 'passed': report['passed'], 'actual_systemd_worker_tested': report['actual_systemd_worker_tested'], 'real_external_model_tested': False}))
    return exit_code


if __name__ == '__main__':
    raise SystemExit(main())
