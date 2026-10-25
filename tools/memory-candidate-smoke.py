#!/usr/bin/env python3
"""Black-box candidate-memory review in a real, isolated Rust daemon.

Uses an ephemeral loopback provider and synthetic conversations only. Explicit
SQLite fixtures test unavailable transport, expiry and restart boundaries; they
never read production data or claim to exercise a real channel. Reports contain
release hashes and check names, never source bodies, model output or credentials.
"""
import argparse
import contextlib
import hashlib
import http.cookiejar
import http.server
import importlib.util
import json
import pathlib
import platform
import re
import secrets
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    'candidate_memory_harness', pathlib.Path(__file__).with_name('memory-smoke.py'))
acceptance = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(acceptance)
PREFIX = acceptance.PREFIX
TERMINAL = {'completed', 'failed', 'cancelled', 'interrupted', 'expired'}


class ResponseStatusError(AssertionError):
    def __init__(self, actual, expected):
        super().__init__('management response status differs from acceptance contract')
        self.actual = actual
        self.expected = sorted(expected)


class Mock:
    def __init__(self):
        self.lock = threading.Lock()
        self.plans = {}
        self.requests = []
        self.extract_requests = []
        self.hold_release = threading.Event()
        self.hold_received = threading.Event()
        self.hold_responded = threading.Event()
        self.active = 0
        self.active_extract = 0
        self.maximum = 0
        self.maximum_extract = 0

    def arm(self, source_ids, mode='normal', held=False):
        plan = {'ids': list(source_ids), 'mode': mode, 'held': held}
        with self.lock:
            for source_id in source_ids:
                self.plans[source_id] = plan
        if held:
            self.hold_received.clear()
            self.hold_release.clear()
            self.hold_responded.clear()

    def count(self):
        with self.lock:
            return len(self.extract_requests)

    def payloads(self):
        with self.lock:
            return list(self.extract_requests)

    def respond(self, payload):
        serialized = json.dumps(payload, ensure_ascii=False)
        extraction = bool(payload.get('messages') and
                          payload['messages'][0].get('role') == 'system' and
                          payload['messages'][0].get('content', '').startswith(
                              'DSH_MEMORY_CANDIDATE_EXTRACTION_V1'))
        with self.lock:
            plan = next((value for key, value in self.plans.items() if key in serialized), None) if extraction else None
            self.requests.append(payload)
            if extraction:
                self.extract_requests.append(payload)
            self.active += 1
            self.active_extract += int(extraction)
            self.maximum = max(self.maximum, self.active)
            self.maximum_extract = max(self.maximum_extract, self.active_extract)
        held = plan is not None and plan['held']
        try:
            if held:
                self.hold_received.set()
                self.hold_release.wait(timeout=25)
            if plan is None:
                return 200, 'SYNTHETIC-ACK'
            ids, mode = plan['ids'], plan['mode']
            items = [{'content': '合成验收偏好：喜欢在安静的环境阅读。',
                      'source_message_ids': ids},
                     {'content': '合成验收偏好：喜欢使用简单的项目代号。',
                      'source_message_ids': [ids[0]]}]
            result = {'candidates': items}
            if mode == 'extra_top':
                result['unexpected'] = True
            elif mode == 'extra_item':
                items[0]['confidence'] = 1
            elif mode == 'wrong_type':
                items[0]['content'] = 27
            elif mode == 'too_many':
                result['candidates'] = [dict(items[0]) for _ in range(4)]
            elif mode == 'too_large':
                items[0]['content'] = '鲸' * 334
            elif mode == 'fake_source':
                items[0]['source_message_ids'] = [str(uuid.uuid4())]
            elif mode == 'empty_source':
                items[0]['source_message_ids'] = []
            elif mode == 'secret':
                items[0]['content'] = 'api_key = sk-SyntheticCandidateSecret01234567890123456789'
            elif mode == 'empty':
                result['candidates'] = []
            raw = json.dumps(result, ensure_ascii=False)
            if mode == 'fenced':
                raw = '```json\n' + raw + '\n```'
            elif mode == 'trailing':
                raw += ' trailing text'
            elif mode == 'malformed':
                raw = '{"candidates":'
            elif mode == 'failure':
                return 503, 'synthetic unavailable'
            return 200, raw
        finally:
            with self.lock:
                self.active -= 1
                self.active_extract -= int(extraction)
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
        payload = json.loads(self.rfile.read(size))
        status, content = self.server.mock.respond(payload)
        raw = json.dumps({'choices': [{'message': {
            'role': 'assistant', 'content': content}}]}, ensure_ascii=False).encode()
        try:
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass


