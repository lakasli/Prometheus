#!/usr/bin/env python3
"""Single-assistant expression and time-context acceptance with isolated data.

Runs a real daemon against an ephemeral loopback provider. Synthetic SQLite
timestamps exercise historical continuity without changing the host clock.
This verifies request construction, authorization and persistence; it does not
prove that an external model sounds natural or that QQ delivered a message.
Reports contain release metadata and check names, never prompts or credentials.
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
import uuid


sys.dont_write_bytecode = True


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, pathlib.Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


base = module('presence_memory_harness', 'memory-smoke.py')
owners = module('presence_owner_harness', 'memory-candidate-smoke.py')
API = base.PREFIX
PERSONALITY = API + '/assistant/personality'
TIME_MARKER = 'DSH_CONVERSATION_TIME_V1'


class MockState(base.MockState):
    def __init__(self):
        super().__init__()
        self.snapshot_probe = None
        self.snapshots_at_dispatch = {}


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        size = int(self.headers.get('Content-Length', '0'))
        if not 0 < size <= 1024 * 1024:
            self.send_error(413)
            return
        payload = json.loads(self.rfile.read(size))
        state = self.server.mock_state
        question = state.question(payload)
        snapshot = state.snapshot_probe(question) if state.snapshot_probe else None
        with state.condition:
            state.requests.append(payload)
            state.snapshots_at_dispatch[question] = snapshot
            state.condition.notify_all()
        raw = json.dumps({'choices': [{'message': {
            'role': 'assistant', 'content': 'SYNTHETIC-PRESENCE-ACK',
        }}], 'usage': {'prompt_tokens': 8, 'completion_tokens': 8}}).encode()
        try:
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass


def system_text(payload):
    return '\n'.join(item['content'] for item in payload['messages'] if item['role'] == 'system')


def time_context(payload):
    contexts = []
    for message in payload['messages']:
        if message['role'] != 'system' or TIME_MARKER not in message['content']:
            continue
        following = message['content'].split(TIME_MARKER, 1)[1]
        start = following.find('{')
        if start >= 0:
            try:
                candidate, _ = json.JSONDecoder().raw_decode(following[start:])
            except json.JSONDecodeError:
                continue
            if isinstance(candidate, dict):
                contexts.append(candidate)
    if len(contexts) != 1:
        raise AssertionError('exactly one server time context must be present')
    return contexts[0]


def prompt_snapshot(harness, run_id):
    with harness.db() as database:
        row = database.execute(
            'SELECT messages_json,digest,compiler_version,created_at '
            'FROM run_prompt_snapshots WHERE run_id=?', (run_id,)).fetchone()
    if row is None:
        raise AssertionError('durable prompt snapshot is missing')
    return dict(row)


def record_counts(harness):
    with harness.db() as database:
        return {name: database.execute('SELECT count(*) FROM ' + name).fetchone()[0]
                for name in ('memory_records', 'memory_candidates', 'messages')}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--personality', default=str(
        pathlib.Path(__file__).resolve().parents[1] / 'docs/examples/assistant-personality-natural-v1.json'))
    parser.add_argument('--report')
    args = parser.parse_args()
    binary = str(pathlib.Path(args.binary).resolve(strict=True))
    web = str(pathlib.Path(args.web_dir).resolve(strict=True))
    asset_path = pathlib.Path(args.personality).resolve(strict=True)
    document = json.loads(asset_path.read_text(encoding='utf-8-sig'))
    personality = copy.deepcopy(document)
    if not isinstance(personality, dict) or not personality.get('mes_example', '').strip():
        raise AssertionError('natural personality artifact must contain expression examples')
    report = {
        'kind': 'single-assistant expression and trusted time isolated black-box acceptance',
        'status': 'running', 'started_at': base.utc_now(),
        'platform': platform.system(), 'architecture': platform.machine(),
        'binary_sha256': hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest(),
        'personality_asset_sha256': hashlib.sha256(asset_path.read_bytes()).hexdigest(),
        'commit': None, 'checks': [], 'production_data_accessed': False,
        'external_model_tested': False, 'naturalness_tested': False,
        'real_qq_tested': False, 'real_weixin_tested': False,
        'synthetic_historical_timestamps': True,
    }
    stage = 'isolated setup'

    def check(name, condition=True):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    mock = MockState()
    provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    provider.daemon_threads = True
    provider.mock_state = mock
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-conversation-presence-') as temporary:
            h = base.Harness(binary, web, temporary, mock)
            try:
                h.start()
                report['commit'] = h.commit
                h.request('GET', PERSONALITY, expected=401, authenticated=False)
                check('single personality requires authentication')
                h.bootstrap()
                h.configure(provider.server_port)

                def snapshot_probe(question):
                    with h.db() as database:
                        row = database.execute(
                            'SELECT p.messages_json,p.digest FROM run_prompt_snapshots p '
                            'JOIN runs r ON r.id=p.run_id WHERE r.input=? AND r.status=?',
                            (question, 'running')).fetchone()
                    return dict(row) if row else None

                mock.snapshot_probe = snapshot_probe
                current, _, _ = h.request('GET', PERSONALITY)
                h.request('PUT', PERSONALITY, {'personality': personality}, expected=403,
                          with_csrf=False, headers={'If-Match': current['etag']})
                h.request('PUT', PERSONALITY, {'personality': personality}, expected=403,
                          headers={'If-Match': current['etag'], 'Origin': 'https://example.invalid'})
                h.request('PUT', PERSONALITY, {'personality': personality}, expected=428)
                check('personality changes require CSRF exact Origin and If-Match')
                before = record_counts(h)
                saved, _, _ = h.request('PUT', PERSONALITY, {'personality': personality},
                                        headers={'If-Match': current['etag']})
                h.request('PUT', PERSONALITY, {'personality': personality}, expected=409,
                          headers={'If-Match': current['etag']})
                after = record_counts(h)
                check('natural personality artifact saves with optimistic concurrency',
                      saved['personality'] == personality and saved['saved'] is True
                      and saved['applied'] is True and saved['effective_from'] == 'next_run')
                check('saving expression examples creates no messages memories or candidates', before == after)

                suffix = secrets.token_hex(8)
                example_question = 'EXPRESSION_QUESTION_' + suffix
                example_answer = 'EXPRESSION_ANSWER_' + suffix
                personality['mes_example'] += (
                    '\n<START>\n{{user}}: ' + example_question +
                    '\n{{char}}: ' + example_answer)
                saved, _, _ = h.request('PUT', PERSONALITY, {'personality': personality},
                                        headers={'If-Match': saved['etag']})
                conversation = h.conversation('synthetic time and expression continuity')
                h.request('POST', API + '/conversations/' + conversation + '/messages', {
                    'content': 'synthetic forged time', 'client_message_id': str(uuid.uuid4()),
                    'current_user_message_at': '2099-01-01T00:00:00+08:00'}, expected=422)
                check('client cannot supply trusted message timestamps through the messages API')
                first_question = 'PRIVATE_FIRST_TURN_' + suffix
                client_id = str(uuid.uuid4())
                clock_before = int(time.time())
                first_run = h.submit(conversation, first_question, client_id)
                result = h.wait_run(first_run)
                clock_after = int(time.time())
                check('ordinary conversation completes through actual loopback HTTP provider',
                      result['status'] == 'completed')
                first_payload = mock.for_question(first_question)
                first_system = system_text(first_payload)
                check('saved expression examples reach the ordinary model system prompt',
                      example_question in first_system and example_answer in first_system
                      and '虚构' in first_system and '不是历史事实' in first_system)
                check('examples never become fake historical model turns',
                      all(example_question not in message['content'] and
                          example_answer not in message['content']
                          for message in first_payload['messages'] if message['role'] != 'system')
                      and first_payload['messages'][-1] == {'role': 'user', 'content': first_question})
                first_time = time_context(first_payload)
                # Time-field assertions below are intentionally pinned to the server contract.
                validate_initial_time(first_time, clock_before, clock_after)
                check('ordinary model request receives server-generated current time and timezone')
                snapshot = prompt_snapshot(h, first_run)
                dispatched = mock.snapshots_at_dispatch[first_question]
                check('complete time and expression prompt is persisted before network dispatch',
                      dispatched is not None and dispatched['messages_json'] == snapshot['messages_json']
                      and json.loads(snapshot['messages_json']) == first_payload['messages']
                      and hashlib.sha256(snapshot['messages_json'].encode()).hexdigest() == snapshot['digest'])

                count_before = mock.count()
                repeated_run = h.submit(conversation, first_question, client_id)
                check('duplicate client message reuses frozen prompt without another model request',
                      repeated_run == first_run and mock.count() == count_before
                      and prompt_snapshot(h, first_run) == snapshot)
                with h.db() as database:
                    historical_at = int(time.time()) - 25 * 3600
                    database.execute('UPDATE messages SET created_at=? WHERE run_id=?',
                                     (historical_at, first_run))
                    database.execute('UPDATE runs SET created_at=?,updated_at=? WHERE id=?',
                                     (historical_at, historical_at, first_run))
                clock_before = int(time.time())
                second_run, second_payload = h.model_turn(conversation, 'continue-next-day-' + suffix)
                clock_after = int(time.time())
                second_time = time_context(second_payload)
                validate_history_time(second_time, historical_at, clock_before, clock_after)
                check('same conversation exposes real historical timestamps and cross-day elapsed time')
                check('same conversation still includes the preceding real user turn',
                      any(message['role'] == 'user' and message['content'] == first_question
                          for message in second_payload['messages']))
                second_snapshot = prompt_snapshot(h, second_run)

                different_conversation = h.conversation('separate synthetic time source')
                _, separate_payload = h.model_turn(different_conversation, 'separate-conversation-' + suffix)
                validate_initial_time(time_context(separate_payload), int(time.time()) - 10, int(time.time()))
                check('new conversation does not inherit unrelated conversation time or messages',
                      first_question not in base.payload_text(separate_payload))
                with owners.other_admin(h):
                    h.request('GET', API + '/conversations/' + conversation + '/messages', expected=404)
                    foreign = h.conversation('different synthetic administrator')
                    _, foreign_payload = h.model_turn(foreign, 'other-owner-' + suffix)
                    validate_initial_time(time_context(foreign_payload), int(time.time()) - 10, int(time.time()))
                    check('different owner cannot inspect or inherit private history or its time context',
                          first_question not in base.payload_text(foreign_payload))

                with h.db() as database:
                    fictional_rows = database.execute(
                        'SELECT count(*) FROM messages WHERE instr(content,?)>0 OR '
                        'instr(content,?)>0 OR instr(content,?)>0',
                        (example_question, example_answer, TIME_MARKER)).fetchone()[0]
                    memory_rows = database.execute('SELECT count(*) FROM memory_records').fetchone()[0]
                    candidate_rows = database.execute('SELECT count(*) FROM memory_candidates').fetchone()[0]
                check('expression and time context never become persisted dialogue or long-term facts',
                      fictional_rows == 0 and memory_rows == 0 and candidate_rows == 0)

                calls_before_restart = mock.count()
                h.stop()
                h.start()
                h.login()
                restored, _, _ = h.request('GET', PERSONALITY)
                check('saved personality and completed time snapshots survive daemon restart',
                      restored['etag'] == saved['etag'] and restored['personality'] == personality
                      and prompt_snapshot(h, first_run) == snapshot
                      and prompt_snapshot(h, second_run) == second_snapshot
                      and mock.count() == calls_before_restart)
                resumed_run, resumed_payload = h.model_turn(conversation, 'after-restart-' + suffix)
                resumed_time = time_context(resumed_payload)
                validate_resumed_time(resumed_time)
                check('new turn after restart continues the same real conversation time context',
                      first_question in base.payload_text(resumed_payload))
                with h.db() as database:
                    future_at = int(time.time()) + 25 * 3600
                    database.execute('UPDATE messages SET created_at=? WHERE run_id=?',
                                     (future_at, resumed_run))
                    database.execute('UPDATE runs SET created_at=?,updated_at=? WHERE id=?',
                                     (future_at, future_at, resumed_run))
                _, future_payload = h.model_turn(conversation, 'future-clock-boundary-' + suffix)
                future_context = time_context(future_payload)
                check('future or backward-clock history cannot produce a confident elapsed interval',
                      future_context['gap_seconds'] is None and
                      future_context['clock']['status'] == 'non_monotonic')
                report['status'] = 'passed'
            finally:
                h.close()
    except Exception as error:
        report['status'] = 'failed'
        report['failure_stage'] = stage
        report['error_type'] = type(error).__name__
        raise
    finally:
        provider.shutdown()
        provider.server_close()
        report['finished_at'] = base.utc_now()
        report['passed'] = len(report['checks'])
        if args.report:
            pathlib.Path(args.report).write_text(
                json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
    print(json.dumps({'status': report['status'], 'passed': report['passed'], 'commit': report['commit']}))


def validate_initial_time(context, before, after):
    validate_current_time(context, before, after)
    if context['previous_visible_user_message_at'] is not None or context['gap_seconds'] is not None:
        raise AssertionError('first conversation turn must not inherit a prior timestamp or gap')
    if context['clock']['status'] != 'unverified':
        raise AssertionError('ordinary server wall clock must not claim verified synchronization')


def validate_history_time(context, historical_at, before, after):
    validate_current_time(context, before, after)
    previous = parse_timestamp(context['previous_visible_user_message_at'])
    current = parse_timestamp(context['current_user_message_at'])
    if int(previous.timestamp()) != historical_at:
        raise AssertionError('previous visible user timestamp does not match persisted history')
    if context['gap_seconds'] != int(current.timestamp()) - historical_at:
        raise AssertionError('conversation interval must come from received user message timestamps')
    if current.date() <= previous.date() or context['gap_seconds'] < 24 * 3600:
        raise AssertionError('synthetic historical fixture must cross the configured local date boundary')


def validate_resumed_time(context):
    validate_current_time(context, int(time.time()) - 10, int(time.time()))
    if context['previous_visible_user_message_at'] is None:
        raise AssertionError('restart must retain the preceding visible user timestamp')
    previous = parse_timestamp(context['previous_visible_user_message_at'])
    current = parse_timestamp(context['current_user_message_at'])
    if context['gap_seconds'] != int(current.timestamp()) - int(previous.timestamp()):
        raise AssertionError('resumed conversation interval must use received message timestamps')


def parse_timestamp(value):
    if not isinstance(value, str):
        raise AssertionError('time contract must use explicit RFC3339 strings')
    result = datetime.datetime.fromisoformat(value)
    if result.utcoffset() != datetime.timedelta(hours=8):
        raise AssertionError('local time must carry the Asia/Shanghai UTC offset')
    return result


def validate_current_time(context, before, after):
    if context['time_zone'] != 'Asia/Shanghai':
        raise AssertionError('trusted conversation timezone differs from configured contract')
    current = parse_timestamp(context['now_local'])
    received = parse_timestamp(context['current_user_message_at'])
    if not before <= int(current.timestamp()) <= after or not before <= int(received.timestamp()) <= after:
        raise AssertionError('time context must reflect server request processing and reception')
    expected_weekday = ('星期一', '星期二', '星期三', '星期四', '星期五', '星期六', '星期日')[current.weekday()]
    if context['weekday'] != expected_weekday:
        raise AssertionError('weekday must match local server date')
    clock = context['clock']
    if (clock['source'] != 'server_wall_clock'
            or clock['message_timestamp'] != 'server_persisted_user_message'
            or clock['gap_basis'] != 'current_user_message_at - previous_visible_user_message_at'):
        raise AssertionError('time provenance must identify the actual server clock and timestamp basis')


if __name__ == '__main__':
    main()
