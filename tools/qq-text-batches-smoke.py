#!/usr/bin/env python3
"""Signed QQ text-batch acceptance against a real isolated daemon.

Synthetic Webhook events cross signature validation, durable reception, model
HTTP and Outbox creation. A durable pacing fence blocks all QQ dispatch before
the synthetic account is enabled. This does not access QQ, send a real message,
or prove real gateway/client behavior. Only a disposable database is mutated.
"""
import argparse
import copy
import hashlib
import http.server
import importlib.util
import json
import os
import pathlib
import platform
import secrets
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid


sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    'qq_batch_harness', pathlib.Path(__file__).with_name('memory-smoke.py'))
base = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(base)
API = base.PREFIX
ACCOUNT = '987654321'
PACING_FENCE_MS = 9_000_000_000_000_000


def openssl_path(explicit=None):
    candidates = [explicit, shutil.which('openssl')]
    if os.name == 'nt':
        candidates.extend([
            r'C:\Program Files\Git\usr\bin\openssl.exe',
            r'C:\Program Files\Git\mingw64\bin\openssl.exe',
        ])
    for candidate in candidates:
        if candidate and pathlib.Path(candidate).is_file():
            return str(pathlib.Path(candidate).resolve())
    raise RuntimeError('OpenSSL with Ed25519 support is required for synthetic Webhook signatures')


