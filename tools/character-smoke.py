#!/usr/bin/env python3
"""Character-card black-box acceptance with a real Rust service and local fixtures.

Only a fresh temporary database, synthetic administrator/key, and loopback mock
model are used. This never proves real QQ or external-model connectivity. It
does not reuse or import the ordinary M1 smoke script.
"""
import argparse
import contextlib
import copy
import datetime
import hashlib
import http.cookiejar
import http.server
import io
import json
import os
import pathlib
import secrets
import socket
import sqlite3
import struct
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import zipfile


API = '/api/admin/v1'
CARDS = API + '/character-cards'


def free_port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def card(name, marker):
    return {
        'spec': 'chara_card_v3', 'spec_version': '3.0',
        'data': {
            'name': name, 'description': marker, 'personality': 'Patient and precise.',
            'scenario': 'An isolated acceptance test.', 'first_mes': 'DISPLAY_ONLY_GREETING',
            'mes_example': '<START>\n{{user}}: A sample?\n{{char}}: A concise sample.',
            'creator_notes': 'METADATA_ONLY_NOTE', 'system_prompt': 'Use the selected identity.',
            'post_history_instructions': 'INACTIVE_POST_HISTORY',
            'alternate_greetings': [], 'group_only_greetings': [], 'tags': ['synthetic'],
            'creator': 'DSH acceptance', 'character_version': '1.0.0',
            'extensions': {'dsh': {'schema_version': 1, 'modules': {
                'chat': {'mode': 'inherit'}, 'vision': {'mode': 'disabled'},
                'speech': {'mode': 'disabled'},
            }}},
        },
    }


def airi_zip(value, extra=(), duplicate=False):
    output = io.BytesIO()
    with zipfile.ZipFile(output, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
        archive.writestr('manifest.json', json.dumps({
            'format': 'airi-character-card', 'version': 1,
            'card': {'path': 'card.json', 'spec': 'chara_card_v3'},
        }))
        archive.writestr('card.json', json.dumps(value))
        if duplicate:
            archive.writestr('card.json', json.dumps(value))
        for name, content in extra:
            # ZipInfo normalizes Windows separators on construction. Restore
            # deliberately hostile names so the fixture tests the raw archive.
            info = zipfile.ZipInfo(name)
            info.filename = name
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, content)
    return output.getvalue()


