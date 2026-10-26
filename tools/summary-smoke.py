#!/usr/bin/env python3
"""Real-daemon summary acceptance with an isolated loopback model, never real QQ.

The long conversation uses HTTP and the actual worker. A few boundary cases use
explicitly synthetic source rows in the temporary database. Reports contain
check names and release metadata, never source text, responses, or credentials.
"""
import argparse
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
import uuid

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    'summary_memory_harness', pathlib.Path(__file__).with_name('memory-smoke.py'))
acceptance = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(acceptance)
PREFIX = acceptance.PREFIX


class Mock:
    def __init__(self):
        self.lock = threading.Lock()
        self.requests = []
        self.canaries = []
        self.summary_mode = 'normal'
        self.hold_release = threading.Event()
        self.hold_received = threading.Event()
        self.hold_next_summary = False
        self.active = 0
        self.active_summary = 0
        self.maximum = 0
        self.maximum_summary = 0

    @staticmethod
    def is_summary(payload):
        return payload['messages'][0]['role'] == 'system' and '对话资料压缩器' in payload['messages'][0]['content']

    @staticmethod
    def question(payload):
        return next(item['content'] for item in reversed(payload['messages']) if item['role'] == 'user')

    def for_question(self, question):
        with self.lock:
            matches = [payload for payload in self.requests
                       if not self.is_summary(payload) and self.question(payload) == question]
        if len(matches) != 1:
            raise AssertionError('primary model request was not observed exactly once')
        return matches[0]

    def summaries(self):
        with self.lock:
            return [payload for payload in self.requests if self.is_summary(payload)]

    def count(self):
        with self.lock:
            return len(self.requests)


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        mock = self.server.mock
        size = int(self.headers.get('Content-Length', '0'))
        if not 0 < size <= 1024 * 1024:
            self.send_error(413)
            return
        payload = json.loads(self.rfile.read(size))
        summary = mock.is_summary(payload)
        question = mock.question(payload)
        with mock.lock:
            mock.requests.append(payload)
            mock.active += 1
            mock.active_summary += int(summary)
            mock.maximum = max(mock.maximum, mock.active)
            mock.maximum_summary = max(mock.maximum_summary, mock.active_summary)
            mode = mock.summary_mode
            held = summary and mock.hold_next_summary
            if held:
                mock.hold_next_summary = False
        if held:
            mock.hold_received.set()
            mock.hold_release.wait(timeout=25)
        serialized = json.dumps(payload, ensure_ascii=False)
        canaries = [canary for canary in mock.canaries if canary in serialized]
        if summary:
            content = '摘要：' + ('早期约定的合成代号是' + '|'.join(canaries) if canaries else '目前没有可引用的验收代号。')
            if mode == 'secretlike':
                content = 'api_key = sk-abcdefghijklmnopqrstuvwxyz0123456789'
            status = 503 if mode == 'failure' else 200
        elif question.startswith('OVERSIZED '):
            content, status = '起点' + '鲸' * 12000 + '终点', 200
        elif question.startswith('RECALL '):
            content, status = ('KNOWN:' + '|'.join(canaries) if canaries else 'UNKNOWN'), 200
        else:
            # Filler responses never echo the canary. Later recall must come
            # from the summary, not a recent assistant repetition of the fact.
            content, status = 'FILLER-ACK', 200
        raw = json.dumps({'choices': [{'message': {'role': 'assistant', 'content': content}}]}).encode()
        with mock.lock:
            mock.active -= 1
            mock.active_summary -= int(summary)
        try:
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass


def snapshot(harness, run):
    with harness.db() as db:
        row = db.execute('SELECT messages_json FROM run_prompt_snapshots WHERE run_id=?', (run,)).fetchone()
    if row is None:
        raise AssertionError('completed model run has no frozen prompt')
    return json.loads(row[0])


def summary_state(harness, conversation):
    return harness.request('GET', PREFIX + '/conversations/' + conversation + '/summary')


class ModelTurnFailure(AssertionError):
    def __init__(self, metadata):
        super().__init__('isolated synthetic model turn failed')
        self.metadata = metadata


