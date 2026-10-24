#!/usr/bin/env python3
"""Actual isolated chat pipeline with synthetic PNGs and a loopback model.

Exercises the ordinary conversation API, two distinct provider requests,
provenance, cancellation and restart. Never reads production data or emits
pictures, prompts, descriptions, answers or credentials in its report.
"""
import argparse
import base64
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


def module(name, file):
    spec = importlib.util.spec_from_file_location(name, pathlib.Path(__file__).with_name(file))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


image = module('conversation_image_fixtures', 'image-analysis-smoke.py')
helpers = image.helpers
PREFIX = helpers.PREFIX
CAPS = PREFIX + '/conversation-images/capabilities'
TERMINAL = {'completed', 'failed', 'cancelled', 'interrupted', 'outcome_unknown'}


def blocks(payload):
    return [block for message in payload.get('messages', [])
            if isinstance(message.get('content'), list) for block in message['content']]


class Mock:
    def __init__(self):
        self.lock = threading.Lock()
        self.requests = []
        self.canary = 'synthetic-vision-' + secrets.token_hex(10)
        self.hold_release = threading.Event()
        self.hold_received = threading.Event()
        self.hold_kind = None
        self.failure = False

    def count(self, vision=None):
        with self.lock:
            return sum(vision is None or item[0] == vision for item in self.requests)

    def payloads(self, vision):
        with self.lock:
            return [item[1] for item in self.requests if item[0] == vision]

    def respond(self, payload):
        vision = any(block.get('type') == 'image_url' for block in blocks(payload))
        with self.lock:
            self.requests.append((vision, payload))
        if self.hold_kind == ('vision' if vision else 'native'):
            self.hold_received.set()
            self.hold_release.wait(timeout=45)
        if vision:
            if self.failure:
                return 503, 'synthetic unavailable'
            return 200, 'A synthetic blue square. Visual source marker: ' + self.canary
        serialized = json.dumps(payload, ensure_ascii=False)
        return 200, 'synthetic chat answer ' + (self.canary if self.canary in serialized else 'without visual source')

    def hold(self, kind):
        self.hold_kind = kind
        self.hold_release.clear()
        self.hold_received.clear()

    def release(self):
        self.hold_kind = None
        self.hold_release.set()


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        length = int(self.headers.get('Content-Length', '0'))
        if not 0 < length <= 4 * 1024 * 1024:
            self.send_error(413)
            return
        payload = json.loads(self.rfile.read(length))
        status, content = self.server.mock.respond(payload)
        body = json.dumps({'choices': [{'message': {'role': 'assistant', 'content': content}}]}).encode()
        try:
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass


def route(conversation, suffix='image-messages'):
    return PREFIX + '/conversations/' + conversation + '/' + suffix


def body(picture, content='请根据这张合成图片回答问题。', client=None):
    return {'content': content, 'client_message_id': client or str(uuid.uuid4()),
            'image_base64': base64.b64encode(picture).decode()}


def submit(harness, conversation, request, expected=202):
    return harness.call('POST', route(conversation), request, expected=expected)[0]


def wait_run(harness, identifier, timeout=42):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value, _, _ = harness.call('GET', PREFIX + '/runs/' + identifier)
        if value['status'] in TERMINAL:
            return value
        time.sleep(0.05)
    raise TimeoutError('conversation image run did not finish')


def private(harness, run):
    with harness.db() as db:
        value = db.execute('SELECT * FROM conversation_images WHERE run_id=?', (run,)).fetchone()
    return dict(value) if value else None


