#!/usr/bin/env python3
"""Exercise assistant memory against a real Rust daemon and a loopback mock.

All accounts, keys, databases, channel-owner fixtures and model responses are
temporary and synthetic. This is not a real QQ or external-model acceptance
test. The report contains metadata and check names, never prompts or secrets.
"""
import argparse
import contextlib
import copy
import datetime
import hashlib
import http.cookiejar
import http.server
import json
import os
import pathlib
import platform
import secrets
import socket
import sqlite3
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid


PREFIX = '/api/admin/v1'
TERMINAL = {'completed', 'failed', 'cancelled', 'interrupted', 'outcome_unknown'}


def free_port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def utc_now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


class MockState:
    def __init__(self):
        self.condition = threading.Condition()
        self.requests = []
        self.canaries = []
        self.hold_question = None
        self.hold_received = threading.Event()
        self.hold_release = threading.Event()
        self.hold_responded = threading.Event()

    def count(self):
        with self.condition:
            return len(self.requests)

    def for_question(self, question):
        with self.condition:
            matches = [payload for payload in self.requests
                       if self.question(payload) == question]
        if len(matches) != 1:
            raise AssertionError('model request must be observed exactly once')
        return matches[0]

    @staticmethod
    def question(payload):
        return next(message['content'] for message in reversed(payload['messages'])
                    if message['role'] == 'user')


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        state = self.server.mock_state
        size = int(self.headers.get('Content-Length', '0'))
        if not 0 < size <= 1024 * 1024:
            self.send_error(413)
            return
        payload = json.loads(self.rfile.read(size))
        question = state.question(payload)
        with state.condition:
            state.requests.append(payload)
            state.condition.notify_all()
        held = question == state.hold_question
        if held:
            state.hold_received.set()
            # The test releases this only after an acknowledged forget action.
            # Timeout merely prevents cleanup from leaving a blocked thread.
            state.hold_release.wait(timeout=30)
        serialized = json.dumps(payload, ensure_ascii=False)
        echoes = [canary for canary in state.canaries if canary in serialized]
        response = json.dumps({
            'choices': [{'message': {'role': 'assistant',
                                     'content': 'isolated-memory-echo:' + '|'.join(echoes)}}],
            'usage': {'prompt_tokens': 8, 'completion_tokens': 8},
        }).encode()
        try:
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(response)))
            self.end_headers()
            self.wfile.write(response)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass
        finally:
            if held:
                state.hold_responded.set()


