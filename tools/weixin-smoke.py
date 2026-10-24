#!/usr/bin/env python3
"""Real-process Weixin management safety acceptance with private synthetic data.

The default workflow never creates an active QR job, enables an account, polls
the official service, or sends a message. --official-qr-probe explicitly fetches
one QR through the production HTTPS boundary and immediately cancels the job;
it never displays/saves the QR, supplies a verification code, confirms a scan,
binds an account, or sends chat. All fixtures are isolated and disposable.
"""
import argparse
import contextlib
import hashlib
import importlib.util
import json
import os
import pathlib
import platform
import re
import sqlite3
import subprocess
import tempfile
import threading
import time
import types
import uuid


PREFIX = '/api/admin/v1'
WEIXIN = PREFIX + '/weixin'
ACTIVE = ('creating', 'waiting', 'scanned', 'need_verification', 'verifying', 'redirecting')


def load_module(name, filename):
    specification = importlib.util.spec_from_file_location(name, filename)
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    return module


acceptance = load_module('weixin_private_acceptance', pathlib.Path(__file__).with_name('memory-smoke.py'))


class PrivateDaemon(acceptance.Harness):
    def start(self):
        # Unlike the model mock harness, this workflow has no reason to enable
        # private model endpoints. iLink always retains its production boundary.
        self.process = subprocess.Popen([
            self.binary, 'serve', '--data-dir', str(self.data_dir), '--listen',
            '127.0.0.1:' + str(self.server_port), '--origin', self.base,
            '--web-dir', self.web_dir,
        ], env=self.environment, stdout=self.log, stderr=self.log)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError('private_daemon_exited_before_ready')
            try:
                health, _, _ = self.request('GET', '/health/ready')
                self.commit = health['commit']
                return
            except (OSError, AssertionError):
                time.sleep(0.05)
        raise TimeoutError('private_daemon_readiness_timeout')


@contextlib.contextmanager
def scratch_parent(value):
    if value is None:
        yield None
        return
    parent = pathlib.Path(value).absolute()
    if parent.is_symlink() or parent.exists() and (not parent.is_dir() or any(parent.iterdir())):
        raise ValueError('scratch_parent_must_be_empty_and_not_a_symlink')
    parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    # A separately owned TemporaryDirectory below this empty parent is the only
    # recursively cleaned path. Existing data directories are never accepted.
    yield str(parent.resolve())


def scalar(harness, sql, arguments=()):
    with harness.db() as database:
        return database.execute(sql, arguments).fetchone()[0]


def seed_login(harness, status, marker, *, owner=None):
    job = str(uuid.uuid4())
    timestamp = int(time.time())
    with harness.db() as database:
        database.execute(
            'INSERT INTO weixin_login_jobs(id,owner_id,status,revision,expected_account_revision,'
            'expires_at,created_at,updated_at,qrcode,display_content,verification_code) '
            'VALUES(?,?,?,1,0,?,?,?,?,?,?)',
            (job, owner or harness.owner, status, timestamp + 300, timestamp, timestamp,
             marker + '-identifier', marker + '-display', marker))
    return job


def seed_disabled_account(harness, secret):
    reference = str(uuid.uuid4())
    path = harness.data_dir / 'secrets' / reference
    with path.open('x', encoding='utf-8') as handle:
        handle.write(secret)
    if os.name != 'nt':
        path.chmod(0o600)
    with harness.db() as database:
        database.execute('INSERT INTO secrets(id,kind,created_at) VALUES(?,\'weixin_bot_token\',?)',
                         (reference, int(time.time())))
        database.execute(
            'INSERT INTO weixin_account(singleton,account_id,base_url,credential_ref,linked_user_id,'
            'revision,generation,enabled,cursor,connection_status,last_error_code,last_event_at,updated_at) '
            'VALUES(1,\'fixture@im.bot\',\'https://ilinkai.weixin.qq.com/\',?, '
            '\'private-fixture-owner@im.wechat\',1,?,0,\'private-fixture-cursor\',\'disabled\',NULL,NULL,?)',
            (reference, str(uuid.uuid4()), int(time.time())))
        database.execute('INSERT OR IGNORE INTO dsh_schema_migrations(version,created_at) VALUES(4,?)',
                         (int(time.time()),))
    return reference