def wait_released(harness, run):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        with harness.db() as db:
            leased = db.execute('SELECT count(*) FROM native_worker_leases WHERE run_id=?', (run,)).fetchone()[0]
        if not leased:
            return
        time.sleep(0.05)
    raise TimeoutError('actual chat transport lease was not released')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report', required=True)
    args = parser.parse_args()
    binary, web = pathlib.Path(args.binary).resolve(strict=True), pathlib.Path(args.web_dir).resolve(strict=True)
    report = {'kind': 'conversation PNG actual-process isolated mock acceptance',
              'started_at': helpers.acceptance.utc_now(), 'commit': None,
              'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'platform': platform.system(), 'architecture': platform.machine(),
              'status': 'running', 'production_data_accessed': False,
              'real_external_vision_tested': False, 'real_external_model_tested': False,
              'real_qq_tested': False, 'real_weixin_private_chat_tested': False,
              'checks': []}
    stage, result_code = 'setup', 0

    def check(name, value):
        nonlocal stage
        stage = name
        if not value:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    mock = Mock()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    server.daemon_threads, server.mock = True, mock
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-conversation-images-') as temporary:
            harness = helpers.Harness(str(binary), str(web), temporary, mock)
            try:
                harness.start()
                report['commit'] = report['deployment_sha'] = harness.commit
                harness.call('GET', CAPS, authenticated=False, expected=401)
                harness.bootstrap()
                caps, _, _ = harness.call('GET', CAPS)
                check('conversation image capability requires auth and an existing eligible provider', not caps['available'])
                harness.configure(server.server_port)
                caps, _, _ = harness.call('GET', CAPS)
                check('Web PNG routing truthfully identifies Flash and a vision-description basis',
                      caps['available'] and caps['model'] == 'deepseek-flash'
                      and caps['basis'] == 'vision_description' and caps['max_images'] == 1)
                with harness.db() as db:
                    check('empty attachment schema does not stamp reader eight', db.execute('SELECT max(version) FROM dsh_schema_migrations').fetchone()[0] < 8)
                conversation = harness.conversation('synthetic image conversation')
                picture = image.png()
                request = body(image.add_metadata(picture, b'SyntheticMetadataNeverTransmit'))
                before = mock.count()
                harness.call('POST', route(conversation), request, authenticated=False, expected=401)
                harness.call('POST', route(conversation), request, with_csrf=False, expected=403)
                harness.call('POST', route(conversation), request, headers={'Origin': 'https://example.invalid'}, expected=403)
                with helpers.other_admin(harness):
                    submit(harness, conversation, request, expected=404)
                check('owner Origin CSRF and authentication reject before decoding or provider dispatch', mock.count() == before)
                invalid = [dict(request, content=''), dict(request, content='bad\0text'),
                           dict(request, content='/memory remember must-not-run'),
                           dict(request, content='/角色 当前'), dict(request, content='/persona current'),
                           dict(request, content='x' * 16001), dict(request, image_base64=base64.b64encode(b'GIF89a').decode()),
                           dict(request, remote_url='https://example.invalid/picture'), dict(request, image_base64='not base64')]
                for value in invalid:
                    submit(harness, conversation, value, expected=(400, 422))
                harness.call('POST', route(conversation, 'messages'), request, expected=(400, 422))
                check('invalid image inputs controls and text-endpoint attachment smuggling never call a model', mock.count() == before)
                created = submit(harness, conversation, request)
                run = created['run_id']
                completed = wait_run(harness, run)
                wait_released(harness, run)
                check('one ordinary Run produces its assistant answer after real vision then native HTTP',
                      completed['status'] == 'completed' and mock.canary in completed['response']
                      and mock.count(True) == 1 and mock.count(False) == 1)
                vision = mock.payloads(True)[-1]
                check('vision dispatch fixes its model and strips PNG metadata before transmission',
                      vision['model'] == 'deepseek-flash'
                      and b'SyntheticMetadataNeverTransmit' not in image.public_image(vision)
                      and not vision.get('tools') and vision['thinking']['type'] == 'disabled')
                native = mock.payloads(False)[-1]
                check('text assistant receives bounded source-labelled user-role description and no image blocks',
                      not blocks(native) and any(m['role'] == 'user' and mock.canary in str(m['content'])
                      and 'DSH_VISION_DESCRIPTION_SOURCE_V1' in str(m['content']) for m in native['messages'])
                      and any('描述' in str(m['content']) or 'description' in str(m['content'])
                              for m in native['messages'] if m['role'] == 'system'))
                row = private(harness, run)
                check('vision completion drops canonical PNG while retaining private source lineage and a completed lease',
                      row['image'] is None and mock.canary in row['description']
                      and row['source_message_id'] and row['request_started_at'] is not None
                      and row['request_finished_at'] is not None)
                items = harness.call('GET', route(conversation, 'messages'))[0]['items']
                check('ordinary message history exposes safe image metadata and honest answer provenance',
                      len(items) == 2 and items[0]['image']['width'] == 8
                      and items[0]['image']['original_retained'] is False
                      and items[1]['basis'] == 'vision_description'
                      and 'description' not in items[0]['image'])
                with harness.db() as db:
                    check('actual attached work raises reader eight without creating legacy image jobs or implicit memory',
                          db.execute('SELECT max(version) FROM dsh_schema_migrations').fetchone()[0] == 8
                          and db.execute('SELECT count(*) FROM image_analysis_jobs').fetchone()[0] == 0
                          and db.execute('SELECT count(*) FROM memory_records').fetchone()[0] == 0)
                duplicate = submit(harness, conversation, dict(request, image_base64=base64.b64encode(picture).decode()))
                submit(harness, conversation, dict(request, content='different question'), expected=409)
                submit(harness, conversation, dict(request, image_base64=base64.b64encode(image.png(color=(2, 3, 4))).decode()), expected=409)
                harness.call('POST', route(conversation, 'messages'),
                             {'content': request['content'], 'client_message_id': request['client_message_id']}, expected=409)
                check('canonical pixels and content bind idempotency across text and image entrypoints without rebilling',
                      duplicate['run_id'] == run and mock.count() == 2)
                with helpers.other_admin(harness):
                    harness.call('GET', route(conversation, 'messages'), expected=404)
                    harness.call('GET', PREFIX + '/runs/' + run, expected=404)
                follow = harness.submit(conversation, '请继续根据上一张图片描述回答。')
                check('follow-up conversation reuses private visual source without another vision request',
                      mock.canary in wait_run(harness, follow)['response'] and mock.count(True) == 1)
                followed_messages = harness.call('GET', route(conversation, 'messages'))[0]['items']
                check('text follow-up retains its actual visual-description basis in public history',
                      followed_messages[-1].get('basis') == 'vision_description')
                other = harness.conversation('unrelated private image conversation')
                check('another conversation never receives the first image description',
                      mock.canary not in wait_run(harness, harness.submit(other, '无图片的独立问题'))['response'])
                candidates = harness.call('GET', route(conversation, 'memory-candidates'))[0]
                check('derived image turn is not eligible as explicit user-fact extraction source',
                      row['source_message_id'] not in {item['id'] for item in candidates['sources']})
                helpers.invalidate_source(harness, conversation, row['source_message_id'])
                after_forget = wait_run(harness, harness.submit(conversation, '清理旧视觉来源后重新开始。'))
                check('source forgetting clears derived description and blocks old visual source and assistant echoes',
                      private(harness, run)['description'] is None and mock.canary not in after_forget['response'])
                before = mock.count()
                large_conversation = harness.conversation('synthetic attachment body boundary')
                submit(harness, large_conversation, body(image.exact_size_png(1048577)), expected=(400, 413, 422))
                check('raw file over one MiB rejects before provider work', mock.count() == before)
                large = submit(harness, large_conversation, body(image.exact_size_png(1048576)))['run_id']
                check('exact one MiB PNG passes the conversation route body limit and is normalized before dispatch',
                      wait_run(harness, large)['status'] == 'completed' and private(harness, large)['image'] is None)
                for kind in ('vision', 'native'):
                    mock.hold(kind)
                    target = harness.conversation('synthetic cancellation ' + kind)
                    before_native = mock.count(False)
                    pending = submit(harness, target, body(picture))['run_id']
                    if not mock.hold_received.wait(timeout=8):
                        raise AssertionError('actual held provider request not observed')
                    harness.call('DELETE', PREFIX + '/runs/' + pending)
                    wait_released(harness, pending)
                    mock.release()
                    time.sleep(0.15)
                    cancelled = wait_run(harness, pending)
                    private_cancelled = private(harness, pending)
                    check('cancellation during ' + kind + ' drops transport before releasing the slot and rejects late output',
                          cancelled['status'] == 'cancelled' and not cancelled.get('response')
                          and private_cancelled['image'] is None and private_cancelled['description'] is None
                          and (kind != 'vision' or mock.count(False) == before_native))
                mock.failure = True
                target = harness.conversation('synthetic failed vision')
                before_native = mock.count(False)
                bad = submit(harness, target, body(picture))['run_id']
                failed = wait_run(harness, bad)
                wait_released(harness, bad)
                check('vision failure clears private bytes and fails honestly without hallucinated text fallback',
                      failed['status'] == 'failed' and mock.count(False) == before_native
                      and private(harness, bad)['image'] is None and private(harness, bad)['description'] is None)
                mock.failure = False
                for kind in ('vision', 'native'):
                    mock.hold(kind)
                    target = harness.conversation('synthetic image restart ' + kind)
                    interrupted = submit(harness, target, body(picture))['run_id']
                    if not mock.hold_received.wait(timeout=8):
                        raise AssertionError('restart provider not observed')
                    calls = mock.count()
                    harness.stop()
                    mock.release()
                    harness.start()
                    restarted = wait_run(harness, interrupted)
                    check('restart during ' + kind + ' interrupts dispatched image work clears bytes and never replays a model request',
                          restarted['status'] == 'interrupted' and mock.count() == calls
                          and private(harness, interrupted)['image'] is None
                          and private(harness, interrupted)['description'] is None)
                report['status'] = 'passed'
            finally:
                mock.release()
                harness.close()
    except Exception as error:
        result_code = 1
        report.update(status='failed', failure_after_check=stage, error_type=type(error).__name__)
        trace = error.__traceback__
        while trace:
            if trace.tb_frame.f_code.co_filename == __file__:
                report['failure_line'] = trace.tb_lineno
            trace = trace.tb_next
        if isinstance(error, helpers.ResponseStatusError):
            report.update(actual_http_status=error.actual, expected_http_statuses=sorted(error.expected))
        print('FAIL isolated conversation-image acceptance; see sanitized report', flush=True)
    finally:
        server.shutdown()
        server.server_close()
        report.update(finished_at=helpers.acceptance.utc_now(), passed=len(report['checks']),
                      mock_vision_requests=mock.count(True), mock_text_requests=mock.count(False))
        path = pathlib.Path(args.report).resolve()
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps({'status': report['status'], 'passed': report['passed'], 'real_external_model_tested': False}))
    return result_code


if __name__ == '__main__':
    raise SystemExit(main())