def turn(harness, conversation, question):
    run = harness.submit(conversation, question)
    try:
        result = harness.wait_run(run)
    except Exception as error:
        with harness.db() as db:
            row = db.execute('SELECT status,error FROM runs WHERE id=?', (run,)).fetchone()
        raise ModelTurnFailure({
            'phase': 'ordinary_model_turn', 'question_kind': question.split(' ', 1)[0],
            'run_id': run, 'run_status': row['status'] if row else 'missing',
            'host_error': (row['error'] or '')[:180] if row else None,
            'wait_error_type': type(error).__name__,
        }) from error
    if result['status'] != 'completed':
        raise ModelTurnFailure({
            'phase': 'ordinary_model_turn', 'question_kind': question.split(' ', 1)[0],
            'run_id': run, 'run_status': result['status'],
            'host_error': (result.get('error') or '')[:180],
        })
    return run, result, harness.mock.for_question(question)


def summary_message(messages):
    matches = [item for item in messages
               if item['role'] == 'user' and '旧对话摘要' in item['content']]
    return matches[0] if len(matches) == 1 else None


def seed_sources(harness, conversation, owner, count, prefix):
    """Synthetic old-source fixtures, only inside Harness.TemporaryDirectory."""
    ids = []
    with harness.db() as db:
        for index in range(count):
            run = str(uuid.uuid4())
            question = prefix + ' source ' + str(index)
            timestamp = int(time.time())
            db.execute('INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,'
                       'status,response,created_at,updated_at,run_kind) VALUES(?,?,?,?,?,?,\'completed\',\'ACK\',?,?,\'model\')',
                       (run, conversation, owner, run, question, json.dumps(harness.applied), timestamp, timestamp))
            for role, content in [('user', question), ('assistant', 'ACK')]:
                db.execute('INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?,?,?,?,?,?)',
                           (str(uuid.uuid4()), conversation, run, role, content, timestamp))
            ids.append(run)
    return ids


