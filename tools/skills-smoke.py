#!/usr/bin/env python3
"""Exercise Skill management and real HTTP dispatch with isolated fixtures.

Only a fresh temporary database, synthetic administrator/key, and a loopback
provider are used. Reports contain check names and release hashes, never skill
instructions, model output, images or credentials. This is not a real external
image-generation or channel acceptance test.
"""
import argparse
import base64
import copy
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
import urllib.request
import uuid

sys.dont_write_bytecode = True


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, pathlib.Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


helpers = module('skills_acceptance_helpers', 'memory-candidate-smoke.py')
images = module('skills_image_fixture', 'image-analysis-smoke.py')
PREFIX = helpers.PREFIX
SKILLS = PREFIX + '/skills'
FIELDS = ('name', 'description', 'instructions', 'kind', 'enabled')


class Mock:
    def __init__(self):
        self.lock = threading.Lock()
        self.requests = []
        self.hold_release = threading.Event()
        self.hold_received = threading.Event()
        self.image_failure = False
        self.generated_skill = {
            'name': 'Synthetic reusable skill',
            'description': 'Use only in isolated acceptance.',
            'instructions': 'Provide a concise synthetic response and verify it.',
            'kind': 'instruction',
        }

    def count(self, image=None):
        with self.lock:
            return sum(image is None or item[0] == image for item in self.requests)

    def payload(self, question):
        with self.lock:
            matches = [payload for image, payload in self.requests if not image
                       and question in self.question(payload)]
        if len(matches) != 1:
            raise AssertionError('synthetic chat request was not observed exactly once')
        return matches[0]

    @staticmethod
    def question(payload):
        return next((str(item.get('content', '')) for item in reversed(payload.get('messages', []))
                     if item.get('role') == 'user'), '')

    def respond(self, path, payload):
        image = path.rstrip('/').endswith('/images/generations')
        with self.lock:
            self.requests.append((image, payload))
        if image:
            if self.image_failure:
                return 503, {'error': {'message': 'synthetic provider unavailable'}}
            return 200, {'data': [{'b64_json': base64.b64encode(images.png()).decode()}]}
        question = self.question(payload)
        if 'hold-freeze-probe' in question:
            self.hold_received.set()
            self.hold_release.wait(timeout=25)
        if 'creator-probe' in question:
            content = '```json\n' + json.dumps(self.generated_skill) + '\n```'
        elif 'html-drawing-probe' in question:
            content = '```html\n<!doctype html><html><body><svg viewBox="0 0 120 80"><rect width="120" height="80" fill="blue"/></svg></body></html>\n```'
        else:
            content = 'Synthetic Skill response.'
        return 200, {'choices': [{'message': {'role': 'assistant', 'content': content}}]}


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        size = int(self.headers.get('Content-Length', '0'))
        if not 0 < size <= 1024 * 1024:
            self.send_error(413)
            return
        payload = json.loads(self.rfile.read(size))
        status, value = self.server.mock.respond(self.path, payload)
        raw = json.dumps(value).encode()
        try:
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass


def fields(item, **changes):
    return {**{key: item[key] for key in FIELDS}, **changes}


def system_text(payload):
    return '\n'.join(str(message.get('content', '')) for message in payload.get('messages', [])
                     if message.get('role') == 'system')


def change(harness, item, **changes):
    return harness.call('PATCH', SKILLS + '/' + item['id'],
                        fields(item, expected_revision=item['revision'], **changes))[0]


def submit(harness, conversation, content, skill=None, client=None, expected=202):
    body = {'content': content, 'client_message_id': client or str(uuid.uuid4())}
    if skill is not None:
        body['skill_id'] = skill
    return harness.call('POST', PREFIX + '/conversations/' + conversation + '/messages',
                        body, expected=expected)[0]