class Harness:
    def __init__(self, binary, web_dir, temporary_root, mock):
        self.binary = binary
        self.web_dir = web_dir
        self.root = pathlib.Path(temporary_root).resolve()
        self.data_dir = self.root / 'data'
        self.database = self.data_dir / 'dsh.db'
        self.server_port = free_port()
        self.base = 'http://127.0.0.1:' + str(self.server_port)
        self.jar = http.cookiejar.CookieJar()
        self.client = urllib.request.build_opener(
            urllib.request.ProxyHandler({}), urllib.request.HTTPCookieProcessor(self.jar))
        self.anonymous = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        self.csrf = None
        self.process = None
        self.log = open(self.root / 'daemon.log', 'ab')
        self.mock = mock
        self.username = 'memory-smoke-admin'
        self.password = secrets.token_urlsafe(32)
        self.key = secrets.token_urlsafe(32)
        self.environment = {name: value for name, value in os.environ.items()
                            if not name.startswith('DSH_')}
        self.environment.update(DSH_WEB_DIR=web_dir, RUST_LOG='warn')
        self.applied = None
        self.owner = None
        self.commit = None

    def request(self, method, path, data=None, expected=200, headers=None,
                with_csrf=True, authenticated=True):
        request_headers = {'Origin': self.base, 'Content-Type': 'application/json'}
        if self.csrf and with_csrf:
            request_headers['X-CSRF-Token'] = self.csrf
        request_headers.update(headers or {})
        body = None if data is None else json.dumps(data).encode()
        request = urllib.request.Request(self.base + path, body, request_headers, method=method)
        client = self.client if authenticated else self.anonymous
        try:
            response = client.open(request, timeout=15)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            raw = response.read()
            code = response.code
            response_headers = response.headers
        if code != expected:
            # Route names and status codes are diagnostic metadata; request and
            # response bodies may contain credentials and are never included.
            raise AssertionError(f'{method} {path}: expected HTTP {expected}, got {code}')
        try:
            value = json.loads(raw)
        except json.JSONDecodeError:
            value = None
        return value, raw, response_headers

    def start(self):
        self.process = subprocess.Popen([
            self.binary, 'serve', '--data-dir', str(self.data_dir),
            '--listen', '127.0.0.1:' + str(self.server_port), '--origin', self.base,
            '--allow-private-model'], env=self.environment, stdout=self.log, stderr=self.log)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError('isolated Rust daemon exited before readiness')
            try:
                health, _, _ = self.request('GET', '/health/ready')
                self.commit = health['commit']
                return
            except (OSError, AssertionError):
                time.sleep(0.05)
        raise TimeoutError('isolated Rust daemon did not become ready')

    def stop(self):
        if self.process and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)

    def close(self):
        self.mock.hold_release.set()
        self.stop()
        self.log.close()

    @contextlib.contextmanager
    def db(self):
        # This harness may mutate only the database under its TemporaryDirectory.
        if not self.database.resolve().is_relative_to(self.root):
            raise RuntimeError('fixture database escaped temporary root')
        connection = sqlite3.connect(self.database, timeout=5)
        connection.row_factory = sqlite3.Row
        try:
            with connection:
                yield connection
        finally:
            connection.close()

    def login(self):
        auth, _, _ = self.request('POST', PREFIX + '/auth/login', {
            'username': self.username, 'password': self.password})
        self.csrf = auth['csrf_token']
        self.owner = auth['user']['id']

    def bootstrap(self):
        subprocess.run([self.binary, 'bootstrap-token', '--data-dir', str(self.data_dir)],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                       env=self.environment, timeout=10)
        token = json.loads((self.data_dir / 'bootstrap-token.json').read_text())['token']
        auth, _, _ = self.request('POST', PREFIX + '/auth/bootstrap', {
            'username': self.username, 'password': self.password, 'bootstrap_token': token})
        self.csrf = auth['csrf_token']
        self.owner = auth['user']['id']

    def configure(self, provider_port):
        current, _, _ = self.request('GET', PREFIX + '/config')
        credential, _, _ = self.request('POST', PREFIX + '/secrets', {
            'kind': 'model_api_key', 'value': self.key})
        candidate = copy.deepcopy(current['config'])
        candidate['model'] = {
            'base_url': 'http://127.0.0.1:' + str(provider_port),
            'model': 'isolated-memory-provider', 'credential_ref': credential['id']}
        candidate['qq']['enabled'] = False
        draft, _, _ = self.request('POST', PREFIX + '/config/drafts', {
            'base_revision': current['revision'], 'config': candidate})
        route = PREFIX + '/config/drafts/' + draft['id']
        validation, _, _ = self.request('POST', route + '/validate', {})
        if not validation['valid']:
            raise AssertionError('isolated model configuration was not valid')
        self.request('POST', route + '/apply', {}, expected=202,
                     headers={'If-Match': '"' + str(current['revision']) + '"'})
        applied, _, _ = self.request('GET', PREFIX + '/config')
        if applied['revision'] != applied['applied_revision']:
            raise AssertionError('isolated model configuration was not applied')
        self.applied = applied['config']

    def conversation(self, title):
        value, _, _ = self.request('POST', PREFIX + '/conversations', {'title': title})
        return value['id']

    def submit(self, conversation, content, client_id=None):
        value, _, _ = self.request(
            'POST', PREFIX + '/conversations/' + conversation + '/messages',
            {'content': content, 'client_message_id': client_id or str(uuid.uuid4())},
            expected=202)
        return value['run_id']

    def wait_run(self, run_id, via_api=True):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if via_api:
                value, _, _ = self.request('GET', PREFIX + '/runs/' + run_id)
            else:
                with self.db() as database:
                    row = database.execute(
                        'SELECT id,status,response,error FROM runs WHERE id=?', (run_id,)).fetchone()
                value = dict(row) if row else None
            if value and value['status'] in TERMINAL:
                return value
            time.sleep(0.05)
        raise TimeoutError('isolated run did not reach a terminal state')

    def model_turn(self, conversation, question):
        run_id = self.submit(conversation, question)
        result = self.wait_run(run_id)
        if result['status'] != 'completed':
            raise AssertionError('isolated model turn did not complete')
        return run_id, self.mock.for_question(question)

    def simulated_owner_turn(self, conversation, owner, question):
        # The queue fixture bypasses a platform adapter. It tests the daemon's
        # owner ACL and worker, not QQ transport/authentication/delivery.
        run_id = str(uuid.uuid4())
        timestamp = int(time.time())
        with self.db() as database:
            database.execute(
                'INSERT OR IGNORE INTO conversations(id,owner_id,title,source_key,created_at) '
                'VALUES(?,?,?, ?,?)',
                (conversation, owner, 'simulated channel owner', owner, timestamp))
            database.execute(
                'INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,'
                'status,created_at,updated_at,run_kind) VALUES(?,?,?,?,?,?,\'queued\',?,?,\'model\')',
                (run_id, conversation, owner, run_id, question,
                 json.dumps(self.applied), timestamp, timestamp))
            database.execute(
                'INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) '
                'VALUES(?,?,?,\'user\',?,?)',
                (str(uuid.uuid4()), conversation, run_id, question, timestamp))
        result = self.wait_run(run_id, via_api=False)
        if result['status'] != 'completed':
            raise AssertionError('simulated owner-domain run did not complete')
        return run_id, result