class Harness(acceptance.Harness):
    def call(self, method, path, data=None, expected=200, headers=None,
             with_csrf=True, authenticated=True, timeout=15):
        request_headers = {'Origin': self.base, 'Content-Type': 'application/json'}
        if self.csrf and with_csrf:
            request_headers['X-CSRF-Token'] = self.csrf
        request_headers.update(headers or {})
        body = None if data is None else json.dumps(data).encode()
        request = urllib.request.Request(self.base + path, body, request_headers, method=method)
        client = self.client if authenticated else self.anonymous
        try:
            response = client.open(request, timeout=timeout)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            raw, code, response_headers = response.read(), response.code, response.headers
        allowed = {expected} if isinstance(expected, int) else set(expected)
        if code not in allowed:
            raise ResponseStatusError(code, allowed)
        try:
            value = json.loads(raw)
        except json.JSONDecodeError:
            value = None
        return value, raw, response_headers


def route(conversation):
    return PREFIX + '/conversations/' + conversation + '/memory-candidates'


def state(harness, conversation):
    value, _, headers = harness.call('GET', route(conversation))
    if headers.get('ETag') != '"' + value['source_revision'] + '"':
        raise AssertionError('candidate source ETag does not match opaque revision')
    return value, headers['ETag']


def wait_job(harness, job_id):
    deadline = time.monotonic() + 28
    while time.monotonic() < deadline:
        value, _, _ = harness.call('GET', PREFIX + '/memory-candidate-jobs/' + job_id)
        if value['status'] in TERMINAL:
            return value
        time.sleep(0.05)
    raise TimeoutError('candidate job did not reach a terminal state')


def start_extract(harness, mock, conversation, mode='normal', held=False, source_ids=None):
    before, tag = state(harness, conversation)
    ids = source_ids if source_ids is not None else [item['id'] for item in before['sources']]
    mock.arm(ids, mode, held)
    value, _, _ = harness.call('POST', route(conversation) + '/extract',
                              {'source_message_ids': ids}, expected=202,
                              headers={'If-Match': tag})
    return value, tag, ids


def source_turn(harness, text='合成验收资料：我喜欢安静阅读，也喜欢简单的项目代号。'):
    conversation = harness.conversation('synthetic candidate-memory source')
    run = harness.submit(conversation, text)
    if harness.wait_run(run)['status'] != 'completed':
        raise AssertionError('synthetic source model run failed')
    return conversation, run


def pending_items(harness, conversation):
    return [item for item in state(harness, conversation)[0]['items']
            if item['status'] == 'pending']


def requested_sources(payload):
    return json.loads(payload['messages'][-1]['content'])['sources']


def memory_count(harness):
    with harness.db() as database:
        return database.execute('SELECT count(*) FROM memory_records WHERE deleted_at IS NULL').fetchone()[0]


def synthetic_run(harness, conversation, content, kind='model', owner=None, status='completed'):
    """Completed source fixture; no channel protocol or model is simulated as real."""
    run, message = str(uuid.uuid4()), str(uuid.uuid4())
    timestamp = int(time.time())
    with harness.db() as database:
        database.execute(
            'INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,'
            'status,response,created_at,updated_at,run_kind) VALUES(?,?,?,?,?,?,?,?,?,?,?)',
            (run, conversation, owner or harness.owner, run, content,
             json.dumps(harness.applied), status, 'SYNTHETIC-ACK', timestamp, timestamp, kind))
        database.execute(
            'INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?,?,?,?,?,?)',
            (message, conversation, run, 'user', content, timestamp))
        assistant = str(uuid.uuid4())
        database.execute(
            'INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?,?,?,?,?,?)',
            (assistant, conversation, run, 'assistant', 'SYNTHETIC-ACK', timestamp))
    return run, message, assistant


