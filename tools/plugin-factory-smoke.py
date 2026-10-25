#!/usr/bin/env python3
"""Fresh-database acceptance of the Rust/WIT/Wasmtime Component factory.

Uses a synthetic loopback model and actual isolated compiler/Component workers.
No production user data, account credentials, or live QQ messages are used.
An unavailable sandbox is reported unavailable (exit 2), never passed.
"""
import argparse
import contextlib
import hashlib
import http.server
import importlib.util
import json
import os
import pathlib
import platform
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import uuid

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('plugin_helpers', pathlib.Path(__file__).with_name('memory-candidate-smoke.py'))
helpers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helpers)
ROOT = helpers.PREFIX + '/plugin-factory'
SUM = '''let values: Vec<i64> = input.trim().trim_start_matches('[').trim_end_matches(']').split(',').filter(|v|!v.trim().is_empty()).map(|v|v.trim().parse::<i64>().unwrap_or(0)).collect(); values.iter().sum::<i64>().to_string()'''


@contextlib.contextmanager
def private_fixture(prefix, report):
    """Never remove an unsuccessful fixture that may retain an unknown unit."""
    root = pathlib.Path(tempfile.mkdtemp(prefix=prefix)).resolve(strict=True)
    completed = False
    try:
        yield str(root)
        completed = report.get('status') == 'passed'
    finally:
        if completed:
            expected_parent = pathlib.Path(tempfile.gettempdir()).resolve(strict=True)
            metadata = root.lstat()
            if (root.parent != expected_parent or not root.name.startswith(prefix)
                    or root.is_symlink() or not root.is_dir()
                    or (hasattr(os, 'getuid') and metadata.st_uid != os.getuid())
                    or (platform.system() == 'Linux' and not shutil.rmtree.avoids_symlink_attacks)):
                raise RuntimeError('fixture cleanup target could not be verified')
            shutil.rmtree(root)
        else:
            report['diagnostics_retained'] = True
            report['diagnostics_directory'] = str(root)


class Mock:
    def __init__(self):
        self.hold_release = threading.Event()
        self.requests = 0


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        size = int(self.headers.get('Content-Length', '0'))
        if not 0 < size < 1048576:
            self.send_error(413)
            return
        payload = json.loads(self.rfile.read(size))
        if payload.get('messages', [{}])[0].get('content', '').startswith('DSH_COMPONENT_FACTORY_V1'):
            self.server.mock.requests += 1
            result = json.dumps({'body': SUM})
        else:
            result = 'SYNTHETIC-ACK'
        raw = json.dumps({'choices': [{'message': {'content': result}}]}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)


def specification(version='0.1.0', delta=0):
    return {'plugin_id': 'user.synthetic-sum', 'version': version,
            'description': 'Synthetic pure integer sum acceptance.',
            'input_schema': {'type': 'array', 'items': {'type': 'integer'}},
            'output_schema': {'type': 'integer'},
            'tests': [{'input': [2, 3], 'expected': 5 + delta},
                      {'input': [-4, 1], 'expected': -3 + delta}]}


def version_path(item):
    return ROOT + '/versions/' + (item if isinstance(item, str) else item['id'])


def refresh(harness, item):
    return harness.call('GET', version_path(item))[0]


def tag(item):
    return {'If-Match': '"' + str(item['revision']) + '"'}


def wait_job(harness, job):
    deadline = time.monotonic() + 110
    while time.monotonic() < deadline:
        value = harness.call('GET', ROOT + '/jobs/' + job['id'])[0]
        if value['status'] not in {'queued', 'running'}:
            with harness.db() as database:
                leased = database.execute('SELECT leased FROM plugin_factory_jobs WHERE id=?', (job['id'],)).fetchone()[0]
            if not leased:
                return value
        time.sleep(0.1)
    raise TimeoutError('bounded plugin factory job did not finish and release')


def action(harness, item, kind):
    item = refresh(harness, item)
    job = harness.call('POST', version_path(item) + '/' + kind,
                       {'client_request_id': str(uuid.uuid4())}, headers=tag(item), expected=202)[0]
    result = wait_job(harness, job)
    return refresh(harness, item), result