def payload_text(payload):
    return json.dumps(payload['messages'], ensure_ascii=False)


def memory_envelope(payload, canary):
    # The envelope precedes the actual current question and is JSON data in a
    # user message. It must never replace the role/system policy.
    for message in payload['messages'][:-1]:
        if message['role'] == 'user' and canary in message['content']:
            for line in reversed(message['content'].splitlines()):
                try:
                    data = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if isinstance(data, list) and any(isinstance(item, dict) and
                                                 'revision' in item and 'content' in item
                                                 for item in data):
                    return data
    raise AssertionError('memory envelope was not observed as user-role JSON data')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report')
    args = parser.parse_args()
    binary = str(pathlib.Path(args.binary).resolve(strict=True))
    web_dir = str(pathlib.Path(args.web_dir).resolve(strict=True))
    with open(binary, 'rb') as binary_file:
        binary_sha256 = hashlib.file_digest(binary_file, 'sha256').hexdigest()
    report = {
        'kind': 'assistant-memory real-process black-box with isolated mock provider',
        'started_at': utc_now(), 'platform': platform.system(),
        'architecture': platform.machine(), 'commit': None, 'status': 'running',
        'binary_sha256': binary_sha256,
        'real_external_model_tested': False, 'real_qq_tested': False,
        'simulated_channel_owner_fixture': True, 'production_data_accessed': False,
        'checks': [],
    }
    current_check = 'test setup'

    def check(name, condition):
        nonlocal current_check
        current_check = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    mock = MockState()
    provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    provider.daemon_threads = True
    provider.mock_state = mock
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    failed = False
    harness = None
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-memory-smoke-') as temporary_root:
            harness = Harness(binary, web_dir, temporary_root, mock)
            try:
                harness.start()
                report['commit'] = harness.commit
                for route in ['/memory', '/memory/export', '/memory/not-a-record']:
                    harness.request('GET', PREFIX + route, expected=401)
                check('memory reads and exports require authentication', True)
                harness.bootstrap()
                harness.request('POST', PREFIX + '/memory', {
                    'content': 'synthetic', 'visibility': 'shared', 'publish': True},
                    expected=403, with_csrf=False)
                harness.request('POST', PREFIX + '/memory', {
                    'content': 'synthetic', 'visibility': 'shared', 'publish': True},
                    expected=403, headers={'Origin': 'https://example.invalid'})
                check('memory mutations enforce CSRF and exact Origin', True)
                offline_conversation = harness.conversation('unconfigured model command fixture')
                offline_canary = 'synthetic-offline-command-' + secrets.token_hex(8)
                before_configuration = mock.count()
                offline_result = harness.wait_run(harness.submit(
                    offline_conversation, '/memory remember ' + offline_canary))
                offline_page, _, _ = harness.request(
                    'GET', PREFIX + '/memory?q=' + urllib.parse.quote(offline_canary))
                check('explicit remember works before any model credential is configured',
                      offline_result['status'] == 'completed' and offline_page['total'] == 1
                      and mock.count() == before_configuration)
                offline_memory = offline_page['items'][0]
                offline_forgotten = harness.wait_run(harness.submit(
                    offline_conversation,
                    '/memory forget ' + offline_memory['id'] + '@' + str(offline_memory['revision'])))
                check('explicit forget also works without a model provider',
                      offline_forgotten['status'] == 'completed' and mock.count() == before_configuration)
                harness.configure(provider.server_port)
                page, _, _ = harness.request('GET', PREFIX + '/memory')
                assistant_id = page['assistant_id']
                uuid.UUID(assistant_id)
                check('assistant has a persisted identity independent of conversation', page['total'] == 0)
                source = harness.conversation('memory source fixture')
                first = harness.conversation('first assistant conversation')
                second = harness.conversation('second assistant conversation')
                suffix = secrets.token_hex(8)
                old = 'synthetic-old-memory-' + suffix
                corrected = 'synthetic-corrected-memory-' + suffix
                private_admin = 'synthetic-private-admin-' + suffix
                private_other = 'synthetic-private-channel-' + suffix
                command_old = 'synthetic-command-old-' + suffix
                command_new = 'synthetic-command-new-' + suffix
                held_canary = 'synthetic-held-memory-' + suffix
                mock.canaries.extend([old, corrected, private_admin, private_other,
                                      command_old, command_new, held_canary])

                source_run, _ = harness.model_turn(source, 'source fixture ' + old)
                source_messages, _, _ = harness.request(
                    'GET', PREFIX + '/conversations/' + source + '/messages')
                source_message = next(message['id'] for message in source_messages['items']
                                      if message['role'] == 'user')
                harness.request('POST', PREFIX + '/memory', {
                    'content': old, 'visibility': 'shared'}, expected=400)
                check('shared publication requires explicit confirmation', True)
                shared, _, shared_headers = harness.request('POST', PREFIX + '/memory', {
                    'content': old, 'visibility': 'shared', 'publish': True,
                    'source_conversation_id': source, 'source_message_id': source_message}, expected=201)
                shared_path = PREFIX + '/memory/' + shared['id']
                check('source and initial revision are returned without secret material',
                      shared['source_message_id'] == source_message and shared['revision'] == 1
                      and shared_headers.get('ETag') == '"1"'
                      and harness.key not in json.dumps(shared))
                harness.request('PATCH', shared_path, {'content': corrected}, expected=428)
                harness.request('DELETE', shared_path, expected=428)
                harness.request('PATCH', shared_path, {'content': corrected}, expected=400,
                                headers={'If-Match': 'invalid'})
                harness.request('PATCH', shared_path, {'content': corrected}, expected=409,
                                headers={'If-Match': '"99"'})
                check('memory edits and deletion require a valid current If-Match revision', True)

                for bad in ['', '  ', 'x' * 4001, 'invalid\x00text']:
                    harness.request('POST', PREFIX + '/memory', {
                        'content': bad, 'visibility': 'shared', 'publish': True}, expected=400)
                harness.request('POST', PREFIX + '/memory', {
                    'content': 'synthetic', 'visibility': 'private'}, expected=400)
                harness.request('POST', PREFIX + '/memory', {
                    'content': 'synthetic', 'visibility': 'shared', 'publish': True,
                    'source_conversation_id': second, 'source_message_id': source_message}, expected=400)
                harness.request('POST', PREFIX + '/memory', {
                    'content': 'synthetic', 'visibility': 'shared', 'publish': True,
                    'source_owner_id': 'forged'}, expected=422)
                for query in ['limit=0', 'limit=101', 'q=' + 'x' * 257,
                              'q=' + urllib.parse.quote('\x00')]:
                    harness.request('GET', PREFIX + '/memory?' + query, expected=400)
                check('invalid content, forged ownership, source mismatch and query bounds are rejected', True)

                first_run, first_payload = harness.model_turn(first, 'shared first probe ' + suffix)
                _, second_payload = harness.model_turn(second, 'shared second probe ' + suffix)
                check('shared assistant memory crosses new Web conversations',
                      old in payload_text(first_payload) and old in payload_text(second_payload))
                envelope = memory_envelope(first_payload, old)
                check('memory enters the model as versioned source data rather than system policy',
                      any(item['id'] == shared['id'] and item['revision'] == 1
                          and item['source_message_id'] == source_message for item in envelope)
                      and all('source_owner_id' not in item and 'created_by' not in item for item in envelope)
                      and all(old not in message['content'] for message in first_payload['messages']
                              if message['role'] == 'system'))
                cli_question = 'shared cli probe ' + suffix
                cli = subprocess.run([
                    binary, 'chat', '--url', harness.base, '--username', harness.username,
                    '--message', cli_question],
                    env=dict(harness.environment, DSH_ADMIN_PASSWORD=harness.password),
                    capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=25)
                check('authenticated real CLI shares the same assistant memory',
                      cli.returncode == 0 and old in payload_text(mock.for_question(cli_question)))
                with harness.db() as database:
                    snapshot = database.execute(
                        'SELECT messages_json,digest,compiler_version FROM run_prompt_snapshots WHERE run_id=?',
                        (first_run,)).fetchone()
                    uses = database.execute(
                        'SELECT memory_id,revision FROM memory_run_uses WHERE run_id=?',
                        (first_run,)).fetchall()
                check('prompt snapshot digest and memory revision provenance are durable',
                      snapshot is not None
                      and hashlib.sha256(snapshot['messages_json'].encode()).hexdigest() == snapshot['digest']
                      and bool(snapshot['compiler_version'])
                      and any(row['memory_id'] == shared['id'] and row['revision'] == 1 for row in uses))

                shared, _, _ = harness.request('PATCH', shared_path, {'content': corrected},
                                               headers={'If-Match': '"1"'})
                for conversation, label in [(first, 'same'), (second, 'other'), (source, 'source')]:
                    _, payload = harness.model_turn(conversation, 'after correction ' + label + ' ' + suffix)
                    check('correction replaces old facts and model echoes in ' + label + ' context',
                          old not in payload_text(payload) and corrected in payload_text(payload))
                source_messages, _, _ = harness.request(
                    'GET', PREFIX + '/conversations/' + source + '/messages')
                check('context invalidation accurately retains original user-visible history',
                      any(old in message['content'] for message in source_messages['items']))

                private, _, _ = harness.request('POST', PREFIX + '/memory', {
                    'content': private_admin, 'visibility': 'private',
                    'source_conversation_id': first}, expected=201)
                harness.request('PATCH', PREFIX + '/memory/' + private['id'], {
                    'content': private_admin, 'visibility': 'shared'}, expected=400,
                    headers={'If-Match': '"1"'})
                check('private records cannot be published without a separate explicit confirmation', True)
                other_owner = 'qq:synthetic-app:c2c:synthetic-owner-' + suffix
                other_conversation = str(uuid.uuid4())
                # Materialize the simulated owner domain without invoking a
                # platform adapter, credential flow or outbound network service.
                with harness.db() as database:
                    database.execute(
                        'INSERT INTO conversations(id,owner_id,title,source_key,created_at) VALUES(?,?,?,?,?)',
                        (other_conversation, other_owner, 'simulated channel owner',
                         other_owner, int(time.time())))
                private_channel, _, _ = harness.request('POST', PREFIX + '/memory', {
                    'content': private_other, 'visibility': 'private',
                    'source_conversation_id': other_conversation}, expected=201)
                other_question = 'simulated owner probe ' + suffix
                other_run, _ = harness.simulated_owner_turn(other_conversation, other_owner, other_question)
                other_payload = mock.for_question(other_question)
                _, owner_payload = harness.model_turn(second, 'admin owner probe ' + suffix)
                check('private retrieval is isolated between Web and simulated channel-owner domains',
                      private_admin not in payload_text(other_payload)
                      and private_other in payload_text(other_payload)
                      and corrected in payload_text(other_payload)
                      and private_other not in payload_text(owner_payload)
                      and private_admin in payload_text(owner_payload))
                harness.request('GET', PREFIX + '/runs/' + other_run, expected=404)
                before = mock.count()
                _, denied = harness.simulated_owner_turn(
                    other_conversation, other_owner, '/memory show ' + private['id'])
                check('simulated channel commands cannot inspect another owner private memory',
                      private_admin not in (denied['response'] or '') and mock.count() == before)

                chinese, _, _ = harness.request('POST', PREFIX + '/memory', {
                    'content': '助手的蓝鲸记忆偏好测试 ' + suffix,
                    'visibility': 'shared', 'publish': True}, expected=201)
                search, _, _ = harness.request('GET', PREFIX + '/memory?q=' + urllib.parse.quote('蓝鲸记忆'))
                short_search, _, _ = harness.request('GET', PREFIX + '/memory?q=' + urllib.parse.quote('蓝鲸'))
                check('Chinese trigram and short-query search return the expected memory',
                      {item['id'] for item in search['items']} == {chinese['id']}
                      and chinese['id'] in {item['id'] for item in short_search['items']})
                literal, _, _ = harness.request(
                    'GET', PREFIX + '/memory?q=' + urllib.parse.quote('x" OR content:*'))
                check('FTS operators in user input are treated as literal search text', literal['total'] == 0)
                exported, _, export_headers = harness.request('GET', PREFIX + '/memory/export')
                check('authenticated export includes current assistant scope and active records',
                      exported['scope']['assistant_id'] == assistant_id
                      and {shared['id'], private['id'], private_channel['id'], chinese['id']}
                      <= {item['id'] for item in exported['items']}
                      and 'attachment' in export_headers.get('Content-Disposition', ''))

                command_conversation = harness.conversation('local memory command fixture')
                before = mock.count()
                command_text = '/memory remember ' + command_old
                command_client_id = 'durable-command-' + suffix
                command_run = harness.submit(command_conversation, command_text, command_client_id)
                command_result = harness.wait_run(command_run)
                duplicate_run = harness.submit(command_conversation, command_text, command_client_id)
                harness.wait_run(duplicate_run)
                command_page, _, _ = harness.request(
                    'GET', PREFIX + '/memory?q=' + urllib.parse.quote(command_old))
                check('remember command commits one effect for a repeated client message ID',
                      command_result['status'] == 'completed' and duplicate_run == command_run
                      and command_page['total'] == 1)
                command_memory = command_page['items'][0]
                for text in ['/memory list ' + command_old, '/memory show ' + command_memory['id'],
                             '/memory correct ' + command_memory['id'] + '@1 ' + command_new]:
                    result = harness.wait_run(harness.submit(command_conversation, text))
                    if result['status'] != 'completed':
                        raise AssertionError('local memory command did not complete')
                command_memory, _, _ = harness.request('GET', PREFIX + '/memory/' + command_memory['id'])
                check('local show, list and correct commands use the stored version',
                      command_memory['revision'] == 2 and command_memory['content'] == command_new)
                stale = harness.wait_run(harness.submit(
                    command_conversation, '/memory correct ' + command_memory['id'] + '@1 rejected stale edit'))
                after_stale, _, _ = harness.request('GET', PREFIX + '/memory/' + command_memory['id'])
                check('stale command correction preserves the current memory revision',
                      stale['status'] == 'completed' and after_stale['revision'] == 2
                      and after_stale['content'] == command_new)
                forgotten_command = harness.wait_run(harness.submit(
                    command_conversation, '/memory forget ' + command_memory['id'] + '@2'))
                check('memory control commands complete without any model request',
                      forgotten_command['status'] == 'completed' and mock.count() == before)
                harness.request('GET', PREFIX + '/memory/' + command_memory['id'], expected=404)
                with harness.db() as database:
                    command_kind = database.execute('SELECT run_kind FROM runs WHERE id=?',
                                                    (command_run,)).fetchone()[0]
                    command_snapshot_count = database.execute(
                        'SELECT count(*) FROM run_prompt_snapshots s JOIN runs r ON r.id=s.run_id '
                        'WHERE r.conversation_id=?', (command_conversation,)).fetchone()[0]
                _, after_commands_payload = harness.model_turn(
                    command_conversation, 'after command cleanup ' + suffix)
                check('memory command content and confirmations never enter future model history',
                      command_kind == 'memory_command' and command_snapshot_count == 0
                      and command_old not in payload_text(after_commands_payload)
                      and command_new not in payload_text(after_commands_payload))

                harness.request('DELETE', shared_path, expected=409, headers={'If-Match': '"1"'})
                forgotten, _, _ = harness.request('DELETE', shared_path, headers={'If-Match': '"2"'})
                check('forget exposes accurate memory, history and backup cleanup boundaries',
                      forgotten['deleted'] is True
                      and forgotten['cleanup']['memory_content'] == 'removed'
                      and forgotten['cleanup']['conversation_history'] == 'retained'
                      and forgotten['cleanup']['offline_backups'] == 'retained')
                harness.request('GET', shared_path, expected=404)
                export_after, _, _ = harness.request('GET', PREFIX + '/memory/export')
                with harness.db() as database:
                    tombstone = database.execute(
                        'SELECT rowid,content,deleted_at,deletion_sequence FROM memory_records WHERE id=?',
                        (shared['id'],)).fetchone()
                    versions = database.execute(
                        'SELECT count(*) FROM memory_versions WHERE memory_id=? AND content IS NOT NULL',
                        (shared['id'],)).fetchone()[0]
                    index_rows = database.execute('SELECT count(*) FROM memory_fts WHERE rowid=?',
                                                  (tombstone['rowid'],)).fetchone()[0]
                check('forget removes all live memory versions and FTS content and retains a tombstone',
                      tombstone['content'] is None and tombstone['deleted_at'] is not None
                      and tombstone['deletion_sequence'] > 0 and versions == 0 and index_rows == 0
                      and shared['id'] not in {item['id'] for item in export_after['items']})
                fresh = harness.conversation('fresh conversation after forget')
                for conversation, label in [(first, 'same'), (fresh, 'new'), (source, 'source')]:
                    _, payload = harness.model_turn(conversation, 'after forget ' + label + ' ' + suffix)
                    check('forgotten facts do not return through ' + label + ' conversation context',
                          old not in payload_text(payload) and corrected not in payload_text(payload))

                held_memory, _, _ = harness.request('POST', PREFIX + '/memory', {
                    'content': held_canary, 'visibility': 'shared', 'publish': True}, expected=201)
                held_conversation = harness.conversation('in-flight memory invalidation')
                mock.hold_question = 'event witnessed in-flight question ' + suffix
                held_run = harness.submit(held_conversation, mock.hold_question)
                if not mock.hold_received.wait(timeout=15):
                    raise TimeoutError('mock provider did not witness the in-flight HTTP request')
                check('in-flight fixture is witnessed by a provider event with the memory included',
                      held_canary in payload_text(mock.for_question(mock.hold_question)))
                harness.request('DELETE', PREFIX + '/memory/' + held_memory['id'],
                                headers={'If-Match': '"1"'})
                cancelled = harness.wait_run(held_run)
                mock.hold_release.set()
                if not mock.hold_responded.wait(timeout=10):
                    raise TimeoutError('mock provider did not settle the released request')
                _, fence_payload = harness.model_turn(held_conversation, 'after in-flight forget ' + suffix)
                with harness.db() as database:
                    held_after = database.execute('SELECT status,response FROM runs WHERE id=?',
                                                  (held_run,)).fetchone()
                    assistant_rows = database.execute(
                        'SELECT count(*) FROM messages WHERE run_id=? AND role=\'assistant\'',
                        (held_run,)).fetchone()[0]
                    outbox_rows = database.execute('SELECT count(*) FROM outbox WHERE run_id=?',
                                                   (held_run,)).fetchone()[0]
                check('forget during a witnessed HTTP request blocks the old result and delivery',
                      cancelled['status'] == 'cancelled' and held_after['status'] == 'cancelled'
                      and held_after['response'] is None and assistant_rows == 0 and outbox_rows == 0
                      and held_canary not in payload_text(fence_payload))

                harness.stop()
                harness.start()
                harness.login()
                restored, _, _ = harness.request('GET', PREFIX + '/memory')
                check('assistant identity and deletion state survive a real daemon restart',
                      restored['assistant_id'] == assistant_id
                      and shared['id'] not in {item['id'] for item in restored['items']}
                      and held_memory['id'] not in {item['id'] for item in restored['items']})
                _, restarted_payload = harness.model_turn(first, 'after restart forget check ' + suffix)
                restarted_new = harness.conversation('new conversation after restart')
                _, restarted_new_payload = harness.model_turn(
                    restarted_new, 'after restart new conversation ' + suffix)
                check('forgotten data and old echoes remain excluded after restart and new conversation',
                      all(canary not in payload_text(payload)
                          for canary in [old, corrected, command_old, command_new, held_canary]
                          for payload in [restarted_payload, restarted_new_payload]))
                harness.request('POST', PREFIX + '/auth/logout', {})
                harness.csrf = None
                harness.request('GET', PREFIX + '/memory', expected=401)
                check('logout revokes memory management access', True)
                report['status'] = 'passed'
            finally:
                harness.close()
    except Exception as error:
        failed = True
        report['status'] = 'failed'
        report['failure_after_check'] = current_check
        report['error_type'] = type(error).__name__
        # Errors generated by this script contain only route/status/check names.
        # Do not print raw subprocess output, HTTP bodies or SQLite row values.
        print('FAIL ' + str(error), flush=True)
    finally:
        mock.hold_release.set()
        provider.shutdown()
        provider.server_close()
        report['finished_at'] = utc_now()
        report['passed'] = len(report['checks'])
        report['mock_model_requests'] = mock.count()
        if args.report:
            destination = pathlib.Path(args.report).resolve()
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps({'status': report['status'], 'passed': report['passed'],
                          'real_external_model_tested': False, 'real_qq_tested': False}))
    return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