def candidate_table(harness):
    with harness.db() as database:
        tables = [row[0] for row in database.execute(
            "SELECT name FROM sqlite_schema WHERE type='table' AND name LIKE '%candidate%'")]
        for name in tables:
            # Names originate in sqlite_schema, not API input, and are quoted.
            columns = {row[1] for row in database.execute('PRAGMA table_info("' + name.replace('"', '""') + '")')}
            if {'id', 'job_id', 'content', 'status', 'expires_at'} <= columns:
                return name
    raise AssertionError('candidate persistence table unavailable')


def expire_candidate(harness, candidate_id):
    table = candidate_table(harness).replace('"', '""')
    with harness.db() as database:
        database.execute('UPDATE "' + table + '" SET expires_at=? WHERE id=?',
                         (int(time.time()) - 1, candidate_id))


def body_cleared(harness, candidate_id):
    table = candidate_table(harness).replace('"', '""')
    with harness.db() as database:
        row = database.execute('SELECT content FROM "' + table + '" WHERE id=?', (candidate_id,)).fetchone()
    return row is not None and row[0] is None


def invalidate_source(harness, conversation, source_id):
    memory, _, _ = harness.call('POST', PREFIX + '/memory', {
        'content': '合成临时来源失效标记。', 'visibility': 'private',
        'source_conversation_id': conversation, 'source_message_id': source_id}, expected=201)
    harness.call('DELETE', PREFIX + '/memory/' + memory['id'],
                 headers={'If-Match': '"' + str(memory['revision']) + '"'})


