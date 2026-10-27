#!/usr/bin/env python3
"""Prove the actual LAN TLS/proxy/auth path against disposable DSH data.

The production backend is never changed or authenticated by this preflight.
Only the new HTTPS listener and persistent project CA are provisioned; every
account, cookie and bootstrap token is synthetic and removed with the fixture.
"""
import argparse
import contextlib
import hashlib
import http.cookiejar
import importlib.util
import json
import os
import pathlib
import secrets
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

sys.dont_write_bytecode = True


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['source', 'binary', 'web-dir', 'caddy', 'root', 'expected-commit', 'report']:
        parser.add_argument('--' + name, required=True)
    args = parser.parse_args()
    source = pathlib.Path(args.source).resolve(strict=True)
    spec = importlib.util.spec_from_file_location('dsh_lan_https', source / 'deploy/lan_https.py')
    lan = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(lan)
    root = pathlib.Path(args.root).resolve(strict=True)
    if root != lan.ROOT:
        parser.error('only the authorized project root is supported')
    report = dict(kind='isolated real LAN HTTPS authentication preflight', checks=[],
                  production_data_accessed=False, machine_trust_store_modified=False,
                  actual_origin=lan.ORIGIN, program_commit=args.expected_commit,
                  binary_sha256=hashlib.sha256(pathlib.Path(args.binary).read_bytes()).hexdigest())
    processes = []
    stage = 'startup'

    def check(name, condition):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    try:
        with contextlib.ExitStack() as stack:
            temporary = stack.enter_context(tempfile.TemporaryDirectory(prefix='dsh-lan-https-'))
            fixture = pathlib.Path(temporary)
            # Registered before process guards: retain diagnostics after the
            # owned processes stop and before the disposable directory is erased.
            stack.callback(lambda: shutil.copyfile(fixture / 'process.log', pathlib.Path(args.report).with_suffix('.process.log'))
                           if (fixture / 'process.log').exists() else None)
            data = fixture / 'data'
            with socket.socket() as probe:
                probe.bind(('127.0.0.1', 0))
                port = probe.getsockname()[1]
            configuration = fixture / 'Caddyfile'
            configuration.write_text(lan.caddyfile(root=root, upstream=port))
            environment = {k:v for k,v in os.environ.items() if not k.startswith('DSH_')}
            with (fixture / 'process.log').open('wb') as log:
                processes.append(subprocess.Popen([
                    args.binary, 'serve', '--data-dir', str(data), '--listen', f'127.0.0.1:{port}',
                    '--origin', lan.ORIGIN, '--secure-cookie', '--web-dir', args.web_dir],
                    env=environment, stdout=log, stderr=log))
                stack.callback(stop, processes[-1])
                processes.append(subprocess.Popen([
                    args.caddy, 'run', '--config', str(configuration), '--adapter', 'caddyfile'],
                    env=environment, stdout=log, stderr=log))
                stack.callback(stop, processes[-1])
                ca = root / 'shared/https/storage/pki/authorities/local/root.crt'
                deadline = time.monotonic() + 30
                while not ca.is_file() and time.monotonic() < deadline:
                    if any(p.poll() is not None for p in processes):
                        raise RuntimeError('isolated backend or proxy exited before CA creation')
                    time.sleep(.1)
                lan.wait_ready(ca, args.expected_commit)
                client = urllib.request.build_opener(urllib.request.ProxyHandler({}),
                    urllib.request.HTTPSHandler(context=ssl.create_default_context(cafile=str(ca))),
                    urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
                csrf = None

                def request(method, path, body=None, expected=200, extra=None, use_csrf=True):
                    headers = {'Origin': lan.ORIGIN, 'Content-Type': 'application/json'}
                    if csrf and use_csrf:
                        headers['X-CSRF-Token'] = csrf
                    headers.update(extra or {})
                    encoded = None if body is None else json.dumps(body).encode()
                    req = urllib.request.Request(lan.ORIGIN + path, encoded, headers, method=method)
                    try:
                        response = client.open(req, timeout=10)
                    except urllib.error.HTTPError as error:
                        response = error
                    with response:
                        raw, status, response_headers = response.read(), response.code, response.headers
                    if status != expected:
                        raise AssertionError(f'{method} {path}: expected {expected}, got {status}')
                    try:
                        value = json.loads(raw)
                    except ValueError:
                        value = None
                    return value, raw, response_headers

                health, _, _ = request('GET', '/health/ready')
                check('LAN TLS chain and IP SAN verify with the dedicated CA', health['commit'] == args.expected_commit)
                _, page, _ = request('GET', '/admin/')
                check('compiled admin page is served through actual port 8086', b'<html' in page.lower())
                api = '/api/admin/v1'
                request('GET', api + '/config', expected=401)
                request('GET', api + '/rest', expected=401)
                check('private management APIs remain authenticated over the proxy', True)
                subprocess.run([args.binary, 'bootstrap-token', '--data-dir', str(data)],
                               check=True, env=environment, stdout=log, stderr=log, timeout=10)
                token = json.loads((data / 'bootstrap-token.json').read_text())['token']
                account = dict(username='lan-synthetic-admin', password=secrets.token_urlsafe(32), bootstrap_token=token)
                auth, _, headers = request('POST', api + '/auth/bootstrap', account)
                csrf = auth['csrf_token']
                cookie = headers.get('Set-Cookie', '')
                check('HTTPS session cookie is Secure HttpOnly and SameSite Strict',
                      all(v.lower() in cookie.lower() for v in ['Secure', 'HttpOnly', 'SameSite=Strict']))
                request('GET', api + '/config')
                check('authenticated management reads work with original Host and HTTPS Origin', True)
                request('GET', api + '/config', expected=403, extra={'Origin':'https://invalid.example'})
                request('POST', api + '/rest/wake', {}, expected=403, use_csrf=False)
                check('foreign Origin and missing CSRF remain rejected', True)
                request('POST', api + '/auth/logout', {})
                request('GET', api + '/config', expected=401)
                check('logout revokes the HTTPS session', True)
                try:
                    request('GET', api + '/config', extra={'Host':'invalid.example:8086'}, expected=404)
                except AssertionError:
                    raise
                check('a mismatched Host cannot read a management resource', True)
                with socket.create_connection((lan.HOST, lan.PORT), timeout=3) as plain:
                    plain.sendall(f'GET /admin/ HTTP/1.1\r\nHost: {lan.HOST}:{lan.PORT}\r\nConnection: close\r\n\r\n'.encode())
                    result = plain.recv(256)
                check('port 8086 does not serve the login page over plaintext HTTP', b' 200 ' not in result.split(b'\r\n',1)[0])
                report.update(status='passed', ca_sha256=hashlib.sha256(ca.read_bytes()).hexdigest())
    except Exception as error:
        report.update(status='failed', failed_check=stage, error_type=type(error).__name__)
        if isinstance(error, AssertionError):
            report['diagnostic'] = str(error)[:250]
        print('FAIL LAN HTTPS preflight (' + type(error).__name__ + ')', flush=True)
    finally:
        for process in reversed(processes):
            stop(process)
        report['passed'] = len(report['checks'])
        report['cleanup_owned_processes_stopped'] = all(p.poll() is not None for p in processes)
        path = pathlib.Path(args.report)
        with path.open('x', encoding='utf-8') as out:
            json.dump(report, out, ensure_ascii=False, indent=2)
    return 0 if report.get('status') == 'passed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