def create(harness, version, body=SUM, delta=0):
    return harness.call('POST', ROOT, {'client_request_id': str(uuid.uuid4()),
                        'specification': specification(version, delta), 'body': body}, expected=201)[0]


def approve(harness, item):
    item = refresh(harness, item)
    body = {key: item[key] for key in ('bundle_sha256', 'source_sha256', 'component_sha256')}
    return harness.call('POST', version_path(item) + '/approve', dict(body, reviewed=True), headers=tag(item))[0]


def activate(harness, item, rollback=False):
    item = refresh(harness, item)
    routes = harness.call('GET', ROOT)[0]['routes']
    route = next((r for r in routes if r['plugin_id'] == item['plugin_id']), None)
    return harness.call('POST', version_path(item) + ('/rollback' if rollback else '/activate'),
                        {'bundle_sha256': item['bundle_sha256'], 'route_revision': route['revision'] if route else 0}, headers=tag(item))[0]


def cleanup_units(harness):
    if not harness.database.exists():
        return
    with harness.db() as database:
        exists = database.execute("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='plugin_factory_units')").fetchone()[0]
        units = [row[0] for row in database.execute('SELECT unit_name FROM plugin_factory_units WHERE finished_at IS NULL')] if exists else []
    for unit in units:
        raw = unit.removeprefix('dsh-plugin-').removesuffix('.service')
        parsed = uuid.UUID(raw)
        if parsed.version != 4 or unit != 'dsh-plugin-' + str(parsed) + '.service':
            raise AssertionError('invalid exact fixture worker unit')
        subprocess.run(['systemctl', '--user', 'stop', '--', unit], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=6, check=False)
        state = subprocess.run(['systemctl', '--user', 'show', unit, '--property=ActiveState', '--value'], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=4, check=False)
        if state.stdout.strip() not in {b'', b'failed', b'inactive'}:
            raise AssertionError('exact fixture unit remained active')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report', required=True)
    args = parser.parse_args()
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    report = {'kind': 'Rust WIT Component factory actual isolation acceptance', 'started_at': helpers.acceptance.utc_now(),
              'binary_sha256': hashlib.file_digest(binary.open('rb'), 'sha256').hexdigest(),
              'platform': platform.system(), 'architecture': platform.machine(), 'status': 'running',
              'model_is_mock': True, 'real_external_model_tested': False, 'real_qq_tested': False,
              'production_data_accessed': False, 'actual_component_worker_tested': False, 'checks': []}
    mock = Mock()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    server.daemon_threads = True
    server.mock = mock
    threading.Thread(target=server.serve_forever, daemon=True).start()
    exit_code = 0

    def check(name, condition):
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    try:
        with private_fixture('dsh-plugin-factory-smoke-', report) as temporary:
            harness = helpers.Harness(str(binary), str(web), temporary, mock)
            try:
                harness.start()
                report['commit'] = harness.commit
                harness.call('GET', ROOT, authenticated=False, expected=401)
                harness.bootstrap()
                page = harness.call('GET', ROOT)[0]
                if not page['available']:
                    report.update(status='unavailable', availability_code=page.get('availability_code'))
                    return_code = 2
                else:
                    return_code = 0
                    harness.configure(server.server_port)
                    harness.call('POST', ROOT + '/check', {}, with_csrf=False, expected=403)
                    harness.call('POST', ROOT + '/check', {}, authenticated=False, expected=401)
                    harness.call('POST', ROOT + '/check', {}, headers={'Origin': 'https://example.invalid'}, expected=403)
                    # Repeat actual start/verify/stop cycles: systemd may
                    # expose a loaded but initially inactive unit before its
                    # cgroup exists. Never turn that scheduling window into a
                    # false failure, or send guest bytes before enforcement.
                    observed_units = set()
                    report['isolation_probe_repetitions'] = 0
                    for repetition in range(32):
                        isolated = harness.call('POST', ROOT + '/check', {}, expected={200, 409, 503}, timeout=110)[0]
                        if 'error' in isolated:
                            report['isolation_failure_code'] = isolated['error'].get('code')
                            raise AssertionError('actual isolated worker lifecycle probe failed')
                        if (not isolated['passed'] or not all(isolated['probes'].values())
                                or isolated['isolation']['landlock_abi'] < 4
                                or not isolated['isolation']['rlimits_enforced']
                                or not isolated['isolation']['pipe_ioctls_verified']):
                            raise AssertionError('actual isolated worker enforcement probe failed')
                        observed_units.add(isolated['isolation']['unit'])
                        report['isolation_probe_repetitions'] = repetition + 1
                    report['isolation_policy'] = isolated['isolation']['policy']
                    check('thirty two fresh actual workers verify cgroup Landlock seccomp FDs and eight canaries then confirm exact exit',
                          len(observed_units) == 32 and harness.call('GET', ROOT)[0]['available'])
                    request = {'client_request_id': str(uuid.uuid4()), 'specification': specification(), 'body': None}
                    first = harness.call('POST', ROOT, request, expected=201)[0]
                    duplicate = harness.call('POST', ROOT, request, expected=201)[0]
                    check('generation UUID is idempotent and one model request only', duplicate['id'] == first['id'])
                    deadline = time.monotonic() + 30
                    while refresh(harness, first)['status'] in {'specified', 'generating'} and time.monotonic() < deadline:
                        time.sleep(0.1)
                    first = refresh(harness, first)
                    check('natural language request generates Rust source without compiling or approving it',
                          first['status'] == 'generated' and first['body'] and first['component_sha256'] is None and mock.requests == 1)
                    with helpers.other_admin(harness):
                        harness.call('GET', version_path(first), expected=404)
                        check('a second administrator cannot read or invoke another owners plugin', harness.call('GET', ROOT)[0]['versions'] == [])
                        harness.call('POST', ROOT + '/plugins/user.synthetic-sum/invoke', {'input': [1]}, expected=404)
                    first, job = action(harness, first, 'build')
                    check('actual Rust compiler builds a hash-bound WIT Component without running guest code', job['status'] == 'completed' and first['status'] == 'built' and len(first['component_sha256']) == 64)
                    first, job = action(harness, first, 'test')
                    check('actual Wasmtime Component executes all JSON input output contract cases', job['status'] == 'completed' and first['status'] == 'eligible' and first['test_report']['passed'])
                    report['actual_component_worker_tested'] = True
                    harness.call('POST', ROOT + '/plugins/user.synthetic-sum/invoke', {'input': [1]}, expected=404)
                    approval = {key: first[key] for key in ('bundle_sha256', 'source_sha256', 'component_sha256')}
                    harness.call('POST', version_path(first) + '/approve', dict(approval, reviewed=True), expected=428)
                    harness.call('POST', version_path(first) + '/approve', dict(approval, reviewed=False), headers=tag(first), expected=409)
                    harness.call('POST', version_path(first) + '/approve', dict(approval, reviewed=True, component_sha256='0' * 64), headers=tag(first), expected=409)
                    approved = approve(harness, first)
                    harness.call('POST', version_path(first) + '/approve', dict(approval, reviewed=True), headers=tag(first), expected=409)
                    check('test completion grants no activation and explicit approval binds exact source component bundle and revision', approved['status'] == 'approved')
                    first = activate(harness, approved)
                    value = harness.call('POST', ROOT + '/plugins/user.synthetic-sum/invoke', {'input': [4, 5]}, timeout=110)[0]
                    check('separate activation registers the tested tool and actual invocation returns nine', first['status'] == 'active' and value['output'] == 9)
                    harness.call('POST', ROOT + '/plugins/user.synthetic-sum/invoke', {'input': {'owner': 'forged'}}, expected=400)
                    with helpers.other_admin(harness):
                        harness.call('POST', ROOT + '/plugins/user.synthetic-sum/invoke', {'input': [4, 5]}, expected=404)
                    harness.stop()
                    harness.start()
                    harness.login()
                    value = harness.call('POST', ROOT + '/plugins/user.synthetic-sum/invoke', {'input': [4, 5]}, timeout=110)[0]
                    check('approved active version and immutable source hashes survive daemon restart', value['output'] == 9 and refresh(harness, first)['component_sha256'] == first['component_sha256'])
                    second = create(harness, '0.2.0', SUM.replace('values.iter().sum::<i64>().to_string()', '(values.iter().sum::<i64>() + 1).to_string()'), delta=1)
                    second, built = action(harness, second, 'build')
                    second, tested = action(harness, second, 'test')
                    check('second immutable version independently builds and tests', built['status'] == 'completed' and tested['status'] == 'completed')
                    second = activate(harness, approve(harness, second))
                    value = harness.call('POST', ROOT + '/plugins/user.synthetic-sum/invoke', {'input': [4, 5]}, timeout=110)[0]
                    check('version activation switches bundle and preserves previous approved version', value['output'] == 10)
                    first = activate(harness, refresh(harness, first), rollback=True)
                    value = harness.call('POST', ROOT + '/plugins/user.synthetic-sum/invoke', {'input': [4, 5]}, timeout=110)[0]
                    check('explicit rollback restores the exact previously approved stateless version', value['output'] == 9 and refresh(harness, second)['status'] == 'rolled_back')
                    first = refresh(harness, first)
                    harness.call('POST', version_path(first) + '/quarantine', {}, headers=tag(first))
                    harness.call('POST', ROOT + '/plugins/user.synthetic-sum/invoke', {'input': [4, 5]}, expected=404)
                    check('quarantine immediately removes the tool route', refresh(harness, first)['status'] == 'quarantined')

                    # Adversarial guests use the same compiler and worker path.
                    attacks = [
                        ('0.3.0', 'loop {}', 'fuel'),
                        ('0.4.0', 'let n=input.len()+100_000_000;let values=vec![1u8;n];values.iter().map(|v|*v as u64).sum::<u64>().to_string()', 'memory'),
                        ('0.5.0', 'extern "C" { fn forbidden_host(); } unsafe { forbidden_host(); } "0".to_owned()', 'host import'),
                        ('0.6.0', '"7".to_owned()', 'wrong contract expectation'),
                    ]
                    for number, body, label in attacks:
                        item = create(harness, number, body)
                        item, built = action(harness, item, 'build')
                        if built['status'] == 'completed':
                            item, tested = action(harness, item, 'test')
                            rejected = tested['status'] == 'failed'
                        else:
                            rejected = label == 'host import' and built['status'] == 'failed'
                        check('actual adversarial ' + label + ' guest is rejected and cannot become eligible', rejected and item['status'] in {'build_failed', 'test_failed'})
                    with harness.db() as database:
                        unfinished = database.execute('SELECT count(*) FROM plugin_factory_units WHERE finished_at IS NULL').fetchone()[0]
                        leases = database.execute('SELECT sum(leased) FROM plugin_factory_jobs').fetchone()[0]
                        registry = database.execute('SELECT active_version FROM plugin_factory_routes WHERE owner_id=?', (harness.owner,)).fetchone()[0]
                    check('every exact worker exits and releases its lease without resurrecting a quarantined route', unfinished == 0 and not leases and registry is None)
                    report['status'] = 'passed'
                exit_code = return_code
            finally:
                try:
                    harness.stop()
                    cleanup_units(harness)
                finally:
                    harness.close()
    except Exception as error:
        exit_code = 1
        report.update(status='failed', error_type=type(error).__name__)
        trace = error.__traceback__
        while trace:
            if trace.tb_frame.f_code.co_filename == __file__:
                report['failure_line'] = trace.tb_lineno
            trace = trace.tb_next
        if isinstance(error, helpers.ResponseStatusError):
            report.update(actual_http_status=error.actual, expected_http_statuses=error.expected)
        print('FAIL isolated Component factory; inspect sanitized metadata', flush=True)
    finally:
        server.shutdown()
        server.server_close()
        report.update(finished_at=helpers.acceptance.utc_now(), passed=len(report['checks']), mock_model_requests=mock.requests)
        path = pathlib.Path(args.report).resolve()
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps({'status': report['status'], 'passed': report['passed'], 'actual_component_worker_tested': report['actual_component_worker_tested']}))
    return exit_code


if __name__ == '__main__':
    raise SystemExit(main())
