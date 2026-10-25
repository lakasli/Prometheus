#!/usr/bin/env python3
"""Keep an isolated DSH daemon and loopback Skill provider alive for browser QA.

Requires a new data directory. It never reads production configuration or keys,
does not initialize an administrator, and writes the short-lived pairing file
only through the daemon's bootstrap-token command. All model output is synthetic.
Stop this exact fixture process with SIGTERM/SIGINT to stop its child daemon.
"""
import argparse
import hashlib
import http.server
import importlib.util
import json
import os
import pathlib
import signal
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

sys.dont_write_bytecode = True


def load_smoke():
    spec = importlib.util.spec_from_file_location(
        'skills_browser_smoke', pathlib.Path(__file__).with_name('skills-smoke.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def private_directory(path):
    path.mkdir(parents=True, exist_ok=True, mode=0o700)
    if os.name != 'nt':
        path.chmod(0o700)


def private_json(path, value):
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, 'w', encoding='utf-8') as handle:
        json.dump(value, handle, ensure_ascii=False, indent=2)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--data-dir', required=True)
    parser.add_argument('--output-dir', required=True)
    args = parser.parse_args()
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    data = pathlib.Path(args.data_dir).resolve()
    output = pathlib.Path(args.output_dir).resolve()
    if not binary.is_file() or not web.is_dir():
        parser.error('binary must be a file and web-dir must be a directory')
    if data.exists() and any(data.iterdir()):
        parser.error('data-dir must be new or empty; existing data is never reused')
    if (output / 'fixture-info.json').exists():
        parser.error('output-dir already has fixture-info.json; use a new directory')
    private_directory(data)
    private_directory(output)
    # Reject an already-used browser port before creating any daemon process.
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 18421))
    smoke = load_smoke()

    class BrowserMock(smoke.Mock):
        def respond(self, path, payload):
            if path.rstrip('/').endswith('/images/generations'):
                return super().respond(path, payload)
            system = '\n'.join(str(item.get('content', ''))
                               for item in payload.get('messages', [])
                               if item.get('role') == 'system')
            question = self.question(payload)
            if ('skill-creator' in system or '制作 Skill' in system
                    or '制作可复用 Skill' in system or 'creator-probe' in question):
                text = '```json\n' + json.dumps(self.generated_skill, ensure_ascii=False) + '\n```'
            elif ('HTML' in system or 'html' in system or '绘画' in system
                  or 'drawing' in question or '绘画' in question or '画' in question):
                text = '''```html
<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><style>
body{margin:0;min-height:320px;display:grid;place-items:center;background:#eaf2fb;font-family:sans-serif}
svg{width:100%;max-width:640px}text{fill:#234577;font-size:18px}</style></head><body>
<svg viewBox="0 0 640 360" role="img" aria-label="合成验收绘画：蓝鲸与海洋">
<rect width="640" height="360" rx="20" fill="#eaf2fb"/><circle cx="530" cy="65" r="34" fill="#f7d991"/>
<path d="M0 245 Q80 220 160 245 T320 245 T480 245 T640 245 V360 H0Z" fill="#80b8d8"/>
<ellipse cx="290" cy="205" rx="130" ry="65" fill="#46699d"/>
<path d="M397 200 L485 140 L470 205 L490 245Z" fill="#46699d"/>
<ellipse cx="255" cy="244" rx="65" ry="18" fill="#c8dfef"/><circle cx="217" cy="190" r="6" fill="#20395d"/>
<path d="M280 140 Q260 85 242 115 M280 140 Q285 85 310 105" fill="none" stroke="#80b8d8" stroke-width="8" stroke-linecap="round"/>
<text x="30" y="45">蓝鲸与海洋 · HTML / SVG 合成验收</text></svg></body></html>
```'''
            else:
                return super().respond(path, payload)
            with self.lock:
                self.requests.append((False, payload))
            return 200, {'choices': [{'message': {'role': 'assistant', 'content': text}}]}

    mock = BrowserMock()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), smoke.Provider)
    server.daemon_threads = True
    server.mock = mock
    threading.Thread(target=server.serve_forever, daemon=True).start()
    stopping = threading.Event()

    def stop(_signal, _frame):
        stopping.set()

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    child = None
    daemon_log = None
    environment = {key: value for key, value in os.environ.items() if not key.startswith('DSH_')}
    environment['RUST_LOG'] = 'dshd=info'
    try:
        # Captured output includes only the private pairing file's location.
        bootstrap = subprocess.run(
            [str(binary), 'bootstrap-token', '--data-dir', str(data)],
            env=environment, capture_output=True, timeout=15)
        if bootstrap.returncode != 0:
            raise RuntimeError('isolated bootstrap-token command failed')
        descriptor = os.open(output / 'daemon.log', os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        daemon_log = os.fdopen(descriptor, 'wb')
        child = subprocess.Popen([
            str(binary), 'serve', '--data-dir', str(data),
            '--listen', '127.0.0.1:18421', '--origin', 'http://127.0.0.1:18421',
            '--web-dir', str(web), '--allow-private-model',
        ], env=environment, stdout=daemon_log, stderr=subprocess.STDOUT)
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        health = None
        deadline = time.monotonic() + 25
        while time.monotonic() < deadline and not stopping.is_set():
            if child.poll() is not None:
                raise RuntimeError('isolated daemon exited before readiness; inspect private daemon.log')
            try:
                with opener.open('http://127.0.0.1:18421/health/ready', timeout=1) as response:
                    health = json.load(response)
                break
            except (OSError, urllib.error.URLError, json.JSONDecodeError):
                stopping.wait(0.2)
        if health is None:
            raise RuntimeError('isolated daemon did not become ready')
        with opener.open('http://127.0.0.1:18421/api/admin/v1/auth/status', timeout=2) as response:
            if json.load(response).get('initialized'):
                raise RuntimeError('isolated daemon must begin uninitialized')
        info = {
            'fixture_pid': os.getpid(), 'daemon_pid': child.pid,
            'origin': 'http://127.0.0.1:18421', 'listen': '127.0.0.1:18421',
            'model_url': f'http://127.0.0.1:{server.server_port}',
            'commit': health.get('commit'),
            'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
            'production_data_accessed': False, 'real_external_model_tested': False,
            'pairing_file': str(data / 'bootstrap-token.json'),
            'daemon_log': str(output / 'daemon.log'),
        }
        private_json(output / 'fixture-info.json', info)
        print(json.dumps({'status': 'ready', 'fixture_pid': os.getpid(),
                          'daemon_pid': child.pid, 'info_file': str(output / 'fixture-info.json')}), flush=True)
        while not stopping.wait(0.5):
            if child.poll() is not None:
                raise RuntimeError('isolated daemon exited; inspect private daemon.log')
    finally:
        mock.hold_release.set()
        if child is not None and child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=15)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=5)
        server.shutdown()
        server.server_close()
        if daemon_log is not None:
            daemon_log.close()


if __name__ == '__main__':
    main()