class MockProvider(http.server.BaseHTTPRequestHandler):
    seen = []
    lock = threading.Lock()
    first_started = threading.Event()
    release_first = threading.Event()
    resource_requests = 0
    snapshot_probe = None
    snapshots_at_dispatch = {}

    def log_message(self, *_):
        pass

    def resource_response(self):
        if self.path.split('?', 1)[0] == '/must-not-fetch':
            with self.lock:
                type(self).resource_requests += 1
        self.send_response(404)
        self.end_headers()

    do_GET = resource_response
    do_HEAD = resource_response
    do_PUT = resource_response
    do_PATCH = resource_response
    do_DELETE = resource_response
    do_OPTIONS = resource_response

    def do_POST(self):
        if self.path.split('?', 1)[0] == '/must-not-fetch':
            self.resource_response()
            return
        value = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        text = next(m['content'] for m in reversed(value['messages']) if m['role'] == 'user')
        probe = type(self).snapshot_probe
        snapshot_exists = probe(text) if probe is not None else False
        with self.lock:
            self.seen.append(value)
            self.snapshots_at_dispatch[text] = snapshot_exists
        if text == 'queue-first':
            self.first_started.set()
            self.release_first.wait(timeout=35)
        payload = json.dumps({'choices': [{'message': {
            'role': 'assistant', 'content': 'fixture-reply:' + text,
        }}], 'usage': {'prompt_tokens': 5, 'completion_tokens': 5}}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        try:
            self.wfile.write(payload)
        except (BrokenPipeError, ConnectionResetError):
            pass


class Acceptance:
    def __init__(self, binary, web_dir, temp):
        self.binary = str(pathlib.Path(binary).resolve())
        with open(self.binary, 'rb') as executable:
            self.binary_sha256 = hashlib.file_digest(executable, 'sha256').hexdigest()
        self.web_dir = str(pathlib.Path(web_dir).resolve())
        self.data_dir = pathlib.Path(temp) / 'data'
        self.base = 'http://127.0.0.1:' + str(free_port())
        self.jar = http.cookiejar.CookieJar()
        self.client = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.jar))
        self.csrf = None
        self.process = None
        self.log = open(pathlib.Path(temp) / 'service.log', 'ab')
        self.provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), MockProvider)
        MockProvider.snapshot_probe = self.snapshot_exists_at_dispatch
        self.provider.daemon_threads = True
        threading.Thread(target=self.provider.serve_forever, daemon=True).start()
        self.checks = []
        self.commit = None
        self.password = secrets.token_urlsafe(24)
        self.secret = secrets.token_urlsafe(32)

    def check(self, name, condition):
        if not condition:
            raise AssertionError(name)
        self.checks.append(name)
        print('PASS ' + name, flush=True)

    @contextlib.contextmanager
    def database(self):
        connection = sqlite3.connect(self.data_dir / 'dsh.db')
        try:
            with connection:
                yield connection
        finally:
            connection.close()

    def snapshot_exists_at_dispatch(self, text):
        try:
            with self.database() as database:
                count = database.execute(
                    'SELECT count(*) FROM run_prompt_snapshots s JOIN runs r ON r.id=s.run_id '
                    'WHERE r.input=? AND r.status=?', (text, 'running')).fetchone()[0]
            return count == 1
        except sqlite3.Error:
            return False

    def request(self, method, path, value=None, *, expected=200, headers=None,
                raw=None, content_type='application/json', csrf=True):
        request_headers = {'Origin': self.base, 'Content-Type': content_type}
        if csrf and self.csrf:
            request_headers['X-CSRF-Token'] = self.csrf
        if headers:
            request_headers.update(headers)
        data = raw if raw is not None else (None if value is None else json.dumps(value).encode())
        request = urllib.request.Request(self.base + path, data, request_headers, method=method)
        try:
            response = self.client.open(request, timeout=12)
        except urllib.error.HTTPError as error:
            response = error
        payload = response.read()
        acceptable = (expected,) if isinstance(expected, int) else expected
        if response.code not in acceptable:
            raise AssertionError(f'{method} {path}: expected {acceptable}, got {response.code}')
        try:
            decoded = json.loads(payload)
        except (json.JSONDecodeError, UnicodeDecodeError):
            decoded = None
        return decoded, payload

    def start(self):
        self.process = subprocess.Popen([
            self.binary, 'serve', '--data-dir', str(self.data_dir),
            '--listen', self.base.replace('http://', ''), '--origin', self.base,
            '--allow-private-model',
        ], env=dict(os.environ, DSH_WEB_DIR=self.web_dir, RUST_LOG='warn'),
            stdout=self.log, stderr=self.log)
        for _ in range(120):
            if self.process.poll() is not None:
                raise RuntimeError('isolated Rust service exited before readiness')
            try:
                health, _ = self.request('GET', '/health/ready')
                self.commit = health.get('commit')
                return
            except (OSError, AssertionError):
                time.sleep(0.1)
        raise TimeoutError('isolated Rust service did not become ready')

    def stop(self):
        if self.process and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)

    def close(self):
        MockProvider.release_first.set()
        self.stop()
        self.provider.shutdown()
        self.provider.server_close()
        self.log.close()

    def login(self, username='character-smoke'):
        result, _ = self.request('POST', API + '/auth/login', {
            'username': username, 'password': self.password,
        })
        self.csrf = result['csrf_token']

    def bootstrap_and_configure(self):
        self.request('GET', CARDS, expected=401)
        self.check('character management requires authentication', True)
        subprocess.run([self.binary, 'bootstrap-token', '--data-dir', str(self.data_dir)],
                       check=True, stdout=subprocess.DEVNULL)
        token = json.loads((self.data_dir / 'bootstrap-token.json').read_text())['token']
        result, _ = self.request('POST', API + '/auth/bootstrap', {
            'username': 'character-smoke', 'password': self.password, 'bootstrap_token': token,
        })
        self.csrf = result['csrf_token']
        self.request('POST', CARDS, {'card': card('CSRF', 'NO_WRITE')}, expected=403, csrf=False)
        self.check('character mutations require CSRF protection', True)
        metadata, _ = self.request('POST', API + '/secrets', {
            'kind': 'model_api_key', 'value': self.secret,
        })
        self.secret_ref = metadata['id']
        self.apply_model('fixture-model-before')

    def apply_model(self, model):
        result, _ = self.request('GET', API + '/config')
        config = copy.deepcopy(result['config'])
        config['model'] = {
            'base_url': 'http://127.0.0.1:' + str(self.provider.server_port),
            'model': model, 'credential_ref': self.secret_ref,
        }
        draft, _ = self.request('POST', API + '/config/drafts', {
            'base_revision': result['revision'], 'config': config,
        })
        path = API + '/config/drafts/' + draft['id']
        self.request('POST', path + '/apply', {}, expected=202,
                     headers={'If-Match': '"' + str(result['revision']) + '"'})
        for _ in range(60):
            current, _ = self.request('GET', API + '/config')
            if current['config']['model']['model'] == model:
                return
            time.sleep(0.05)
        raise TimeoutError('model fixture configuration did not apply')

    @staticmethod
    def draft_path(draft):
        return CARDS + '/' + draft['card_id'] + '/drafts/' + draft['draft_id']

    def new_draft(self, value):
        result, _ = self.request('POST', CARDS, {'card': value})
        return result

    def save(self, draft, value):
        updated, _ = self.request('PUT', self.draft_path(draft), {'card': value},
                                  headers={'If-Match': draft['draft_etag']})
        return updated

    def publish(self, draft, key=None):
        key = key or secrets.token_hex(12)
        result, _ = self.request('POST', self.draft_path(draft) + '/publish', {},
                                 headers={'If-Match': draft['draft_etag'], 'Idempotency-Key': key})
        return result, key

    def list_cards(self):
        return self.request('GET', CARDS)[0]

    def set_default(self, published):
        current = self.list_cards()
        return self.request('PUT', CARDS + '/default', {
            'card_id': published['card_id'], 'revision': published['revision'],
        }, headers={'If-Match': current['default_etag']})[0]

    def conversation(self, title):
        return self.request('POST', API + '/conversations', {'title': title})[0]

    def send(self, conversation, text):
        return self.request('POST', API + '/conversations/' + conversation['id'] + '/messages', {
            'content': text, 'client_message_id': secrets.token_hex(12),
        }, expected=202)[0]['run_id']

    def wait_run(self, run_id):
        for _ in range(180):
            value, _ = self.request('GET', API + '/runs/' + run_id)
            if value['status'] in ('completed', 'failed', 'cancelled', 'interrupted'):
                if value['status'] != 'completed':
                    raise AssertionError('synthetic character run did not complete')
                return value
            time.sleep(0.1)
        raise TimeoutError('synthetic character run did not settle')

    @staticmethod
    def observed(text):
        with MockProvider.lock:
            matches = [value for value in MockProvider.seen if next(
                (message.get('content') for message in reversed(value.get('messages', []))
                 if message.get('role') == 'user'), None) == text]
        return matches[-1] if matches else None

    @staticmethod
    def system(value):
        return '\n'.join(message.get('content', '') for message in value['messages']
                         if message.get('role') == 'system')

    def test_cards(self):
        original = card('Card Alpha', 'IDENTITY_ALPHA_REV1')
        draft = self.new_draft(original)
        before = self.list_cards()
        self.check('creating a draft does not select a default', before['default'] is None)
        validation, _ = self.request('POST', self.draft_path(draft) + '/validate', {})
        self.check('supported text card is valid and bindable', validation['valid'] and validation['bindable'])
        preview, _ = self.request('POST', self.draft_path(draft) + '/preview', {})
        self.check('preview distinguishes runtime text from inactive metadata',
                   'IDENTITY_ALPHA_REV1' in preview['system_prompt']
                   and all(marker not in preview['system_prompt'] for marker in (
                       'METADATA_ONLY_NOTE', 'DISPLAY_ONLY_GREETING', 'INACTIVE_POST_HISTORY')))
        stale = copy.deepcopy(original)
        stale['data']['description'] = 'EDIT_WITH_OLD_ETAG'
        changed = self.save(draft, stale)
        self.request('PUT', self.draft_path(draft), {'card': original}, expected=409,
                     headers={'If-Match': draft['draft_etag']})
        self.check('stale draft ETag cannot overwrite a newer edit', True)
        draft = self.save(changed, original)
        revision1, key = self.publish(draft)
        again, _ = self.publish(draft, key)
        self.check('publish retry is idempotent', revision1 == again)
        self.request('POST', self.draft_path(draft) + '/publish', {}, expected=409,
                     headers={'If-Match': revision1['draft_etag'], 'Idempotency-Key': key})
        self.check('publish idempotency key cannot be reused for a different revision intent', True)
        self.check('publishing does not change the default', self.list_cards()['default'] is None)
        self.set_default(revision1)
        old = self.conversation('Bound to Alpha revision 1')
        self.wait_run(self.send(old, 'old-before-edit'))
        self.check('Web conversation uses the published default identity',
                   'IDENTITY_ALPHA_REV1' in self.system(self.observed('old-before-edit')))

        changed_value = copy.deepcopy(original)
        changed_value['data']['description'] = 'IDENTITY_ALPHA_REV2'
        changed, _ = self.request('POST', CARDS + '/' + revision1['card_id'] + '/drafts', {
            'revision': revision1['revision'],
        })
        changed = self.save(changed, changed_value)
        self.wait_run(self.send(old, 'old-after-draft-save'))
        self.check('saving a draft leaves bound conversations unchanged',
                   'IDENTITY_ALPHA_REV1' in self.system(self.observed('old-after-draft-save'))
                   and 'IDENTITY_ALPHA_REV2' not in self.system(self.observed('old-after-draft-save')))
        revision2, _ = self.publish(changed)
        default = self.list_cards()['default']
        self.check('new publication preserves the precise older default revision',
                   default['card_id'] == revision1['card_id'] and default['revision'] == revision1['revision'])
        inherited_old = self.conversation('New conversation before changing default')
        self.wait_run(self.send(inherited_old, 'new-before-default-change'))
        self.check('new conversations retain the explicitly selected default revision',
                   'IDENTITY_ALPHA_REV1' in self.system(self.observed('new-before-default-change')))
        self.set_default(revision2)
        new = self.conversation('Bound to Alpha revision 2')
        self.wait_run(self.send(new, 'new-after-default-change'))
        self.wait_run(self.send(old, 'old-after-default-change'))
        self.check('changing default affects only newly created conversations',
                   'IDENTITY_ALPHA_REV2' in self.system(self.observed('new-after-default-change'))
                   and 'IDENTITY_ALPHA_REV1' in self.system(self.observed('old-after-default-change')))

        exported, raw = self.request('GET', CARDS + '/' + revision2['card_id']
                                     + '/revisions/' + str(revision2['revision']) + '/export')
        forbidden = (self.secret.encode(), self.secret_ref.encode(), b'credential_ref',
                     b'base_url', b'127.0.0.1', b'old-after-default-change')
        self.check('export contains the portable card without credentials or conversation data',
                   exported['spec'] == 'chara_card_v3'
                   and exported['data']['description'] == 'IDENTITY_ALPHA_REV2'
                   and not any(value in raw for value in forbidden))
        cli = subprocess.run([self.binary, 'chat', '--url', self.base, '--username',
                              'character-smoke', '--message', 'cli-persona-default'],
                             env=dict(os.environ, DSH_ADMIN_PASSWORD=self.password),
                             capture_output=True, text=True, timeout=20)
        observed = self.observed('cli-persona-default')
        self.check('authenticated CLI uses the same character default pipeline',
                   cli.returncode == 0 and 'fixture-reply:cli-persona-default' in cli.stdout
                   and observed is not None and 'IDENTITY_ALPHA_REV2' in self.system(observed))
        explicit_cli = subprocess.run([
            self.binary, 'chat', '--url', self.base, '--username', 'character-smoke',
            '--character-card', revision1['card_id'], '--character-revision', str(revision1['revision']),
            '--message', 'cli-persona-explicit',
        ], env=dict(os.environ, DSH_ADMIN_PASSWORD=self.password), capture_output=True,
            text=True, timeout=20)
        explicit_seen = self.observed('cli-persona-explicit')
        self.check('CLI can create a conversation with an explicit published revision',
                   explicit_cli.returncode == 0 and explicit_seen is not None
                   and 'IDENTITY_ALPHA_REV1' in self.system(explicit_seen)
                   and 'IDENTITY_ALPHA_REV2' not in self.system(explicit_seen))
        self.old_conversation = old
        self.current_revision = revision2

    def test_queue_and_restart(self):
        first = self.send(self.old_conversation, 'queue-first')
        if not MockProvider.first_started.wait(timeout=12):
            raise TimeoutError('first fixture model request did not enter its gate')
        second = self.send(self.old_conversation, 'queue-second')
        second_state, public_run = self.request('GET', API + '/runs/' + second)
        self.check('same-conversation second run remains queued behind the first',
                   second_state['status'] == 'queued')
        self.check('ordinary run status does not expose configuration or prompt snapshots',
                   not any(value in public_run for value in (
                       self.secret.encode(), self.secret_ref.encode(), b'config_json',
                       b'character_snapshot_json', b'messages_json')))
        self.apply_model('fixture-model-after')
        MockProvider.release_first.set()
        self.wait_run(first)
        self.wait_run(second)
        first_seen = self.observed('queue-first')
        second_seen = self.observed('queue-second')
        self.check('queued run freezes identity and model configuration before later changes',
                   second_seen['model'] == 'fixture-model-before'
                   and 'IDENTITY_ALPHA_REV1' in self.system(second_seen)
                   and 'IDENTITY_ALPHA_REV2' not in self.system(second_seen))
        self.check('execution history includes the previous completed reply but no future input',
                   any(m['role'] == 'assistant' and m['content'] == 'fixture-reply:queue-first'
                       for m in second_seen['messages'])
                   and not any(m.get('content') == 'queue-second' for m in first_seen['messages']))
        with self.database() as database:
            columns = {row[1] for row in database.execute('PRAGMA table_info(runs)')}
            self.check('run schema persists character snapshots', 'character_snapshot_json' in columns)
            snapshot = database.execute(
                'SELECT character_snapshot_json FROM runs WHERE id=?', (second,)).fetchone()[0]
            self.check('persistent run snapshot contains the original static identity',
                       snapshot is not None and 'IDENTITY_ALPHA_REV1' in snapshot
                       and 'IDENTITY_ALPHA_REV2' not in snapshot and self.secret not in snapshot)
            prompt = database.execute(
                'SELECT messages_json FROM run_prompt_snapshots WHERE run_id=?', (second,)).fetchone()
            self.check('provider observes an already-persisted complete prompt snapshot on receipt',
                       prompt is not None and 'fixture-reply:queue-first' in prompt[0]
                       and MockProvider.snapshots_at_dispatch.get('queue-first') is True
                       and MockProvider.snapshots_at_dispatch.get('queue-second') is True)
        self.stop()
        self.start()
        self.login()
        self.wait_run(self.send(self.old_conversation, 'after-process-restart'))
        seen = self.observed('after-process-restart')
        self.check('restart preserves the older conversation binding and newer model config',
                   'IDENTITY_ALPHA_REV1' in self.system(seen) and seen['model'] == 'fixture-model-after')
        current = self.list_cards()['default']
        self.check('restart preserves the exact published default reference',
                   current['card_id'] == self.current_revision['card_id']
                   and current['revision'] == self.current_revision['revision'])

    def test_imports(self):
        before = len(self.list_cards()['items'])
        portable = card('Imported text', 'IMPORTED_JSON_IDENTITY')
        portable['data']['assets'] = [{
            'type': 'icon', 'name': 'untrusted-remote-resource', 'ext': 'png',
            'uri': 'http://127.0.0.1:' + str(self.provider.server_port) + '/must-not-fetch',
        }]
        inspected, _ = self.request('POST', CARDS + '/import/inspect', raw=json.dumps(portable).encode())
        self.check('JSON inspection previews conversion without creating a card',
                   len(self.list_cards()['items']) == before
                   and inspected['card']['data']['description'] == 'IMPORTED_JSON_IDENTITY')
        self.check('inspection drops unsupported resource references without fetching them',
                   'assets' not in inspected['card']['data'] and MockProvider.resource_requests == 0
                   and any('assets' in path for path in inspected['dropped_fields']))
        # An offline local administrator fixture exercises owner scoping without
        # exposing account-creation APIs or using any production identity.
        with self.database() as database:
            database.execute(
                'INSERT INTO admins(id,username,password_hash,created_at) '
                'SELECT ?,?,password_hash,created_at FROM admins WHERE username=?',
                (secrets.token_hex(16), 'character-smoke-secondary', 'character-smoke'))
        previous_client, previous_jar, previous_csrf = self.client, self.jar, self.csrf
        self.jar = http.cookiejar.CookieJar()
        self.client = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.jar))
        self.csrf = None
        try:
            self.login('character-smoke-secondary')
            self.request('POST', CARDS + '/import/' + inspected['import_id'] + '/confirm', {
                'card': inspected['card'], 'digest': inspected['digest'],
            }, expected=(403, 404))
            self.check('import confirmation is scoped to the inspecting administrator', True)
        finally:
            self.client, self.jar, self.csrf = previous_client, previous_jar, previous_csrf
        self.request('POST', CARDS + '/import/' + inspected['import_id'] + '/confirm', {
            'card': inspected['card'], 'digest': '0' * 64,
        }, expected=409)
        self.check('import confirmation is bound to the inspected digest', True)
        confirmed, _ = self.request('POST', CARDS + '/import/' + inspected['import_id'] + '/confirm', {
            'card': inspected['card'], 'digest': inspected['digest'],
        })
        self.check('confirmed import creates only a draft and preserves the existing default',
                   len(self.list_cards()['items']) == before + 1 and 'draft_id' in confirmed
                   and self.list_cards()['default']['card_id'] == self.current_revision['card_id'])

        imported = card('AIRI package fixture', 'IMPORTED_ZIP_IDENTITY')
        imported['data']['extensions'] = {'airi': {'modules': {
            'consciousness': {'provider': '', 'model': ''},
            'vision': {'provider': '', 'model': ''},
            'speech': {'provider': '', 'model': '', 'voice_id': ''},
        }, 'agents': {'unsafe-fixture': {'prompt': 'DO_NOT_IMPORT_AGENT'}}}}
        inspected_zip, _ = self.request('POST', CARDS + '/import/inspect',
                                         raw=airi_zip(imported), content_type='application/zip')
        self.check('AIRI ZIP uses an explicit whitelist and reports unsupported extensions',
                   inspected_zip['card']['data']['description'] == 'IMPORTED_ZIP_IDENTITY'
                   and 'airi' not in inspected_zip['card']['data']['extensions']
                   and bool(inspected_zip['dropped_fields']))
        with self.database() as database:
            database.execute('UPDATE character_imports SET expires_at=0 WHERE id=?',
                             (inspected_zip['import_id'],))
        expired, _ = self.request('POST', CARDS + '/import/' + inspected_zip['import_id'] + '/confirm', {
            'card': inspected_zip['card'], 'digest': inspected_zip['digest'],
        }, expected=(404, 410, 422))
        self.check('expired import confirmation cannot create a draft',
                   expired['error']['code'] in ('character_import_expired', 'character_not_found', 'not_found'))
        bad_inputs = [
            ('path traversal', airi_zip(imported, [('../escape.json', '{}')])),
            ('absolute path', airi_zip(imported, [('/escape.json', '{}')])),
            ('drive path', airi_zip(imported, [('C:/escape.json', '{}')])),
            ('ambiguous backslash path', airi_zip(imported, [('dir\\escape.json', '{}')])),
            ('case-insensitive path collision', airi_zip(imported, [('CARD.JSON', '{}')])),
            ('high compression ratio', airi_zip(imported, [('payload.bin', b'A' * 400000)])),
            ('too many entries', airi_zip(imported, [(f'item-{i}.bin', b'x') for i in range(65)])),
            ('invalid archive', b'PK\x03\x04not-a-valid-archive'),
        ]
        for label, payload in bad_inputs:
            self.request('POST', CARDS + '/import/inspect', raw=payload,
                         content_type='application/zip', expected=(400, 413, 415, 422))
            self.check('unsafe ZIP rejected: ' + label, True)
        # Construct duplicate names without logging zipfile's harmless fixture warning.
        import warnings
        with warnings.catch_warnings():
            warnings.simplefilter('ignore', UserWarning)
            duplicate = airi_zip(imported, duplicate=True)
        self.request('POST', CARDS + '/import/inspect', raw=duplicate,
                     content_type='application/zip', expected=(400, 422))
        self.check('unsafe ZIP rejected: duplicate normalized entry', True)
        symbolic = io.BytesIO()
        with zipfile.ZipFile(symbolic, 'w') as archive:
            archive.writestr('manifest.json', json.dumps({
                'format': 'airi-character-card', 'version': 1,
                'card': {'path': 'card.json', 'spec': 'chara_card_v3'},
            }))
            archive.writestr('card.json', json.dumps(imported))
            link = zipfile.ZipInfo('external-link')
            link.create_system = 3
            link.external_attr = 0o120777 << 16
            archive.writestr(link, '../outside')
        self.request('POST', CARDS + '/import/inspect', raw=symbolic.getvalue(),
                     content_type='application/zip', expected=(400, 422))
        self.check('unsafe ZIP rejected: symbolic link', True)
        stored = io.BytesIO()
        with zipfile.ZipFile(stored, 'w', compression=zipfile.ZIP_STORED) as archive:
            archive.writestr('manifest.json', json.dumps({
                'format': 'airi-character-card', 'version': 1,
                'card': {'path': 'card.json', 'spec': 'chara_card_v3'},
            }))
            archive.writestr('card.json', json.dumps(imported))
            card_info = archive.getinfo('card.json')
        damaged = bytearray(stored.getvalue())
        name_length, extra_length = struct.unpack_from('<HH', damaged, card_info.header_offset + 26)
        body_offset = card_info.header_offset + 30 + name_length + extra_length
        # Keep JSON structurally intact while invalidating the stored CRC.
        marker_at = damaged.find(b'IMPORTED_ZIP_IDENTITY', body_offset)
        if marker_at < 0:
            raise AssertionError('CRC fixture marker missing')
        damaged[marker_at] = ord('X')
        self.request('POST', CARDS + '/import/inspect', raw=bytes(damaged),
                     content_type='application/zip', expected=(400, 422))
        self.check('unsafe ZIP rejected: CRC mismatch in a read JSON entry', True)
        self.request('POST', CARDS + '/import/inspect', raw=b' ' * (256 * 1024 + 1), expected=413)
        self.check('import JSON has a hard request-size bound', True)
        self.request('POST', CARDS + '/import/inspect', raw=b'x' * (8 * 1024 * 1024 + 1),
                     content_type='application/zip', expected=413)
        self.check('import ZIP has a hard compressed-body bound', True)
        deep_json = b'{"spec":"chara_card_v3","spec_version":"3.0","extra":' + b'[' * 35 + b'0' + b']' * 35 + b'}'
        self.request('POST', CARDS + '/import/inspect', raw=deep_json, expected=(400, 422))
        self.check('deeply nested external JSON is rejected', True)
        self.request('POST', CARDS + '/import/inspect', raw=b'{' + b'"spec":"chara_card_v3","spec":"other"}', expected=(400, 422))
        self.check('duplicate JSON keys are rejected', True)
        secret_card = copy.deepcopy(portable)
        secret_card['data']['extensions']['api_key'] = 'SYNTHETIC_SECRET_FIELD'
        self.request('POST', CARDS + '/import/inspect', raw=json.dumps(secret_card).encode(), expected=(400, 422))
        self.check('known credential fields are rejected during import', True)

    def test_archive_and_module_gates(self):
        alternate = self.new_draft(card('Card Beta', 'IDENTITY_BETA_REV1'))
        published, _ = self.publish(alternate)
        bound, _ = self.request('POST', API + '/conversations', {
            'title': 'Explicit Beta binding',
            'character': {'card_id': published['card_id'], 'revision': published['revision']},
        })
        self.request('PUT', CARDS + '/' + published['card_id'] + '/qq-access', {'enabled': True},
                     headers={'If-Match': str(published['revision'])})
        listed = next(item for item in self.list_cards()['items'] if item['id'] == published['card_id'])
        self.check('administrator explicitly controls whether a published card is QQ-selectable',
                   listed['qq_selectable'] is True)
        self.request('POST', CARDS + '/' + published['card_id'] + '/archive', {},
                     headers={'If-Match': str(published['revision'])})
        self.request('POST', API + '/conversations', {
            'title': 'Must reject archived card',
            'character': {'card_id': published['card_id'], 'revision': published['revision']},
        }, expected=422)
        self.check('archiving prevents new conversation bindings', True)
        self.request('POST', CARDS + '/' + published['card_id'] + '/drafts', {
            'revision': published['revision'],
        }, expected=422)
        self.check('archiving prevents creating new editable drafts', True)
        self.wait_run(self.send(bound, 'bound-before-archive'))
        self.check('archiving preserves existing immutable conversation bindings',
                   'IDENTITY_BETA_REV1' in self.system(self.observed('bound-before-archive')))
        unsupported = card('Future vision fixture', 'FUTURE_ONLY')
        unsupported['data']['extensions']['dsh']['modules']['vision'] = {'mode': 'inherit'}
        draft = self.new_draft(unsupported)
        validation, _ = self.request('POST', self.draft_path(draft) + '/validate', {})
        self.check('format validity is distinct from currently supported module bindings',
                   validation['valid'] is True and validation['bindable'] is False)
        self.request('POST', self.draft_path(draft) + '/publish', {}, expected=422,
                     headers={'If-Match': draft['draft_etag'], 'Idempotency-Key': secrets.token_hex(12)})
        self.check('unavailable vision capabilities cannot be enabled by publishing a card', True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report')
    args = parser.parse_args()
    report = {
        'kind': 'Character-card black-box acceptance with isolated local fixtures',
        'real_external_model_tested': False, 'real_qq_tested': False,
        'uses_production_data_or_credentials': False,
        'started_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
    }
    with tempfile.TemporaryDirectory(prefix='dsh-character-smoke-') as temp:
        acceptance = Acceptance(args.binary, args.web_dir, temp)
        try:
            acceptance.start()
            acceptance.bootstrap_and_configure()
            acceptance.test_cards()
            acceptance.test_queue_and_restart()
            acceptance.test_imports()
            acceptance.test_archive_and_module_gates()
            report['status'] = 'passed'
        except Exception as error:
            report['status'] = 'failed'
            report['failure'] = str(error)
            raise
        finally:
            acceptance.close()
            report.update({
                'reported_build_commit': acceptance.commit,
                'binary_sha256': acceptance.binary_sha256,
                'source_provenance_note': 'The health-reported commit alone does not identify uncommitted local source edits.',
                'checks': acceptance.checks,
                'passed': len(acceptance.checks),
                'finished_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
                'platform': os.uname().machine if hasattr(os, 'uname') else os.name,
            })
            if args.report:
                pathlib.Path(args.report).write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({'status': report['status'], 'passed': report['passed'], 'real_qq_tested': False}))


if __name__ == '__main__':
    main()
