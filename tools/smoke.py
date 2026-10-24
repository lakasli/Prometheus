#!/usr/bin/env python3
"""Black-box M1 acceptance against a real Rust binary and isolated local mock model.

Never uses production data or credentials. Mock-provider results are not proof of
real DeepSeek/QQ connectivity. Run on the target Linux host after deploying SHA.
"""
import argparse
import copy
import http.cookiejar
import http.server
import json
import os
import pathlib
import secrets
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request


def port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


class Provider(http.server.BaseHTTPRequestHandler):
    requests_seen = []

    def log_message(self, *_):
        pass

    def do_POST(self):
        data = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        Provider.requests_seen.append(data)
        text = next(m['content'] for m in reversed(data['messages']) if m['role'] == 'user')
        if text == 'restart-pending':
            time.sleep(2)
        payload = json.dumps({'choices': [{'message': {'role': 'assistant', 'content': 'mock-response:' + text}}], 'usage': {'prompt_tokens': 5, 'completion_tokens': 5}}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        try:
            self.wfile.write(payload)
        except (BrokenPipeError, ConnectionResetError):
            pass


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report')
    args = parser.parse_args()
    binary = str(pathlib.Path(args.binary).resolve())
    web_dir = str(pathlib.Path(args.web_dir).resolve())
    checks = []

    def check(name, predicate):
        if not predicate:
            raise AssertionError(name)
        checks.append(name)
        print('PASS ' + name, flush=True)

    provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    server_port = port()
    base = 'http://127.0.0.1:' + str(server_port)
    origin = base
    jar = http.cookiejar.CookieJar()
    client = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
    csrf = None

    def request(method, path, data=None, expected=200, headers=None, with_csrf=True):
        hdr = {'Origin': origin, 'Content-Type': 'application/json'}
        if csrf and with_csrf:
            hdr['X-CSRF-Token'] = csrf
        if headers:
            hdr.update(headers)
        body = None if data is None else json.dumps(data).encode()
        req = urllib.request.Request(base + path, body, hdr, method=method)
        try:
            response = client.open(req, timeout=12)
        except urllib.error.HTTPError as error:
            response = error
        raw = response.read()
        if response.code != expected:
            raise AssertionError(f'{method} {path}: expected {expected}, got {response.code}')
        try:
            return json.loads(raw), raw
        except json.JSONDecodeError:
            return None, raw

    process = None
    with tempfile.TemporaryDirectory(prefix='dsh-smoke-') as temp:
        data_dir = pathlib.Path(temp) / 'data'
        log = open(pathlib.Path(temp) / 'server.log', 'ab')

        def start():
            nonlocal process
            env = dict(os.environ, DSH_WEB_DIR=web_dir, RUST_LOG='warn')
            process = subprocess.Popen([binary, 'serve', '--data-dir', str(data_dir), '--listen', '127.0.0.1:' + str(server_port), '--origin', origin, '--allow-private-model'], env=env, stdout=log, stderr=log)
            for _ in range(100):
                if process.poll() is not None:
                    raise RuntimeError('Rust test process exited before ready')
                try:
                    request('GET', '/health/ready')
                    return
                except (OSError, AssertionError):
                    time.sleep(0.1)
            raise TimeoutError('Rust test process did not become ready')

        def stop(force=False):
            nonlocal process
            if process and process.poll() is None:
                process.kill() if force else process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)

        def wait_run(run_id):
            for _ in range(150):
                value, _ = request('GET', '/api/admin/v1/runs/' + run_id)
                if value['status'] in ('completed', 'failed', 'cancelled', 'interrupted', 'outcome_unknown'):
                    return value
                time.sleep(0.1)
            raise TimeoutError('Run did not reach a terminal state')

        try:
            start()
            request('GET', '/api/admin/v1/status', expected=401)
            check('unauthenticated management request denied', True)
            _, page = request('GET', '/admin/')
            check('production frontend entry served', b'<div id="app"' in page)
            request('GET', '/api/admin/v1/nonexistent', expected=404)
            state, _ = request('GET', '/api/admin/v1/auth/status')
            check('fresh database needs bootstrap', state['initialized'] is False)
            subprocess.run([binary, 'bootstrap-token', '--data-dir', str(data_dir)], check=True, stdout=subprocess.DEVNULL)
            token = json.loads((data_dir / 'bootstrap-token.json').read_text())['token']
            password = secrets.token_urlsafe(24)
            auth, _ = request('POST', '/api/admin/v1/auth/bootstrap', {'username': 'smoke-admin', 'password': password, 'bootstrap_token': token})
            csrf = auth['csrf_token']
            request('POST', '/api/admin/v1/conversations', {'title': 'No CSRF'}, expected=403, with_csrf=False)
            check('authenticated mutation requires CSRF token', True)
            config, _ = request('GET', '/api/admin/v1/config')
            api_key = secrets.token_urlsafe(32)
            metadata, _ = request('POST', '/api/admin/v1/secrets', {'kind': 'model_api_key', 'value': api_key})
            _, raw = request('GET', '/api/admin/v1/secrets')
            check('secret metadata does not reveal stored key', api_key.encode() not in raw)
            candidate = copy.deepcopy(config['config'])
            candidate['model'] = {'base_url': 'http://127.0.0.1:' + str(provider.server_port), 'model': 'isolated-smoke-provider', 'credential_ref': metadata['id']}
            candidate['assistant']['name'] = 'Smoke verified'
            draft, _ = request('POST', '/api/admin/v1/config/drafts', {'base_revision': config['revision'], 'config': candidate})
            draft_path = '/api/admin/v1/config/drafts/' + draft['id']
            validation, _ = request('POST', draft_path + '/validate', {})
            check('draft validated by server', validation['valid'] is True)
            operation, _ = request('POST', draft_path + '/apply', {}, expected=202, headers={'If-Match': '"' + str(config['revision']) + '"'})
            for _ in range(50):
                op, _ = request('GET', '/api/admin/v1/operations/' + operation['id'])
                if op['status'] in ('completed', 'applied', 'succeeded', 'failed'):
                    break
                time.sleep(0.1)
            applied, _ = request('GET', '/api/admin/v1/config')
            check('desired configuration actually applied', applied['revision'] == applied['applied_revision'] and applied['config']['assistant']['name'] == 'Smoke verified')
            request('POST', draft_path + '/apply', {}, expected=409, headers={'If-Match': '"' + str(config['revision']) + '"'})
            check('stale revision cannot overwrite configuration', True)
            conversation, _ = request('POST', '/api/admin/v1/conversations', {'title': 'Acceptance'})
            messages_path = '/api/admin/v1/conversations/' + conversation['id'] + '/messages'
            submitted, _ = request('POST', messages_path, {'content': 'hello smoke', 'client_message_id': 'smoke-1'}, expected=202)
            completed = wait_run(submitted['run_id'])
            check('native backend calls configured model', completed['status'] == 'completed')
            duplicate, _ = request('POST', messages_path, {'content': 'hello smoke', 'client_message_id': 'smoke-1'}, expected=202)
            check('duplicate input maps to same durable run', duplicate['run_id'] == submitted['run_id'])
            messages, _ = request('GET', messages_path)
            check('assistant response persisted once', len([m for m in messages['items'] if m['role'] == 'assistant' and m['content'] == 'mock-response:hello smoke']) == 1)
            other, _ = request('POST', '/api/admin/v1/conversations', {'title': 'Isolated'})
            other_messages, _ = request('GET', '/api/admin/v1/conversations/' + other['id'] + '/messages')
            check('different conversation has no shared history', len(other_messages['items']) == 0)
            pending, _ = request('POST', messages_path, {'content': 'restart-pending', 'client_message_id': 'smoke-restart'}, expected=202)
            time.sleep(0.3)
            queued, _ = request('POST', messages_path, {'content': 'restart-queued', 'client_message_id': 'smoke-restart-queued'}, expected=202)
            stop(force=True)
            start()
            auth, _ = request('POST', '/api/admin/v1/auth/login', {'username': 'smoke-admin', 'password': password})
            csrf = auth['csrf_token']
            restored, _ = request('GET', '/api/admin/v1/config')
            check('configuration survives process restart', restored['revision'] == applied['revision'])
            recovered = wait_run(pending['run_id'])
            check('interrupted external request is not blindly replayed', recovered['status'] == 'interrupted')
            queued_result = wait_run(queued['run_id'])
            check('durable queued run recovers after restart', queued_result['status'] == 'completed')
            messages, _ = request('GET', messages_path)
            check('restart does not duplicate assistant delivery', len([m for m in messages['items'] if m['role'] == 'assistant' and m['content'] == 'mock-response:restart-queued']) == 1)
            request('POST', '/api/admin/v1/auth/logout', {})
            csrf = None
            request('GET', '/api/admin/v1/status', expected=401)
            check('logout revokes the session', True)
            report = {'kind': 'M1 black-box smoke with isolated mock model', 'real_external_model_tested': False, 'real_qq_tested': False, 'checks': checks, 'passed': len(checks), 'platform': os.uname().machine if hasattr(os, 'uname') else os.name}
            if args.report:
                pathlib.Path(args.report).write_text(json.dumps(report, ensure_ascii=False, indent=2))
            print(json.dumps({'passed': len(checks), 'external_credentials_used': False}))
        finally:
            stop()
            log.close()
            provider.shutdown()


if __name__ == '__main__':
    main()