class Signer:
    def __init__(self, executable, directory, secret):
        self.executable = executable
        self.key = pathlib.Path(directory) / 'synthetic-webhook-key.der'
        self.input = pathlib.Path(directory) / 'synthetic-webhook-input.bin'
        source = secret.encode('ascii')
        seed = (source * ((32 + len(source) - 1) // len(source)))[:32]
        self.key.write_bytes(bytes.fromhex('302e020100300506032b657004220420') + seed)
        if os.name != 'nt':
            self.key.chmod(0o600)

    def sign(self, timestamp, body):
        self.input.write_bytes(timestamp.encode('ascii') + body)
        result = subprocess.run([
            self.executable, 'pkeyutl', '-sign', '-rawin', '-inkey', str(self.key),
            '-keyform', 'DER', '-in', str(self.input),
        ], capture_output=True, check=False, timeout=10)
        if result.returncode or len(result.stdout) != 64:
            raise RuntimeError('synthetic Webhook signing failed')
        return result.stdout.hex()


class Model(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        size = int(self.headers.get('Content-Length', '0'))
        if not 0 < size <= 1024 * 1024:
            self.send_error(413)
            return
        payload = json.loads(self.rfile.read(size))
        mock = self.server.mock
        question = mock.question(payload)
        with mock.condition:
            mock.requests.append(payload)
            mock.condition.notify_all()
        held = question == mock.hold_question
        if held:
            mock.hold_received.set()
            mock.hold_release.wait(timeout=25)
        raw = json.dumps({'choices': [{'message': {
            'role': 'assistant', 'content': 'SYNTHETIC_BATCH_ACK',
        }}], 'usage': {'prompt_tokens': 8, 'completion_tokens': 8}}).encode()
        try:
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass
        finally:
            if held:
                mock.hold_responded.set()


def rows(harness, sql, arguments=()):
    with harness.db() as database:
        return [dict(row) for row in database.execute(sql, arguments)]


def wait_for(predicate, label, timeout=20):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.025)
    raise TimeoutError(label)


def apply_config(harness, config):
    current, _, _ = harness.request('GET', API + '/config')
    draft, _, _ = harness.request('POST', API + '/config/drafts', {
        'base_revision': current['revision'], 'config': config})
    route = API + '/config/drafts/' + draft['id']
    validation, _, _ = harness.request('POST', route + '/validate', {})
    if not validation['valid']:
        raise AssertionError('synthetic channel configuration must validate')
    harness.request('POST', route + '/apply', {}, expected=202,
                    headers={'If-Match': '"' + str(current['revision']) + '"'})
    applied, _, _ = harness.request('GET', API + '/config')
    harness.applied = applied['config']


def webhook(harness, signer, event, expected=200, invalid_signature=False):
    raw = json.dumps(event, ensure_ascii=False, separators=(',', ':')).encode('utf-8')
    timestamp = str(int(time.time()))
    signature = '00' * 64 if invalid_signature else signer.sign(timestamp, raw)
    request = urllib.request.Request(harness.base + '/webhooks/qq', raw, {
        'Content-Type': 'application/json', 'X-Bot-Appid': ACCOUNT,
        'X-Signature-Timestamp': timestamp, 'X-Signature-Ed25519': signature,
    }, method='POST')
    try:
        response = harness.anonymous.open(request, timeout=12)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        code, result = response.code, response.read()
    if code != expected:
        raise AssertionError(f'synthetic signed Webhook: expected HTTP {expected}, got {code}')
    if expected == 200 and json.loads(result) != {'op': 12, 'd': 0}:
        raise AssertionError('signed Webhook must acknowledge durable receipt')


def event(user, content, attachments=None):
    value = {'op': 0, 'id': 'event_' + uuid.uuid4().hex, 't': 'C2C_MESSAGE_CREATE',
             'd': {'id': 'message_' + uuid.uuid4().hex, 'content': content,
                   'timestamp': base.utc_now(), 'author': {'user_openid': user}}}
    if attachments is not None:
        value['d']['attachments'] = attachments
    return value


def mapping(harness, value):
    key = 'qq:' + ACCOUNT + ':C2C_MESSAGE_CREATE:' + value['id']
    found = rows(harness, 'SELECT run_id FROM inbox WHERE event_key=?', (key,))
    return found[0]['run_id'] if found else None


def received_runs(harness, values):
    return [wait_for(lambda value=value: mapping(harness, value),
                     'acknowledged synthetic event did not reach Inbox') for value in values]


def model_result(harness, run):
    result = harness.wait_run(run, via_api=False)
    if result['status'] != 'completed':
        raise AssertionError('synthetic QQ model run must complete')
    return rows(harness, 'SELECT * FROM runs WHERE id=?', (run,))[0]


def one_outbox(harness, run):
    found = rows(harness, 'SELECT * FROM outbox WHERE run_id=?', (run,))
    return len(found) == 1 and found[0]['status'] == 'queued'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report', required=True)
    parser.add_argument('--openssl')
    args = parser.parse_args()
    binary = str(pathlib.Path(args.binary).resolve(strict=True))
    web = str(pathlib.Path(args.web_dir).resolve(strict=True))
    crypto = openssl_path(args.openssl)
    report = {
        'kind': 'QQ signed synthetic Webhook text batches through real daemon and model HTTP to Outbox',
        'status': 'running', 'started_at': base.utc_now(), 'checks': [], 'commit': None,
        'binary_sha256': hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest(),
        'architecture': platform.machine(), 'production_data_accessed': False,
        'signed_webhook_tested': False, 'mock_model_http_tested': False,
        'real_qq_tested': False, 'qq_outbound_http_tested': False,
        'real_gateway_tested': False, 'real_external_model_tested': False,
        'outbound_safety': 'synthetic account is durably fenced by reply_account_pacing before enabling QQ',
    }
    stage, code = 'setup', 0

    def check(name, condition=True):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    mock = base.MockState()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
    server.daemon_threads = True
    server.mock = mock
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-qq-text-batches-') as temporary:
            h = base.Harness(binary, web, temporary, mock)
            try:
                h.start()
                h.bootstrap()
                h.configure(server.server_port)
                report['commit'] = h.commit
                secret = secrets.token_urlsafe(32)
                signer = Signer(crypto, temporary, secret)
                credential, _, _ = h.request('POST', API + '/secrets', {
                    'kind': 'qq_app_secret', 'value': secret})
                # Install the fence while QQ is disabled. No test ever removes it.
                with h.db() as database:
                    database.execute('INSERT INTO reply_account_pacing(account_id,next_ms) VALUES(?,?)',
                                     (ACCOUNT, PACING_FENCE_MS))
                config = copy.deepcopy(h.applied)
                config['qq'] = {'enabled': True, 'app_id': ACCOUNT,
                                'credential_ref': credential['id'], 'transport': 'webhook'}
                apply_config(h, config)
                check('isolated synthetic webhook account is enabled only after durable outbound fencing')
                rejected = event('signature_user', 'must never enter model')
                webhook(h, signer, rejected, expected=403, invalid_signature=True)
                check('invalid signed inbound event is rejected before durable reception',
                      not rows(h, 'SELECT 1 FROM inbox'))

                suffix = secrets.token_hex(6)
                user = 'batch_' + suffix
                texts = ['BATCH_FIRST_' + suffix, 'BATCH_SECOND_' + suffix, 'BATCH_THIRD_' + suffix]
                incoming = [event(user, text) for text in texts]
                before = mock.count()
                for value in incoming:
                    webhook(h, signer, value)
                runs = received_runs(h, incoming)
                report['signed_webhook_tested'] = True
                check('three authenticated QQ events map to exactly one durable Run', len(set(runs)) == 1)
                run = runs[0]
                batch = rows(h, 'SELECT * FROM qq_text_batches WHERE run_id=?', (run,))[0]
                check('pending batch has bounded quiet time and three durable fragments',
                      batch['fragment_count'] == 3 and batch['due_ms'] <= batch['first_ms'] + 8000
                      and len(rows(h, 'SELECT 1 FROM qq_text_fragments WHERE run_id=?', (run,))) == 3)
                check('quiet period defers inference while preserving the queued batch',
                      batch['due_ms'] > int(time.time() * 1000) and mock.count() == before
                      and rows(h, 'SELECT status FROM runs WHERE id=?', (run,))[0]['status'] == 'queued')
                webhook(h, signer, incoming[1])
                alias = copy.deepcopy(incoming[1])
                alias['id'] = 'duplicate_' + uuid.uuid4().hex
                webhook(h, signer, alias)
                result = model_result(h, run)
                payload = mock.for_question(result['input'])
                check('three ordered fragments generate one actual model HTTP request and one Outbox',
                      mock.count() == before + 1 and one_outbox(h, run)
                      and all(result['input'].count(text) == 1 for text in texts)
                      and [result['input'].find(text) for text in texts] == sorted(
                          result['input'].find(text) for text in texts)
                      and payload['messages'][-1] == {'role': 'user', 'content': result['input']})
                target = json.loads(result['reply_json'])
                check('batch retains its owner config identity and latest real passive reply target',
                      result['owner_id'] == 'qq:' + ACCOUNT + ':c2c:' + user
                      and json.loads(result['config_json'])['qq']['credential_ref'] == credential['id']
                      and target['message_id'] == incoming[-1]['d']['id']
                      and target['target'] == user and target['account_id'] == ACCOUNT)
                settled_count = mock.count()
                webhook(h, signer, incoming[0])
                late_alias = copy.deepcopy(incoming[0])
                late_alias['id'] = 'late_duplicate_' + uuid.uuid4().hex
                webhook(h, signer, late_alias)
                wait_for(lambda: not rows(h, "SELECT 1 FROM qq_received_events WHERE status IN ('queued','processing')"),
                         'duplicate receipt did not settle')
                check('duplicates before and after batch completion never repeat a fragment or model request',
                      mock.count() == settled_count
                      and len(rows(h, 'SELECT 1 FROM runs WHERE owner_id=?', (result['owner_id'],))) == 1
                      and rows(h, 'SELECT fragment_count FROM qq_text_batches WHERE run_id=?', (run,))[0]['fragment_count'] == 3)

                restart_events = [event('restart_' + suffix, 'RESTART_' + str(i) + '_' + suffix) for i in range(3)]
                for value in restart_events:
                    webhook(h, signer, value)
                restart_runs = received_runs(h, restart_events)
                restart_run = restart_runs[0]
                before_restart = rows(h, 'SELECT * FROM runs WHERE id=?', (restart_run,))[0]
                check('restart fixture is an acknowledged pending batch with no dispatched model request',
                      len(set(restart_runs)) == 1 and before_restart['status'] == 'queued')
                restart_count = mock.count()
                h.stop()
                check('stopping the daemon does not dispatch the pending batch', mock.count() == restart_count)
                h.start()
                h.login()
                restarted = model_result(h, restart_run)
                check('acknowledged pending fragments survive real daemon restart and execute once',
                      restarted['input'] == before_restart['input']
                      and restarted['reply_json'] == before_restart['reply_json']
                      and restarted['config_json'] == before_restart['config_json']
                      and restarted['character_snapshot_json'] == before_restart['character_snapshot_json']
                      and mock.count() == restart_count + 1 and one_outbox(h, restart_run))

                held_event = event('inflight_' + suffix, 'HELD_MODEL_' + suffix)
                mock.hold_question = held_event['d']['content']
                mock.hold_release.clear()
                mock.hold_received.clear()
                webhook(h, signer, held_event)
                held_run = received_runs(h, [held_event])[0]
                if not mock.hold_received.wait(timeout=10):
                    raise TimeoutError('held synthetic model request did not start')
                held_payload = copy.deepcopy(mock.for_question(mock.hold_question))
                followups = [event('inflight_' + suffix, 'FOLLOWUP_' + str(i) + '_' + suffix) for i in range(3)]
                for value in followups:
                    webhook(h, signer, value)
                next_runs = received_runs(h, followups)
                next_run = next_runs[0]
                check('messages arriving during inference form a new queued batch',
                      len(set(next_runs)) == 1 and next_run != held_run
                      and rows(h, 'SELECT status FROM runs WHERE id=?', (next_run,))[0]['status'] == 'queued'
                      and mock.for_question(mock.hold_question) == held_payload)
                mock.hold_release.set()
                model_result(h, held_run)
                followup_result = model_result(h, next_run)
                check('inference completion preserves old payload and later batch executes once in order',
                      all(value['d']['content'] not in base.payload_text(held_payload) for value in followups)
                      and all(value['d']['content'] in followup_result['input'] for value in followups)
                      and one_outbox(h, held_run) and one_outbox(h, next_run))

                for label, boundary in [
                    ('memory command', '/memory list'),
                    ('unknown slash', '/synthetic_batch_boundary'),
                    ('explicit Skill', '/skill skill-creator prepare a synthetic reusable greeting'),
                    ('long text', 'LONG_BOUNDARY_' + suffix + '_' + 'x' * 257),
                ]:
                    scope = 'boundary_' + uuid.uuid4().hex
                    values = [event(scope, 'BEFORE_' + suffix), event(scope, boundary),
                              event(scope, 'AFTER_' + suffix)]
                    for value in values:
                        webhook(h, signer, value)
                    boundary_runs = received_runs(h, values)
                    check(label + ' seals adjacent text instead of joining their batch',
                          len(set(boundary_runs)) == 3)
                    for boundary_run in boundary_runs:
                        model_result(h, boundary_run)

                scope = 'media_' + suffix
                media_values = [event(scope, 'MEDIA_BEFORE_' + suffix),
                                event(scope, 'synthetic unsupported audio', [{
                                    'content_type': 'audio/ogg', 'url': 'https://example.invalid/never-fetch'}]),
                                event(scope, 'MEDIA_AFTER_' + suffix)]
                for value in media_values:
                    webhook(h, signer, value)
                media_runs = received_runs(h, media_values)
                check('unsupported media is a separate notice and seals adjacent text batches',
                      len(set(media_runs)) == 3
                      and rows(h, 'SELECT run_kind FROM runs WHERE id=?', (media_runs[1],))[0]['run_kind'] == 'media_notice')
                for media_run in media_runs:
                    model_result(h, media_run)

                first_config_event = event('config_' + suffix, 'OLD_CONFIG_' + suffix)
                webhook(h, signer, first_config_event)
                first_config_run = received_runs(h, [first_config_event])[0]
                check('configuration boundary fixture is still pending before the change',
                      rows(h, 'SELECT status FROM runs WHERE id=?', (first_config_run,))[0]['status'] == 'queued')
                previous_model = h.applied['model']['model']
                config = copy.deepcopy(h.applied)
                config['model']['model'] = 'synthetic-batch-new-model'
                apply_config(h, config)
                next_config_event = event('config_' + suffix, 'NEW_CONFIG_' + suffix)
                webhook(h, signer, next_config_event)
                next_config_run = received_runs(h, [next_config_event])[0]
                old_config = model_result(h, first_config_run)
                new_config = model_result(h, next_config_run)
                check('configuration changes split batches while each Run retains its frozen model',
                      first_config_run != next_config_run
                      and json.loads(old_config['config_json'])['model']['model'] == previous_model
                      and json.loads(new_config['config_json'])['model']['model'] == 'synthetic-batch-new-model')

                first_persona_event = event('personality_' + suffix, 'OLD_PERSONALITY_' + suffix)
                webhook(h, signer, first_persona_event)
                first_persona_run = received_runs(h, [first_persona_event])[0]
                check('personality boundary fixture is pending before the saved revision changes',
                      rows(h, 'SELECT status FROM runs WHERE id=?', (first_persona_run,))[0]['status'] == 'queued')
                current_persona, _, _ = h.request('GET', API + '/assistant/personality')
                changed_persona = copy.deepcopy(current_persona['personality'])
                changed_persona['personality'] = 'SYNTHETIC_NEW_PERSONALITY_' + suffix
                h.request('PUT', API + '/assistant/personality', {'personality': changed_persona},
                          headers={'If-Match': current_persona['etag']})
                next_persona_event = event('personality_' + suffix, 'NEW_PERSONALITY_' + suffix)
                webhook(h, signer, next_persona_event)
                next_persona_run = received_runs(h, [next_persona_event])[0]
                old_persona = model_result(h, first_persona_run)
                new_persona = model_result(h, next_persona_run)
                check('saved personality revisions split batches without rewriting the queued personality',
                      first_persona_run != next_persona_run
                      and old_persona['character_digest'] != new_persona['character_digest']
                      and changed_persona['personality'] not in old_persona['character_snapshot_json']
                      and changed_persona['personality'] in new_persona['character_snapshot_json'])

                capped_events = [event('cap_' + suffix, 'CAP_' + str(i) + '_' + suffix) for i in range(9)]
                for value in capped_events:
                    webhook(h, signer, value)
                capped_runs = received_runs(h, capped_events)
                check('eight-fragment cap creates a new batch for the ninth short message',
                      len(set(capped_runs[:8])) == 1 and capped_runs[8] != capped_runs[0])
                for capped_run in set(capped_runs):
                    model_result(h, capped_run)

                owners = [event('owner_a_' + suffix, 'OWNER_A_' + suffix),
                          event('owner_b_' + suffix, 'OWNER_B_' + suffix)]
                for value in owners:
                    webhook(h, signer, value)
                owner_runs = received_runs(h, owners)
                first_owner, second_owner = [model_result(h, owner_run) for owner_run in owner_runs]
                check('different QQ source identities never share a batch or private model input',
                      owner_runs[0] != owner_runs[1]
                      and first_owner['owner_id'] != second_owner['owner_id']
                      and owners[1]['d']['content'] not in base.payload_text(mock.for_question(first_owner['input']))
                      and owners[0]['d']['content'] not in base.payload_text(mock.for_question(second_owner['input'])))
                fence = rows(h, 'SELECT next_ms FROM reply_account_pacing WHERE account_id=?', (ACCOUNT,))
                check('all QQ Outboxes remain queued behind the original fence without any dispatch attempt',
                      fence[0]['next_ms'] == PACING_FENCE_MS
                      and not rows(h, "SELECT 1 FROM outbox WHERE status!='queued'")
                      and not rows(h, "SELECT 1 FROM outbox_reply_parts WHERE status!='queued'"))
                check('synthetic control boundaries never create long-term memory',
                      not rows(h, 'SELECT 1 FROM memory_records'))
                report['status'] = 'passed'
            finally:
                h.close()
    except Exception as error:
        code = 1
        report.update(status='failed', failed_phase=stage, error_type=type(error).__name__)
        print('FAIL ' + stage + ': ' + type(error).__name__, file=sys.stderr)
    finally:
        server.shutdown()
        server.server_close()
        report['finished_at'] = base.utc_now()
        report['passed'] = len(report['checks'])
        report['mock_model_http_tested'] = mock.count() > 0
        output = pathlib.Path(args.report)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    return code


if __name__ == '__main__':
    raise SystemExit(main())