def refused_future_start(harness, root, table, version, expected_diagnostic):
    directory = pathlib.Path(root)
    directory.mkdir(mode=0o700)
    with harness.db() as source, contextlib.closing(sqlite3.connect(directory / 'dsh.db')) as target:
        source.backup(target)
        timestamp_column = 'applied_at' if table == 'weixin_schema_migrations' else 'created_at'
        target.execute(f'INSERT INTO {table}(version,{timestamp_column}) VALUES(?,?)',
                       (version, int(time.time())))
        target.commit()
    server_port = acceptance.free_port()
    process = subprocess.Popen([
        harness.binary, 'serve', '--data-dir', str(directory), '--listen',
        '127.0.0.1:' + str(server_port), '--origin', 'http://127.0.0.1:' + str(server_port),
        '--web-dir', harness.web_dir,
    ], env=harness.environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        output, error = process.communicate(timeout=10)
    except subprocess.TimeoutExpired:
        process.terminate()
        try:
            process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.communicate(timeout=5)
        return False
    # Only inspect static migration diagnostics locally; never print daemon
    # output or include it in the JSON report.
    return process.returncode != 0 and expected_diagnostic.encode() in output + error


def official_probe(harness, report, check):
    report['real_qr_fetch_tested'] = True
    operation, _, _ = harness.request('POST', WEIXIN + '/login', {}, expected=202,
                                      headers={'If-Match': '"0"'})
    job = operation['id']
    path = WEIXIN + '/login/' + job
    deadline = time.monotonic() + 25
    view = operation
    qr = None
    try:
        while time.monotonic() < deadline:
            view, _, _ = harness.request('GET', path)
            if view.get('qr_content'):
                qr = view['qr_content']
                break
            if view['status'] in {'confirmed', 'existing_credentials'}:
                raise AssertionError('unexpected_account_confirmation')
            if view['status'] in {'failed', 'expired', 'interrupted', 'verification_blocked'}:
                error_code = view.get('error_code')
                if isinstance(error_code, str) and re.fullmatch(r'[A-Za-z0-9_-]{1,64}', error_code):
                    report['official_qr_error_code'] = error_code
                raise AssertionError('official_qr_fetch_did_not_complete')
            time.sleep(0.1)
        check('explicit official QR probe obtains display content without scanning or submitting a code',
              isinstance(qr, str) and bool(qr))
        ordinary, raw, _ = harness.request('GET', WEIXIN)
        check('ordinary management status never returns the real QR display content',
              'qr_content' not in (ordinary.get('pending_login') or {}) and qr.encode() not in raw)
        harness.request('GET', path, expected=401, authenticated=False)
        check('real QR owner endpoint still rejects unauthenticated reads', True)
    finally:
        # Cancellation is attempted even when the probe fails; no code or scan
        # confirmation is ever sent. A revision race is resolved by rereading.
        for _ in range(4):
            view, _, _ = harness.request('GET', path)
            if view['status'] not in ACTIVE:
                break
            try:
                harness.request('DELETE', path, expected=200,
                                headers={'If-Match': '"' + str(view['revision']) + '"'})
                break
            except AssertionError:
                continue
    view, _, _ = harness.request('GET', path)
    state, _, _ = harness.request('GET', WEIXIN)
    check('official QR probe is cancelled and has not bound any account',
          view['status'] == 'cancelled' and not view.get('qr_content') and state['account'] is None)
    check('cancelled official QR probe clears private transport values',
          scalar(harness, 'SELECT count(*) FROM weixin_login_jobs WHERE id=? AND '
                 '(qrcode IS NOT NULL OR display_content IS NOT NULL OR verification_code IS NOT NULL)',
                 (job,)) == 0)
    report['real_qr_fetch_passed'] = True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report')
    parser.add_argument('--data-dir', help='Optional empty scratch parent; an owned temporary subdirectory is used and cleaned.')
    parser.add_argument('--official-qr-probe', action='store_true', help='Explicitly fetch one official QR and immediately cancel; never scan, bind or send.')
    args = parser.parse_args()
    binary = str(pathlib.Path(args.binary).resolve(strict=True))
    web_dir = str(pathlib.Path(args.web_dir).resolve(strict=True))
    with open(binary, 'rb') as handle:
        binary_hash = hashlib.file_digest(handle, 'sha256').hexdigest()
    report = {
        'kind': 'weixin-management isolated real-process safety black-box',
        'started_at': acceptance.utc_now(), 'platform': platform.system(),
        'architecture': platform.machine(), 'status': 'running', 'deployment_sha': None,
        'commit': None, 'binary_sha256': binary_hash, 'production_data_accessed': False,
        'official_qr_probe_requested': args.official_qr_probe,
        'real_qr_fetch_tested': False, 'real_qr_fetch_passed': False,
        'real_weixin_private_chat_tested': False, 'real_external_model_tested': False,
        'checks': [],
    }
    phase = 'private fixture setup'

    def check(name, predicate):
        nonlocal phase
        phase = name
        if not predicate:
            raise AssertionError('acceptance_check_failed')
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    failed = False
    harness = None
    try:
        with scratch_parent(args.data_dir) as parent:
            with tempfile.TemporaryDirectory(prefix='dsh-weixin-smoke-', dir=parent) as temporary:
                dummy = types.SimpleNamespace(hold_release=threading.Event())
                harness = PrivateDaemon(binary, web_dir, temporary, dummy)
                try:
                    harness.start()
                    report['deployment_sha'] = report['commit'] = harness.commit
                    for path in [WEIXIN, WEIXIN + '/login/' + str(uuid.uuid4())]:
                        harness.request('GET', path, expected=401, authenticated=False)
                    check('Weixin management and QR owner reads require authentication', True)
                    harness.bootstrap()
                    initial, _, headers = harness.request('GET', WEIXIN)
                    check('fresh account is empty with honest private-text-only capabilities',
                          initial['account'] is None and initial['revision'] == 0
                          and initial['pending_login'] is None
                          and initial['capabilities'] == {'text_private': True, 'linked_owner_only': True,
                                                         'groups': False, 'media': False, 'max_accounts': 1})
                    check('delivery counters are present without pretending acceptance is phone receipt',
                          initial['delivery'] == {'queued': 0, 'sending': 0, 'accepted': 0, 'failed': 0, 'unknown': 0})
                    check('authenticated management responses forbid caching', headers.get('Cache-Control') == 'no-store')
                    harness.request('POST', WEIXIN + '/login', {}, expected=403, with_csrf=False,
                                    headers={'If-Match': '"0"'})
                    harness.request('POST', WEIXIN + '/login', {}, expected=403,
                                    headers={'If-Match': '"0"', 'Origin': 'https://example.invalid'})
                    check('QR mutations enforce CSRF and the exact management Origin', True)
                    harness.request('POST', WEIXIN + '/login', {}, expected=428)
                    harness.request('POST', WEIXIN + '/login', {}, expected=400, headers={'If-Match': '"invalid"'})
                    harness.request('POST', WEIXIN + '/login', {}, expected=409, headers={'If-Match': '"1"'})
                    check('QR creation requires If-Match and stale versions cannot create a job',
                          scalar(harness, 'SELECT count(*) FROM weixin_login_jobs') == 0)
                    marker = '9374510268437190'
                    fake = WEIXIN + '/login/' + str(uuid.uuid4())
                    for label, body in [('numeric', {'code': int(marker)}),
                                        ('nested', {'code': {'nested': marker}}),
                                        ('non_digit', {'code': marker + 'x'})]:
                        _, raw, _ = harness.request('POST', fake + '/verification', body, expected=400,
                                                    headers={'If-Match': '"1"'})
                        check('verification validation rejects ' + label + ' input without echoing sensitive values',
                              marker.encode() not in raw)
                    harness.request('DELETE', fake, expected=428)
                    harness.request('DELETE', fake, expected=404, headers={'If-Match': '"1"'})
                    check('unknown QR operations cannot mutate another login task', True)
                    harness.request('POST', WEIXIN + '/check', {}, expected=400, headers={'If-Match': '"0"'})
                    harness.request('POST', WEIXIN + '/enabled', {'enabled': True}, expected=400,
                                    headers={'If-Match': '"0"'})
                    check('connection checks and enable reject an unconfigured account without creating work',
                          scalar(harness, 'SELECT count(*) FROM weixin_checks') == 0
                          and scalar(harness, 'SELECT count(*) FROM weixin_login_jobs') == 0)
                    blocked = seed_login(harness, 'verification_blocked', 'synthetic-private-blocked')
                    view, raw, _ = harness.request('GET', WEIXIN + '/login/' + blocked)
                    check('blocked QR state is terminal and clears its private QR and pairing data',
                          view['status'] == 'verification_blocked' and not view.get('qr_content')
                          and b'synthetic-private-blocked' not in raw)
                    other = seed_login(harness, 'cancelled', 'synthetic-other-owner', owner='other-fixture-owner')
                    harness.request('GET', WEIXIN + '/login/' + other, expected=404)
                    check('a logged-in administrator cannot read a different QR owner task', True)
                    harness.stop()
                    harness.start()
                    harness.login()
                    restarted, _, _ = harness.request('GET', WEIXIN)
                    check('daemon restart preserves an empty account and creates no QR request',
                          restarted['account'] is None and restarted['revision'] == 0
                          and restarted['pending_login'] is None)
                    if args.official_qr_probe:
                        phase = 'explicit official QR fetch and immediate cancellation'
                        official_probe(harness, report, check)
                    harness.stop()
                    interrupted = seed_login(harness, 'need_verification', 'synthetic-unfinished-pairing')
                    harness.start()
                    harness.login()
                    view, raw, _ = harness.request('GET', WEIXIN + '/login/' + interrupted)
                    check('restart interrupts private QR work and scrubs it before workers start',
                          view['status'] == 'interrupted' and not view.get('qr_content')
                          and b'synthetic-unfinished-pairing' not in raw
                          and scalar(harness, 'SELECT count(*) FROM weixin_login_jobs WHERE qrcode IS NOT NULL '
                                     'OR display_content IS NOT NULL OR verification_code IS NOT NULL') == 0)
                    secret = 'synthetic-private-weixin-token-' + uuid.uuid4().hex
                    reference = seed_disabled_account(harness, secret)
                    account, raw, _ = harness.request('GET', WEIXIN)
                    check('disabled synthetic account projects status without token, cursor, linked user or credential reference',
                          account['revision'] == 1 and account['account']['enabled'] is False
                          and account['account']['credential_configured'] is True
                          and all(value.encode() not in raw for value in [secret, reference,
                              'private-fixture-cursor', 'private-fixture-owner@im.wechat']))
                    harness.request('DELETE', PREFIX + '/secrets/' + reference, expected=409)
                    check('generic credential deletion protects a disabled Weixin account reference',
                          (harness.data_dir / 'secrets' / reference).is_file()
                          and scalar(harness, 'SELECT count(*) FROM secrets WHERE id=?', (reference,)) == 1)
                    harness.request('POST', WEIXIN + '/enabled', {'enabled': False}, expected=409,
                                    headers={'If-Match': '"0"'})
                    disabled, _, _ = harness.request('POST', WEIXIN + '/enabled', {'enabled': False},
                                                      headers={'If-Match': '"1"'})
                    check('configuration revision zero is a real precondition and disable applies a new version',
                          disabled['revision'] == 2 and disabled['account']['enabled'] is False)
                    with harness.db() as database:
                        database.execute("UPDATE weixin_account SET connection_status='needs_login',last_error_code='token_expired'")
                    harness.request('POST', WEIXIN + '/enabled', {'enabled': True}, expected=400,
                                    headers={'If-Match': '"2"'})
                    check('known expired credentials cannot be re-enabled without a new login',
                          scalar(harness, 'SELECT enabled FROM weixin_account') == 0)
                    for path in [WEIXIN, PREFIX + '/config', PREFIX + '/memory/export', PREFIX + '/secrets']:
                        _, raw, _ = harness.request('GET', path)
                        if secret.encode() in raw:
                            raise AssertionError('credential_material_leaked')
                    check('management metadata and exports never return the stored synthetic token', True)
                    harness.log.flush()
                    diagnostic = (harness.root / 'daemon.log').read_bytes()
                    check('validation and daemon diagnostics do not echo pairing digits or stored token',
                          marker.encode() not in diagnostic and secret.encode() not in diagnostic)
                    harness.stop()
                    phase = 'future schema startup guards'
                    check('future global reader schema is refused before serving', refused_future_start(
                        harness, pathlib.Path(temporary) / 'future-global', 'dsh_schema_migrations', 1000,
                        'database schema is newer than this binary'))
                    check('future Weixin storage schema is refused before serving', refused_future_start(
                        harness, pathlib.Path(temporary) / 'future-weixin', 'weixin_schema_migrations', 2,
                        'Weixin storage schema is newer than this binary'))
                    check('future Weixin login schema is refused before serving', refused_future_start(
                        harness, pathlib.Path(temporary) / 'future-login', 'weixin_login_schema', 2,
                        'Unsupported weixin login schema'))
                    compatibility = load_module('weixin_compatibility_acceptance', pathlib.Path(__file__).resolve().parents[1] / 'deploy/check-compatibility.py')
                    release = pathlib.Path(temporary) / 'old-reader'
                    (release / 'deploy').mkdir(parents=True)
                    manifest = release / 'deploy/release-capabilities.json'
                    manifest.write_text(json.dumps({'schema_reader': 4, 'character_snapshot': 1,
                                                   'assistant_memory': 1, 'memory_context': 1,
                                                   'conversation_summary': 1}), encoding='utf-8')
                    blocked_reader = False
                    try:
                        compatibility.check(harness.data_dir, release)
                    except RuntimeError as error:
                        blocked_reader = 'iLink' in str(error)
                    check('deployment capability gate rejects silent loss of Weixin account and private contexts', blocked_reader)
                    capability = json.loads(manifest.read_text())
                    capability.update(weixin_ilink=1, weixin_private_context=1)
                    manifest.write_text(json.dumps(capability), encoding='utf-8')
                    compatibility.check(harness.data_dir, release)
                    check('deployment capability gate accepts an explicitly compatible reader', True)
                    report['status'] = 'passed'
                finally:
                    harness.close()
    except Exception as error:
        failed = True
        report['status'] = 'failed'
        report['failure_phase'] = phase
        report['error_type'] = type(error).__name__
        # Do not print the exception message: urllib, SQLite or JSON errors may
        # contain sensitive request, response, path or transport values.
        print('FAIL Weixin safety acceptance; consult sanitized phase and error type', flush=True)
    report['finished_at'] = acceptance.utc_now()
    report['passed'] = len(report['checks'])
    if args.report:
        destination = pathlib.Path(args.report).resolve()
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
    print(json.dumps({'status': report['status'], 'passed': report['passed'],
                      'real_qr_fetch_tested': report['real_qr_fetch_tested'],
                      'real_weixin_private_chat_tested': False}))
    return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