@contextlib.contextmanager
def other_admin(harness):
    """Second authenticated synthetic admin; same password hash, distinct owner."""
    identifier, username = str(uuid.uuid4()), 'candidate-other-' + secrets.token_hex(5)
    with harness.db() as database:
        database.execute('INSERT INTO admins(id,username,password_hash,created_at) '
                         'SELECT ?,?,password_hash,? FROM admins WHERE id=?',
                         (identifier, username, int(time.time()), harness.owner))
    previous = (harness.client, harness.jar, harness.csrf, harness.owner, harness.username)
    harness.jar = http.cookiejar.CookieJar()
    harness.client = urllib.request.build_opener(
        urllib.request.ProxyHandler({}), urllib.request.HTTPCookieProcessor(harness.jar))
    harness.csrf, harness.username = None, username
    try:
        harness.login()
        yield
    finally:
        harness.client, harness.jar, harness.csrf, harness.owner, harness.username = previous


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report')
    args = parser.parse_args()
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    with binary.open('rb') as handle:
        binary_hash = hashlib.file_digest(handle, 'sha256').hexdigest()
    report = {'kind': 'candidate-memory real-process acceptance with isolated loopback model',
              'started_at': acceptance.utc_now(), 'commit': None, 'deployment_sha': None,
              'binary_sha256': binary_hash, 'platform': platform.system(),
              'architecture': platform.machine(), 'status': 'running',
              'real_external_model_tested': False, 'real_qq_tested': False,
              'real_weixin_private_chat_tested': False, 'production_data_accessed': False,
              'synthetic_source_fixtures_used': True, 'checks': []}
    stage = 'setup'

    def check(name, condition):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    mock = Mock()
    provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    provider.daemon_threads = True
    provider.mock = mock
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    failed = False
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-candidate-smoke-') as temporary:
            harness = Harness(str(binary), str(web), temporary, mock)
            try:
                harness.start()
                report['commit'] = report['deployment_sha'] = harness.commit
                harness.bootstrap()
                harness.configure(provider.server_port)
                chat, run = source_turn(harness)
                initial, tag = state(harness, chat)
                source_id = initial['sources'][0]['id']
                extract = route(chat) + '/extract'
                body = {'source_message_ids': [source_id]}
                before = mock.count()
                harness.call('GET', route(chat), expected=401, authenticated=False)
                harness.call('POST', extract, body, expected=401, authenticated=False)
                harness.call('POST', extract, body, expected=403, with_csrf=False)
                harness.call('POST', extract, body, expected=403,
                             headers={'Origin': 'https://example.invalid', 'If-Match': tag})
                harness.call('POST', extract, body, expected=428)
                for stale in ['"0"', '"stale"']:
                    harness.call('POST', extract, body, expected=(400, 409), headers={'If-Match': stale})
                check('authentication Origin CSRF and source revision guard extraction before model work',
                      mock.count() == before and initial['items'] == [] and initial['job'] is None)
                check('ordinary chat never automatically extracts candidate memories', mock.count() == 0)
                check('eligible source API exposes actual user message IDs and fixed policy limits',
                      all(initial['policy'].get(key) == value for key, value in {
                          'max_sources': 12, 'max_source_bytes': 12000,
                          'max_candidates': 3, 'max_content_bytes': 1000}.items())
                      and initial['sources'][0]['byte_length'] == len(initial['sources'][0]['content'].encode())
                      and source_id != run)

                with harness.db() as database:
                    assistant_id = database.execute(
                        "SELECT id FROM messages WHERE run_id=? AND role='assistant'", (run,)).fetchone()[0]
                    foreign = str(uuid.uuid4())
                    database.execute('INSERT INTO conversations(id,owner_id,title,created_at) VALUES(?,?,?,?)',
                                     (foreign, 'qq:synthetic:c2c:other-owner', 'synthetic foreign source', int(time.time())))
                _, foreign_id, _ = synthetic_run(harness, foreign, '合成外部来源', owner='qq:synthetic:c2c:other-owner')
                harness.call('GET', route(foreign), expected=404)
                harness.call('POST', route(foreign) + '/extract', {'source_message_ids': [foreign_id]},
                             expected=404, headers={'If-Match': tag})
                for invalid_ids in [[], [source_id, source_id], [str(uuid.uuid4())], [assistant_id], [foreign_id]]:
                    harness.call('POST', extract, {'source_message_ids': invalid_ids},
                                 expected=(400, 422), headers={'If-Match': tag})
                harness.call('POST', extract, dict(body, visibility='shared'),
                             expected=(400, 422), headers={'If-Match': tag})
                check('source selection rejects duplicates assistant fabricated cross-owner and extra fields',
                      mock.count() == before)

                count_before = memory_count(harness)
                job, _, ids = start_extract(harness, mock, chat)
                done = wait_job(harness, job['id'])
                candidates = pending_items(harness, chat)
                check('one manual job creates bounded pending candidates without storing long-term memory',
                      done['status'] == 'completed' and done['source_count'] == len(ids)
                      and len(candidates) == 2 and memory_count(harness) == count_before)
                payload = mock.payloads()[-1]
                check('extractor request includes only selected user source provenance as data',
                      [item['id'] for item in requested_sources(payload)] == ids
                      and source_id in json.dumps(payload) and assistant_id not in json.dumps(payload)
                      and foreign_id not in json.dumps(payload)
                      and all(set(item['source_message_ids']) <= set(ids) for item in candidates))
                candidate = candidates[0]
                candidate_path = PREFIX + '/memory-candidates/' + candidate['id']
                with other_admin(harness):
                    harness.call('GET', route(chat), expected=404)
                    harness.call('GET', candidate_path, expected=404)
                    harness.call('GET', PREFIX + '/memory-candidate-jobs/' + job['id'], expected=404)
                    harness.call('POST', candidate_path + '/accept', {}, expected=404,
                                 headers={'If-Match': '"' + str(candidate['revision']) + '"'})
                    harness.call('POST', candidate_path + '/reject', {}, expected=404,
                                 headers={'If-Match': '"' + str(candidate['revision']) + '"'})
                check('a second authenticated administrator cannot inspect or approve another source owner',
                      memory_count(harness) == count_before)
                harness.call('GET', candidate_path, expected=401, authenticated=False)
                harness.call('GET', PREFIX + '/memory-candidate-jobs/' + job['id'],
                             expected=401, authenticated=False)
                harness.call('POST', candidate_path + '/accept', {}, expected=403, with_csrf=False)
                harness.call('POST', candidate_path + '/accept', {}, expected=428)
                harness.call('POST', candidate_path + '/accept', {}, expected=(400, 409),
                             headers={'If-Match': '"0"'})
                harness.call('POST', candidate_path + '/accept', {'visibility': 'shared'},
                             expected=(400, 422), headers={'If-Match': '"' + str(candidate['revision']) + '"'})
                accepted, _, _ = harness.call('POST', candidate_path + '/accept', {},
                    headers={'If-Match': '"' + str(candidate['revision']) + '"'})
                harness.call('POST', candidate_path + '/accept', {}, expected=409,
                             headers={'If-Match': '"' + str(candidate['revision']) + '"'})
                again, _, _ = harness.call('POST', candidate_path + '/accept', {},
                    headers={'If-Match': '"' + str(accepted['revision']) + '"'})
                memory, _, _ = harness.call('GET', PREFIX + '/memory/' + accepted['memory_id'])
                with harness.db() as database:
                    assistant = database.execute("SELECT value FROM settings WHERE key='assistant.identity.v1'").fetchone()[0]
                check('approval atomically creates exactly one source-private stable-assistant memory',
                      accepted['status'] == 'accepted' and not accepted.get('content')
                      and again['memory_id'] == accepted['memory_id']
                      and memory_count(harness) == count_before + 1
                      and memory['visibility'] == 'private' and memory['source_owner_id'] == harness.owner
                      and memory['source_conversation_id'] == chat and memory['source_message_id'] in ids
                      and memory['assistant_id'] == assistant and body_cleared(harness, candidate['id']))
                reject = candidates[1]
                rejected, _, _ = harness.call('POST', PREFIX + '/memory-candidates/' + reject['id'] + '/reject', {},
                    headers={'If-Match': '"' + str(reject['revision']) + '"'})
                check('rejection clears candidate body retains provenance and never writes memory',
                      rejected['status'] == 'rejected' and not rejected.get('content')
                      and rejected['source_message_ids'] == reject['source_message_ids']
                      and body_cleared(harness, reject['id']) and memory_count(harness) == count_before + 1)

                secret = 'password = SyntheticCandidatePassword987654321'
                filtered = harness.conversation('synthetic excluded-source fixtures')
                _, safe_id, _ = synthetic_run(harness, filtered, '合成普通资料 token budget = 4096')
                _, secret_id, _ = synthetic_run(harness, filtered, secret)
                _, control_id, _ = synthetic_run(harness, filtered, '/memory remember synthetic-control', kind='memory_command')
                _, character_id, _ = synthetic_run(harness, filtered, '/character synthetic-control', kind='character_command')
                _, incomplete_id, _ = synthetic_run(harness, filtered, '合成未完成资料', status='failed')
                filtered_state, filtered_tag = state(harness, filtered)
                offered = {item['id'] for item in filtered_state['sources']}
                check('secrets control commands and failed Runs never become eligible extraction sources',
                      offered == {safe_id} and filtered_state.get('redacted_source_count', 0) >= 1
                      and secret not in json.dumps(filtered_state))
                for excluded in [secret_id, control_id, character_id, incomplete_id]:
                    harness.call('POST', route(filtered) + '/extract', {'source_message_ids': [excluded]},
                                 expected=400, headers={'If-Match': filtered_tag})
                filtered_job, _, _ = start_extract(harness, mock, filtered)
                check('secret-like source data is absent from the actual extraction request',
                      wait_job(harness, filtered_job['id'])['status'] == 'completed'
                      and secret not in json.dumps(mock.payloads()[-1]) and safe_id in json.dumps(mock.payloads()[-1]))
                pending_before_restart = pending_items(harness, filtered)[0]

                calls = mock.count()
                group = harness.conversation('synthetic unsupported group source')
                with harness.db() as database:
                    database.execute('UPDATE conversations SET source_key=? WHERE id=?',
                                     ('qq:synthetic:group:synthetic-group', group))
                _, group_id, _ = synthetic_run(harness, group, '合成群聊资料')
                harness.call('GET', route(group), expected=403)
                harness.call('POST', route(group) + '/extract', {'source_message_ids': [group_id]},
                             expected=403, headers={'If-Match': '"0.0.0"'})
                draft, _, _ = harness.call('POST', PREFIX + '/character-cards', {})
                draft_path = (PREFIX + '/character-cards/' + draft['card_id'] +
                              '/drafts/' + draft['draft_id'])
                draft, _, _ = harness.call('POST', draft_path + '/bind-current-model',
                                          headers={'If-Match': draft['draft_etag']})
                test, _, _ = harness.call('POST', draft_path + '/test-conversations',
                                         headers={'If-Match': draft['draft_etag']})
                _, draft_id, _ = synthetic_run(harness, test['id'], '合成草稿测试资料')
                harness.call('GET', route(test['id']), expected=403)
                harness.call('POST', route(test['id']) + '/extract', {'source_message_ids': [draft_id]},
                             expected=403, headers={'If-Match': '"0.0.0"'})
                check('draft character tests and group sources refuse extraction without any model work',
                      mock.count() == calls)

                bounded = harness.conversation('synthetic source count and byte boundaries')
                old_ids = []
                for index in range(14):
                    _, fixture_id, _ = synthetic_run(harness, bounded, '合成安全短资料 ' + str(index))
                    old_ids.append(fixture_id)
                bounded_state, bounded_tag = state(harness, bounded)
                check('source selection exposes only the most recent twelve eligible user messages',
                      len(bounded_state['sources']) == 12
                      and {item['id'] for item in bounded_state['sources']} == set(old_ids[-12:]))
                harness.call('POST', route(bounded) + '/extract', {'source_message_ids': old_ids[-13:]},
                             expected=400, headers={'If-Match': bounded_tag})
                _, large_one, _ = synthetic_run(harness, bounded, '合成大资料甲 ' + '文' * 2300)
                _, large_two, _ = synthetic_run(harness, bounded, '合成大资料乙 ' + '文' * 2300)
                _, overlarge, _ = synthetic_run(harness, bounded, '合成超限资料 ' + '文' * 4000)
                byte_state, byte_tag = state(harness, bounded)
                offered = {item['id'] for item in byte_state['sources']}
                harness.call('POST', route(bounded) + '/extract', {'source_message_ids': [large_one, large_two]},
                             expected=400, headers={'If-Match': byte_tag})
                harness.call('POST', route(bounded) + '/extract', {'source_message_ids': [overlarge]},
                             expected=400, headers={'If-Match': byte_tag})
                harness.call('POST', route(bounded) + '/extract', {'source_message_ids': [large_one]},
                             expected=409, headers={'If-Match': bounded_tag})
                check('source count combined-byte and single-message limits fail before model work',
                      {large_one, large_two} <= offered and overlarge not in offered and mock.count() == calls)
                selected_job, _, selected_ids = start_extract(harness, mock, bounded, source_ids=[large_one])
                check('manual selection sends only explicitly chosen eligible source messages',
                      wait_job(harness, selected_job['id'])['status'] == 'completed'
                      and [item['id'] for item in requested_sources(mock.payloads()[-1])] == selected_ids)

                for mode in ['malformed', 'fenced', 'trailing', 'extra_top', 'extra_item', 'wrong_type',
                             'too_many', 'too_large', 'fake_source', 'empty_source', 'secret', 'failure']:
                    fixture, _ = source_turn(harness)
                    invalid_before = memory_count(harness)
                    calls = mock.count()
                    invalid, _, _ = start_extract(harness, mock, fixture, mode)
                    failed_job = wait_job(harness, invalid['id'])
                    invalid_state, _ = state(harness, fixture)
                    check('strict extractor rejects ' + mode + ' without memory or candidate body persistence',
                          failed_job['status'] == 'failed'
                          and re.fullmatch('[a-z][a-z0-9_]*', failed_job.get('error_code') or '') is not None
                          and invalid_state['items'] == [] and memory_count(harness) == invalid_before
                          and mock.count() == calls + 1)
                empty_chat, _ = source_turn(harness)
                empty_job, _, _ = start_extract(harness, mock, empty_chat, 'empty')
                check('valid empty extraction succeeds without inventing candidates',
                      wait_job(harness, empty_job['id'])['status'] == 'completed'
                      and state(harness, empty_chat)[0]['items'] == [])
                export, _, _ = harness.call('GET', PREFIX + '/memory/export')
                check('rejected secret-like model output is absent from memory export and daemon logs',
                      'sk-SyntheticCandidateSecret01234567890123456789' not in json.dumps(export)
                      and b'sk-SyntheticCandidateSecret01234567890123456789' not in
                      (harness.root / 'daemon.log').read_bytes())

                expiring, _ = source_turn(harness)
                expiry_job, _, _ = start_extract(harness, mock, expiring)
                wait_job(harness, expiry_job['id'])
                expired = pending_items(harness, expiring)[0]
                expire_candidate(harness, expired['id'])
                expiry_before = memory_count(harness)
                harness.call('POST', PREFIX + '/memory-candidates/' + expired['id'] + '/accept', {},
                             expected=409, headers={'If-Match': '"' + str(expired['revision']) + '"'})
                expired_view, _, _ = harness.call('GET', PREFIX + '/memory-candidates/' + expired['id'])
                check('expired review refuses approval and clears body while retaining source IDs',
                      expired_view['status'] == 'expired' and not expired_view.get('content')
                      and expired_view['source_message_ids'] == expired['source_message_ids']
                      and body_cleared(harness, expired['id']) and memory_count(harness) == expiry_before)

                stale_chat, _ = source_turn(harness)
                stale_job, _, stale_ids = start_extract(harness, mock, stale_chat)
                wait_job(harness, stale_job['id'])
                stale = pending_items(harness, stale_chat)[0]
                old_state, old_tag = state(harness, stale_chat)
                invalidate_source(harness, stale_chat, stale_ids[0])
                changed, changed_tag = state(harness, stale_chat)
                stale_before = memory_count(harness)
                harness.call('POST', PREFIX + '/memory-candidates/' + stale['id'] + '/accept', {},
                             expected=409, headers={'If-Match': '"' + str(stale['revision']) + '"'})
                check('forget invalidates pending candidate provenance and excludes pre-floor source history',
                      old_tag != changed_tag and changed['sources'] == []
                      and body_cleared(harness, stale['id']) and memory_count(harness) == stale_before)
                invalidate_source(harness, stale_chat, stale_ids[0])
                _, second_tag = state(harness, stale_chat)
                check('repeated invalidation changes epoch even when history floor does not advance',
                      second_tag != changed_tag and old_state['source_revision'] != changed['source_revision'])

                held_chat, _ = source_turn(harness)
                held_job, held_tag, held_ids = start_extract(harness, mock, held_chat, held=True)
                if not mock.hold_received.wait(timeout=10):
                    raise AssertionError('held candidate request was not observed')
                calls = mock.count()
                harness.call('POST', route(held_chat) + '/extract', {'source_message_ids': held_ids},
                             expected=409, headers={'If-Match': held_tag})
                invalidate_source(harness, held_chat, held_ids[0])
                mock.hold_release.set()
                held_done = wait_job(harness, held_job['id'])
                if not mock.hold_responded.wait(timeout=5):
                    raise AssertionError('held candidate response did not finish')
                check('in-flight extraction cannot publish after source epoch and floor invalidation',
                      held_done['status'] in {'cancelled', 'failed', 'interrupted'}
                      and state(harness, held_chat)[0]['items'] == [] and mock.count() == calls)

                cancel_chat, _ = source_turn(harness)
                cancel_job, _, _ = start_extract(harness, mock, cancel_chat, held=True)
                if not mock.hold_received.wait(timeout=10):
                    raise AssertionError('cancelled candidate request was not observed')
                calls = mock.count()
                parallel_chat, _ = source_turn(harness)
                queued_job, _, _ = start_extract(harness, mock, parallel_chat)
                queued_path = PREFIX + '/memory-candidate-jobs/' + queued_job['id']
                queued_now, _, _ = harness.call('GET', queued_path)
                harness.call('DELETE', queued_path,
                             headers={'If-Match': '"' + str(queued_now['revision']) + '"'})
                check('normal chat retains a native worker slot while another extraction queues without dispatch',
                      mock.count() == calls and wait_job(harness, queued_job['id'])['status'] == 'cancelled')
                queued_expiry_chat, _ = source_turn(harness)
                queued_expiry, _, _ = start_extract(harness, mock, queued_expiry_chat)
                with harness.db() as database:
                    database.execute('UPDATE memory_candidate_jobs SET expires_at=? WHERE id=?',
                                     (int(time.time()) - 1, queued_expiry['id']))
                expired_job = wait_job(harness, queued_expiry['id'])
                check('expired queued extraction never dispatches its stale source payload',
                      expired_job['status'] == 'expired' and mock.count() == calls
                      and state(harness, queued_expiry_chat)[0]['items'] == [])
                job_path = PREFIX + '/memory-candidate-jobs/' + cancel_job['id']
                current, _, _ = harness.call('GET', job_path)
                harness.call('DELETE', job_path, expected=428)
                harness.call('DELETE', job_path, expected=(400, 409), headers={'If-Match': '"0"'})
                harness.call('DELETE', job_path, headers={'If-Match': '"' + str(current['revision']) + '"'})
                mock.hold_release.set()
                cancelled = wait_job(harness, cancel_job['id'])
                if not mock.hold_responded.wait(timeout=5):
                    raise AssertionError('cancelled mock HTTP handler did not release')
                check('explicit job cancellation uses current revision and never retries dispatched model work',
                      cancelled['status'] == 'cancelled' and state(harness, cancel_chat)[0]['items'] == [])

                restart_chat, _ = source_turn(harness)
                restart_job, _, _ = start_extract(harness, mock, restart_chat, held=True)
                if not mock.hold_received.wait(timeout=10):
                    raise AssertionError('restart-boundary request was not observed')
                restart_calls = mock.count()
                harness.stop()
                mock.hold_release.set()
                mock.hold_responded.wait(timeout=5)
                harness.start()
                harness.login()
                restarted = wait_job(harness, restart_job['id'])
                check('daemon restart interrupts dispatched extraction without automatic model retry',
                      restarted['status'] in {'interrupted', 'cancelled'}
                      and mock.count() == restart_calls and state(harness, restart_chat)[0]['items'] == [])
                restored, _, _ = harness.call('GET', candidate_path)
                check('accepted provenance and its single private memory survive daemon restart',
                      restored['status'] == 'accepted' and not restored.get('content')
                      and restored['memory_id'] == accepted['memory_id']
                      and body_cleared(harness, candidate['id']))
                pending_restored, _, _ = harness.call(
                    'GET', PREFIX + '/memory-candidates/' + pending_before_restart['id'])
                check('unexpired pending review survives restart without silent approval or content loss',
                      pending_restored['status'] == 'pending'
                      and pending_restored['content'] == pending_before_restart['content']
                      and pending_restored['source_message_ids'] == pending_before_restart['source_message_ids'])
                check('extractor shares bounded native slots and never creates parallel extractor calls',
                      mock.maximum_extract <= 1 and mock.maximum <= 2)
                report['status'] = 'passed'
            finally:
                harness.close()
    except Exception as error:
        failed = True
        report.update(status='failed', failure_after_check=stage, error_type=type(error).__name__)
        trace = error.__traceback__
        while trace:
            if trace.tb_frame.f_code.co_filename == __file__:
                report['failure_line'] = trace.tb_lineno
            trace = trace.tb_next
        if isinstance(error, ResponseStatusError):
            report.update(actual_http_status=error.actual, expected_http_statuses=error.expected)
        print('FAIL isolated candidate acceptance; inspect sanitized check metadata', flush=True)
    finally:
        mock.hold_release.set()
        provider.shutdown()
        provider.server_close()
        report.update(finished_at=acceptance.utc_now(), passed=len(report['checks']),
                      mock_extract_requests=mock.count(), observed_max_model_requests=mock.maximum,
                      observed_max_extract_requests=mock.maximum_extract)
        if args.report:
            destination = pathlib.Path(args.report).resolve()
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps({'status': report['status'], 'passed': report['passed'],
                          'real_external_model_tested': False, 'production_data_accessed': False}))
    return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