def configure(harness, native):
    current = harness.call('GET', PREFIX + '/config')[0]
    config = copy.deepcopy(current['config'])
    config['model']['image_generation'] = native
    config['assistant']['max_concurrent_runs'] = 1
    draft = harness.call('POST', PREFIX + '/config/drafts', {
        'base_revision': current['revision'], 'config': config})[0]
    path = PREFIX + '/config/drafts/' + draft['id']
    valid = harness.call('POST', path + '/validate', {})[0]
    if not valid['valid']:
        raise AssertionError('synthetic capability configuration was not valid')
    operation = harness.call('POST', path + '/apply', {}, expected=202,
                             headers={'If-Match': '"' + str(current['revision']) + '"'})[0]
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        result = harness.call('GET', PREFIX + '/operations/' + operation['id'])[0]
        if result['status'] in ('completed', 'applied', 'succeeded', 'failed'):
            break
        time.sleep(0.05)
    result = harness.call('GET', PREFIX + '/config')[0]
    if result['applied_revision'] != result['revision'] or result['config']['model'].get('image_generation', False) != native:
        raise AssertionError('synthetic capability configuration did not apply')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report', required=True)
    args = parser.parse_args()
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    report = {
        'kind': 'Skill actual-process isolated loopback-provider acceptance',
        'started_at': helpers.acceptance.utc_now(), 'commit': None,
        'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
        'platform': platform.system(), 'architecture': platform.machine(),
        'production_data_accessed': False, 'real_external_model_tested': False,
        'real_external_image_generation_tested': False,
        'real_qq_tested': False, 'real_weixin_private_chat_tested': False,
        'status': 'running', 'checks': [],
    }
    stage, code = 'setup', 0

    def check(name, value=True):
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
        with tempfile.TemporaryDirectory(prefix='dsh-skills-') as temporary:
            harness = helpers.Harness(str(binary), str(web), temporary, mock)
            try:
                harness.start()
                report['commit'] = harness.commit
                harness.call('GET', SKILLS, authenticated=False, expected=401)
                harness.bootstrap()
                initial = harness.call('GET', SKILLS)[0]
                by_id = {item['id']: item for item in initial['items']}
                check('three persisted default Skills exist and native capability is initially false',
                      set(by_id) == {'skill-creator', 'drawing', 'task-trigger-setup'}
                      and all(item['enabled'] and item['builtin'] and item['revision'] == 1 for item in by_id.values())
                      and not initial['drawing']['native_available'])
                body = {'name': 'Synthetic instruction', 'description': 'Synthetic test only.',
                        'instructions': 'SKILL_FREEZE_OLD_' + secrets.token_hex(8),
                        'kind': 'instruction', 'enabled': True}
                harness.call('POST', SKILLS, body, authenticated=False, expected=401)
                harness.call('POST', SKILLS, body, with_csrf=False, expected=403)
                harness.call('POST', SKILLS, body, headers={'Origin': 'https://example.invalid'}, expected=403)
                for invalid in (dict(body, name=' '), dict(body, name='x' * 129),
                                dict(body, description='x' * 1025), dict(body, kind='shell'),
                                dict(body, instructions=' '), dict(body, instructions='x' * 16001),
                                dict(body, arbitrary_shell='never-execute')):
                    harness.call('POST', SKILLS, invalid, expected=(400, 422))
                check('authentication Origin CSRF and bounded typed Skill fields reject invalid writes')
                item = harness.call('POST', SKILLS, body, expected=201)[0]
                check('new Skill is persisted with a server identity and revision',
                      item['id'] not in by_id and item['revision'] == 1 and not item['builtin'])
                harness.call('PATCH', SKILLS + '/' + item['id'],
                             fields(item, expected_revision=0), expected=409)
                harness.call('DELETE', SKILLS + '/' + item['id'], {'expected_revision': 0}, expected=409)
                harness.call('PATCH', SKILLS + '/' + item['id'],
                             fields(item, expected_revision=1), with_csrf=False, expected=403)
                harness.call('DELETE', SKILLS + '/' + item['id'], {'expected_revision': 1},
                             with_csrf=False, expected=403)
                check('Skill updates and deletes require CSRF and reject stale revisions')
                harness.configure(server.server_port)
                configure(harness, False)
                target = harness.conversation('Synthetic Skill selection')
                before = mock.count()
                submit(harness, target, 'missing-skill-probe', 'missing-skill', expected=(400, 404, 422))
                disabled = change(harness, item, enabled=False)
                submit(harness, target, 'disabled-skill-probe', item['id'], expected=(400, 404, 422))
                check('missing and disabled Skills reject before model dispatch', mock.count() == before)
                item = change(harness, disabled, enabled=True)
                blocker = harness.submit(harness.conversation('Synthetic queue blocker'), 'hold-freeze-probe')
                if not mock.hold_received.wait(timeout=8):
                    raise AssertionError('queue blocker did not reach the synthetic HTTP provider')
                client = str(uuid.uuid4())
                queued = submit(harness, target, 'frozen-skill-probe', item['id'], client)['run_id']
                old_instructions = item['instructions']
                item = change(harness, item, instructions='SKILL_FREEZE_NEW_' + secrets.token_hex(8))
                mock.hold_release.set()
                check('held ordinary provider request completes after queue freeze', harness.wait_run(blocker)['status'] == 'completed')
                completed = harness.wait_run(queued)
                payload = json.dumps(mock.payload('frozen-skill-probe'), ensure_ascii=False)
                check('queued Run freezes the chosen Skill revision before later management edits',
                      completed['status'] == 'completed' and old_instructions in payload
                      and item['instructions'] not in payload)
                duplicate = submit(harness, target, 'frozen-skill-probe', item['id'], client)
                submit(harness, target, 'frozen-skill-probe', 'drawing', client, expected=409)
                check('message idempotency includes selected Skill identity without rebilling', duplicate['run_id'] == queued)
                plain = harness.submit(target, 'plain-after-skill-probe')
                check('an unselected follow-up does not inherit previous Skill instructions',
                      harness.wait_run(plain)['status'] == 'completed'
                      and old_instructions not in json.dumps(mock.payload('plain-after-skill-probe'), ensure_ascii=False))
                command = submit(harness, target, '/skill ' + item['id'] + ' command-skill-probe')['run_id']
                check('explicit slash command routes the selected Skill through the ordinary worker',
                      harness.wait_run(command)['status'] == 'completed'
                      and item['instructions'] in json.dumps(mock.payload('command-skill-probe'), ensure_ascii=False))
                creator = submit(harness, target, 'creator-probe', 'skill-creator')['run_id']
                result = harness.wait_run(creator)
                unchanged = harness.call('GET', SKILLS)[0]['items']
                check('default creator produces a structured reviewable draft without automatic installation',
                      result['status'] == 'completed' and '```json' in result['response']
                      and json.dumps(mock.generated_skill) in result['response']
                      and {row['id'] for row in unchanged} == set(by_id) | {item['id']}
                      and by_id['skill-creator']['instructions'] in system_text(mock.payload('creator-probe')))
                reviewed = harness.call('POST', SKILLS, dict(mock.generated_skill, enabled=False), expected=201)[0]
                check('explicitly reviewed generated Skill can be saved disabled', not reviewed['enabled'])
                before_images = mock.count(True)
                html = submit(harness, target, 'html-drawing-probe', 'drawing')['run_id']
                result = harness.wait_run(html)
                check('drawing without native image capability uses an actual HTML model response',
                      result['status'] == 'completed' and '```html' in result['response']
                      and mock.count(True) == before_images
                      and by_id['drawing']['instructions'] in system_text(mock.payload('html-drawing-probe')))
                configure(harness, True)
                check('applied model image capability is reflected in Skill management',
                      harness.call('GET', SKILLS)[0]['drawing']['native_available'])
                before_chat = mock.count(False)
                native = submit(harness, target, 'native-drawing-probe', 'drawing')['run_id']
                result = harness.wait_run(native)
                image_url = result.get('generated_image_url')
                check('native drawing calls the real image HTTP route and exposes an owned image reference',
                      result['status'] == 'completed' and isinstance(image_url, str)
                      and image_url.endswith('/' + native) and 'base64,' not in result['response']
                      and mock.count(True) == before_images + 1 and mock.count(False) == before_chat)
                image_request = urllib.request.Request(harness.base + image_url, headers={'Origin': harness.base})
                with harness.client.open(image_request, timeout=15) as image_response:
                    image_bytes = image_response.read(4 * 1024 * 1024 + 1)
                    image_headers = image_response.headers
                harness.call('GET', image_url, authenticated=False, expected=401)
                with helpers.other_admin(harness):
                    harness.call('GET', image_url, expected=404)
                check('generated PNG bytes require authentication and the owning administrator',
                      image_bytes.startswith(b'\x89PNG\r\n\x1a\n')
                      and len(image_bytes) <= 4 * 1024 * 1024
                      and image_headers.get('Content-Type', '').startswith('image/png'))
                messages = harness.call('GET', PREFIX + '/conversations/' + target + '/messages')[0]['items']
                check('ordinary conversation history keeps an image reference without embedding image bytes',
                      any(message['role'] == 'assistant' and message.get('generated_image_url') == image_url for message in messages)
                      and all('data:image/png;base64,' not in message['content'] for message in messages))
                mock.image_failure = True
                failure = submit(harness, target, 'native-failure-probe', 'drawing')['run_id']
                result = harness.wait_run(failure)
                check('native image failure remains failed and does not invent an HTML or chat fallback',
                      result['status'] == 'failed' and not result.get('response')
                      and mock.count(False) == before_chat and mock.count(True) > before_images + 1)
                follow = harness.submit(target, 'after-native-image-probe')
                check('ordinary text follow-up excludes prior native image bytes and image-only turns from its model context',
                      harness.wait_run(follow)['status'] == 'completed'
                      and 'native-drawing-probe' not in json.dumps(mock.payload('after-native-image-probe'), ensure_ascii=False)
                      and image_url not in json.dumps(mock.payload('after-native-image-probe'), ensure_ascii=False)
                      and 'data:image/png;base64,' not in json.dumps(mock.payload('after-native-image-probe'), ensure_ascii=False))
                drawing = change(harness, by_id['drawing'], enabled=False)
                harness.call('DELETE', SKILLS + '/skill-creator', {'expected_revision': by_id['skill-creator']['revision']})
                harness.call('DELETE', SKILLS + '/' + reviewed['id'], {'expected_revision': reviewed['revision']})
                harness.stop()
                harness.start()
                harness.login()
                restored = {value['id']: value for value in harness.call('GET', SKILLS)[0]['items']}
                check('restart preserves edits and disabled defaults without resurrecting deleted Skills',
                      set(restored) == {'drawing', item['id'], 'task-trigger-setup'}
                      and not restored['drawing']['enabled']
                      and restored['drawing']['revision'] == drawing['revision']
                      and restored[item['id']]['instructions'] == item['instructions'])
                report['status'] = 'passed'
            finally:
                mock.hold_release.set()
                harness.close()
    except Exception as error:
        code = 1
        report.update(status='failed', failure_after_check=stage, error_type=type(error).__name__)
        trace = error.__traceback__
        while trace:
            if trace.tb_frame.f_code.co_filename == __file__:
                report['failure_line'] = trace.tb_lineno
            trace = trace.tb_next
        if isinstance(error, helpers.ResponseStatusError):
            report.update(actual_http_status=error.actual, expected_http_statuses=error.expected)
        print('FAIL isolated Skill acceptance; see sanitized report', flush=True)
    finally:
        server.shutdown()
        server.server_close()
        report.update(finished_at=helpers.acceptance.utc_now(), passed=len(report['checks']),
                      mock_chat_requests=mock.count(False), mock_image_requests=mock.count(True))
        path = pathlib.Path(args.report).resolve()
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps({'status': report['status'], 'passed': report['passed'],
                          'real_external_image_generation_tested': False, 'report': str(path)}))
    return code


if __name__ == '__main__':
    raise SystemExit(main())