def seed_historical_oversized_source(harness, conversation, question):
    """Import a synthetic legacy transcript; do not weaken new-reply limits."""
    run = str(uuid.uuid4())
    response = '起点' + '鲸' * 12000 + '终点'
    timestamp = int(time.time())
    with harness.db() as db:
        db.execute('INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,'
                   'status,response,created_at,updated_at,run_kind) '
                   'VALUES(?,?,?,?,?,?,\'completed\',?,?,?,\'model\')',
                   (run, conversation, harness.owner, 'historical-oversized-fixture:' + run,
                    question, json.dumps(harness.applied),
                    response, timestamp, timestamp))
        for role, content in [('user', question), ('assistant', response)]:
            db.execute('INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?,?,?,?,?,?)',
                       (str(uuid.uuid4()), conversation, run, role, content, timestamp))
        rowid = db.execute('SELECT rowid FROM runs WHERE id=?', (run,)).fetchone()[0]
    return {'kind': 'synthetic historical oversized transcript imported into temporary database',
            'run_id': run, 'run_rowid': rowid, 'response_bytes': len(response.encode()),
            'response_characters': len(response), 'fresh_model_generation': False}


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
    report = {'kind': 'conversation-summary real-process acceptance with isolated mock',
              'started_at': acceptance.utc_now(), 'commit': None, 'binary_sha256': binary_hash,
              'platform': platform.system(), 'architecture': platform.machine(), 'status': 'running',
              'real_external_model_tested': False, 'real_qq_tested': False,
              'production_data_accessed': False, 'synthetic_source_fixtures_used': True, 'checks': []}
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
        with tempfile.TemporaryDirectory(prefix='dsh-summary-smoke-') as temporary:
            harness = acceptance.Harness(str(binary), str(web), temporary, mock)
            try:
                harness.start()
                report['commit'] = harness.commit
                harness.bootstrap()
                harness.configure(provider.server_port)
                suffix = secrets.token_hex(8)
                canary = 'SUMMARYFACT' + suffix.upper()
                mock.canaries.append(canary)
                chat = harness.conversation('synthetic long conversation')
                route = PREFIX + '/conversations/' + chat + '/summary'
                harness.request('GET', route, expected=401, authenticated=False)
                harness.request('POST', route + '/rebuild', {}, expected=403, with_csrf=False)
                check('summary management enforces authentication and CSRF', True)
                first_question = 'FACT ' + canary
                first, _, _ = turn(harness, chat, first_question)
                secret_fixture = 'password = SyntheticPassword987654321'
                questions = [first_question]
                for index in range(15):
                    question = 'FILLER ' + str(index) + ' ' + suffix
                    if index == 1:
                        question += ' ' + secret_fixture
                    elif index == 2:
                        question += ' token budget = 4096'
                    elif index == 3:
                        question = 'OVERSIZED ' + suffix
                    questions.append(question)
                    if index == 3:
                        report['historical_oversized_source_fixture'] = seed_historical_oversized_source(
                            harness, chat, question)
                    else:
                        turn(harness, chat, question)
                recalled_run, recalled, payload = turn(harness, chat, 'RECALL long-window ' + suffix)
                state, _, tag = summary_state(harness, chat)
                summary = state['summary']
                check('actual worker summarizes a conversation longer than the original 24-message window',
                      len(questions) >= 16 and summary is not None and canary in recalled['response'])
                envelope = summary_message(payload['messages'])
                check('early fact reaches the model as user-role private summary data',
                      envelope is not None and canary in envelope['content'] and payload['messages'][0]['role'] == 'system')
                with harness.db() as db:
                    through = db.execute('SELECT v.through_run_rowid FROM run_summary_uses u JOIN conversation_summary_versions v '
                                         'ON v.conversation_id=u.conversation_id AND v.revision=u.summary_revision WHERE u.run_id=?', (recalled_run,)).fetchone()[0]
                    covered = [row[0] for row in db.execute('SELECT input FROM runs WHERE conversation_id=? AND run_kind=\'model\' AND rowid<=?', (chat, through))]
                    source_message = db.execute('SELECT id FROM messages WHERE run_id=? AND role=\'user\'', (first,)).fetchone()[0]
                raw_contents = [message['content'] for message in payload['messages']]
                check('covered original user turns are not repeated alongside their summary',
                      first_question not in raw_contents and all(text not in raw_contents for text in covered))
                check('summary revision usage and exact prompt snapshot persist atomically',
                      snapshot(harness, recalled_run) == payload['messages'])
                batches = [json.loads(item['messages'][1]['content']) for item in mock.summaries()]
                check('source batches and retained summaries obey fixed byte and turn budgets',
                      all(len(batch['new_source_turns']) <= 8
                          and len(json.dumps(batch['new_source_turns'], ensure_ascii=False, separators=(',', ':')).encode()) <= 24000
                          and len((batch.get('previous_summary') or '').encode()) <= 6000 for batch in batches))
                check('secret-like source bodies are omitted with auditable redaction counts',
                      summary['redacted_source_count'] >= 1 and all(secret_fixture not in json.dumps(batch) for batch in batches))
                check('ordinary token-budget discussion remains available to compaction',
                      any('token budget = 4096' in json.dumps(batch) for batch in batches))
                historical = report['historical_oversized_source_fixture']
                historical_turns = [item for batch in batches for item in batch['new_source_turns']
                                    if item['run_id'] == historical['run_id']]
                check('oversized source turns advance via explicitly marked bounded head-tail excerpts',
                      historical['response_bytes'] > 24000 and summary['truncated_source_count'] >= 1
                      and len(historical_turns) == 1 and historical_turns[0].get('truncated')
                      and historical_turns[0].get('omitted_bytes', 0) > 0
                      and '起点' in historical_turns[0].get('assistant', '')
                      and '终点' in historical_turns[0].get('assistant', '')
                      and len(historical_turns[0]['assistant'].encode()) <= 1600
                      and through >= historical['run_rowid'])
                version_before = summary['revision']
                harness.stop(); harness.start(); harness.login()
                _, restored, restored_payload = turn(harness, chat, 'RECALL persisted-summary ' + suffix)
                restored_state, _, _ = summary_state(harness, chat)
                check('daemon restart retains the summary and recalls the early fact',
                      canary in restored['response'] and restored_state['summary']['revision'] >= version_before
                      and summary_message(restored_payload['messages']) is not None)
                another = harness.conversation('same owner different conversation')
                _, isolated, isolated_payload = turn(harness, another, 'RECALL other-conversation ' + suffix)
                other_state, _, _ = summary_state(harness, another)
                check('summary remains private to its source conversation even for the same owner',
                      isolated['response'] == 'UNKNOWN' and canary not in json.dumps(isolated_payload)
                      and other_state['summary'] is None)
                other_owner = 'qq:synthetic:c2c:another_owner'
                foreign = str(uuid.uuid4())
                _, foreign_result = harness.simulated_owner_turn(foreign, other_owner, 'RECALL other-owner ' + suffix)
                harness.request('GET', PREFIX + '/conversations/' + foreign + '/summary', expected=404)
                check('another source owner cannot inspect or receive the private summary', foreign_result['response'] == 'UNKNOWN')
                state, _, headers = summary_state(harness, chat)
                old_tag = headers['ETag']
                with harness.db() as db:
                    count_before = db.execute('SELECT count(*) FROM messages WHERE conversation_id=?', (chat,)).fetchone()[0]
                harness.request('POST', route + '/rebuild', {}, expected=428)
                harness.request('POST', route + '/rebuild', {}, expected=409, headers={'If-Match': '"stale"'})
                rebuilt, _, _ = harness.request('POST', route + '/rebuild', {}, expected=202, headers={'If-Match': old_tag})
                harness.request('POST', route + '/rebuild', {}, expected=409, headers={'If-Match': old_tag})
                check('manual rebuild uses an opaque epoch-aware ETag and rejects stale writes',
                      rebuilt['status'] == 'pending_next_run' and rebuilt['revision'] != state['revision'])
                with harness.db() as db:
                    count_after = db.execute('SELECT count(*) FROM messages WHERE conversation_id=?', (chat,)).fetchone()[0]
                check('manual rebuild clears only derived summary and preserves original messages',
                      count_before == count_after and summary_state(harness, chat)[0]['summary'] is None)
                _, rebuilt_result, _ = turn(harness, chat, 'RECALL rebuilt-summary ' + suffix)
                check('next real worker run rebuilds an invalidated derived summary', canary in rebuilt_result['response'])
                memory, _, _ = harness.request('POST', PREFIX + '/memory', {
                    'content': '合成早期代号为' + canary, 'visibility': 'private',
                    'source_conversation_id': chat, 'source_message_id': source_message}, expected=201)
                use_run, _, _ = turn(harness, chat, 'RECALL explicit-provenance ' + suffix)
                with harness.db() as db:
                    used = db.execute('SELECT count(*) FROM memory_run_uses WHERE run_id=? AND memory_id=?', (use_run, memory['id'])).fetchone()[0]
                harness.request('DELETE', PREFIX + '/memory/' + memory['id'], headers={'If-Match': '"1"'})
                check('forget clears the derived summary through real source and memory-use provenance',
                      used == 1 and summary_state(harness, chat)[0]['summary'] is None)
                _, forgotten, forgotten_payload = turn(harness, chat, 'RECALL after-forget ' + suffix)
                check('forgotten source and old assistant echoes disappear from future prompts',
                      forgotten['response'] == 'UNKNOWN' and canary not in json.dumps(forgotten_payload))
                harness.stop(); harness.start(); harness.login()
                for index in range(8):
                    turn(harness, chat, 'CLEAN post-forget ' + str(index) + ' ' + suffix)
                _, clean, clean_payload = turn(harness, chat, 'RECALL rebuilt-after-forget ' + suffix)
                clean_summary = summary_state(harness, chat)[0]['summary']
                check('restart and a fresh summary cannot reconstruct pre-forget facts',
                      clean['response'] == 'UNKNOWN' and clean_summary is not None
                      and canary not in clean_summary['content'] and canary not in json.dumps(clean_payload))
                with harness.db() as db:
                    retained = db.execute('SELECT content FROM messages WHERE id=?', (source_message,)).fetchone()[0]
                check('forgetting future context does not falsely claim original-message erasure', canary in retained)
                bad_output = harness.conversation('synthetic output blocking fixture')
                seed_sources(harness, bad_output, harness.owner, 8, 'safe source')
                mock.summary_mode = 'secretlike'
                turn(harness, bad_output, 'FALLBACK output-blocking ' + suffix)
                bad_state = summary_state(harness, bad_output)[0]
                check('secret-like model summary output is rejected without failing normal chat',
                      bad_state['summary'] is None and bad_state['last_job']['error_code'] == 'invalid_output')
                failure = harness.conversation('synthetic provider failure fixture')
                seed_sources(harness, failure, harness.owner, 8, 'fallback recent transcript')
                mock.summary_mode = 'failure'
                fallback_run, fallback_result, fallback_payload = turn(harness, failure, 'FALLBACK provider-failure ' + suffix)
                failure_state = summary_state(harness, failure)[0]
                check('summary provider failure falls back to the original recent-message window',
                      fallback_result['status'] == 'completed' and failure_state['last_job']['error_code'] == 'model_failed'
                      and summary_message(fallback_payload['messages']) is None
                      and any('fallback recent transcript' in item['content'] for item in snapshot(harness, fallback_run)))
                mock.summary_mode = 'normal'
                held_chat = harness.conversation('synthetic in-flight invalidation fixture')
                held_canary = 'PENDINGFACT' + suffix.upper()
                mock.canaries.append(held_canary)
                seed_sources(harness, held_chat, harness.owner, 8, held_canary)
                held_memory, _, _ = harness.request('POST', PREFIX + '/memory', {
                    'content': held_canary, 'visibility': 'private', 'source_conversation_id': held_chat}, expected=201)
                mock.hold_next_summary = True
                held_run = harness.submit(held_chat, 'RECALL held-summary ' + suffix)
                if not mock.hold_received.wait(timeout=10):
                    raise AssertionError('in-flight summary request was not observed')
                parallel = harness.conversation('synthetic busy summary slot fixture')
                seed_sources(harness, parallel, harness.owner, 8, 'safe parallel source')
                _, parallel_result, parallel_payload = turn(harness, parallel, 'FALLBACK busy-summary-slot ' + suffix)
                parallel_state = summary_state(harness, parallel)[0]
                check('busy global summary slot lets another native Run finish using its original window',
                      parallel_result['status'] == 'completed' and not mock.hold_release.is_set()
                      and parallel_state['summary'] is None and parallel_state['last_job'] is None
                      and summary_message(parallel_payload['messages']) is None)
                harness.request('DELETE', PREFIX + '/memory/' + held_memory['id'], headers={'If-Match': '"1"'})
                mock.hold_release.set()
                held_result = harness.wait_run(held_run)
                _, held_after, held_payload = turn(harness, held_chat, 'RECALL cancelled-stale-summary ' + suffix)
                check('forgetting cancels in-flight compaction and rejects its stale result',
                      held_result['status'] == 'cancelled' and held_after['response'] == 'UNKNOWN'
                      and held_canary not in json.dumps(held_payload) and summary_state(harness, held_chat)[0]['summary'] is None)
                with harness.db() as db:
                    duplicates = db.execute('SELECT count(*) FROM (SELECT trigger_run_id FROM summary_jobs GROUP BY trigger_run_id HAVING count(*)>1)').fetchone()[0]
                    running = db.execute('SELECT count(*) FROM runs WHERE status=\'running\'').fetchone()[0]
                    attempted = db.execute('SELECT count(*) FROM summary_jobs').fetchone()[0]
                check('summary requests obey one attempt per Run and the bounded native worker slots',
                      duplicates == 0 and len(mock.summaries()) == attempted
                      and mock.maximum_summary <= 1 and mock.maximum <= 2 and running == 0)
                report['status'] = 'passed'
            finally:
                harness.close()
    except Exception as error:
        failed = True
        report.update(status='failed', failure_after_check=stage, error_type=type(error).__name__)
        if isinstance(error, ModelTurnFailure):
            report['failure_phase'] = error.metadata['phase']
            report['failed_synthetic_turn'] = error.metadata
        print('FAIL isolated summary acceptance; inspect check metadata', flush=True)
    finally:
        mock.hold_release.set()
        provider.shutdown(); provider.server_close()
        report.update(finished_at=acceptance.utc_now(), passed=len(report['checks']),
                      mock_model_requests=mock.count(), mock_summary_requests=len(mock.summaries()),
                      observed_max_model_requests=mock.maximum, observed_max_summary_requests=mock.maximum_summary)
        if args.report:
            destination = pathlib.Path(args.report).resolve()
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps({'status': report['status'], 'passed': report['passed'],
                          'real_external_model_tested': False, 'real_qq_tested': False}))
    return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
