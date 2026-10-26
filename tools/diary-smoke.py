#!/usr/bin/env python3
"""Diary API/process acceptance using an isolated database and synthetic sources.

Optional --live-config-from reads only the current model configuration and its
credential reference from an authorized deployment, then invokes that provider
with synthetic diary events in a temporary instance. Production diary data and
channels are never copied. Reports contain check metadata, never source text,
prompts, model outputs or credentials.
"""
import argparse
import copy
import datetime
import hashlib
import ipaddress
import http.server
import importlib.util
import json
import pathlib
import platform
import re
import sqlite3
import socket
import tempfile
import threading
import time
import urllib.parse

spec = importlib.util.spec_from_file_location(
    'memory_smoke', pathlib.Path(__file__).with_name('memory-smoke.py'))
memory = importlib.util.module_from_spec(spec)
spec.loader.exec_module(memory)
PREFIX = memory.PREFIX


class Provider(memory.Provider):
    def do_POST(self):
        size = int(self.headers.get('Content-Length', '0'))
        if not 0 < size <= 1024 * 1024:
            self.send_error(413)
            return
        payload = json.loads(self.rfile.read(size))
        with self.server.mock_state.condition:
            self.server.mock_state.requests.append(payload)
        if payload['messages'][0]['content'].startswith('DSH_DIARY_GENERATOR_V2'):
            content = json.dumps({'facts': '今天收到了一条合成测试记录，记下对方学习了一首诗。',
                                  'feelings': '我觉得这次交流轻松。'}, ensure_ascii=False)
        else:
            content = '已收到这条合成测试消息。'
        raw = json.dumps({'choices': [{'message': {'content': content}}]}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(raw)))
        self.end_headers()
        try:
            self.wfile.write(raw)
        except (BrokenPipeError, ConnectionResetError):
            pass


def configure_live(harness, data_dir):
    root = pathlib.Path(data_dir).resolve(strict=True)
    connection = sqlite3.connect((root / 'dsh.db').as_uri() + '?mode=ro', uri=True)
    try:
        revision = connection.execute("SELECT value FROM settings WHERE key='active_revision'").fetchone()[0]
        source = json.loads(connection.execute(
            'SELECT config_json FROM config_revisions WHERE revision=?', (revision,)).fetchone()[0])
    finally:
        connection.close()
    endpoint = urllib.parse.urlsplit(source['model']['base_url'])
    if endpoint.scheme != 'https' or not endpoint.hostname:
        raise RuntimeError('external acceptance requires the configured HTTPS provider')
    addresses = socket.getaddrinfo(endpoint.hostname, endpoint.port or 443, type=socket.SOCK_STREAM)
    if not addresses or any(not ipaddress.ip_address(row[4][0]).is_global for row in addresses):
        raise RuntimeError('external acceptance cannot classify a local mock as a real provider')
    ref = source['model']['credential_ref']
    if not re.fullmatch(r'[0-9a-fA-F-]{36}', ref):
        raise RuntimeError('unsupported credential reference format')
    secret_file = (root / 'secrets' / ref).resolve(strict=True)
    if not secret_file.is_relative_to(root / 'secrets'):
        raise RuntimeError('credential escaped deployment secret directory')
    key = secret_file.read_text()
    current, _, _ = harness.request('GET', PREFIX + '/config')
    credential, _, _ = harness.request('POST', PREFIX + '/secrets', {
        'kind': 'model_api_key', 'value': key})
    candidate = copy.deepcopy(current['config'])
    candidate['model'] = copy.deepcopy(source['model'])
    candidate['model']['credential_ref'] = credential['id']
    candidate['qq']['enabled'] = False
    draft, _, _ = harness.request('POST', PREFIX + '/config/drafts', {
        'base_revision': current['revision'], 'config': candidate})
    harness.request('POST', PREFIX + '/config/drafts/' + draft['id'] + '/apply', {},
                    expected=202, headers={'If-Match': str(current['revision'])})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report')
    parser.add_argument('--live-config-from')
    args = parser.parse_args()
    binary = str(pathlib.Path(args.binary).resolve(strict=True))
    web_dir = str(pathlib.Path(args.web_dir).resolve(strict=True))
    report = {'kind': 'isolated-diary-process-api', 'status': 'running',
              'started_at': memory.utc_now(), 'platform': platform.system(),
              'architecture': platform.machine(),
              'binary_sha256': hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest(),
              'external_model_requested': bool(args.live_config_from),
              'external_model_passed': False, 'production_data_modified': False,
              'real_channel_tested': False, 'checks': []}
    mock = memory.MockState()
    provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    provider.daemon_threads = True
    provider.mock_state = mock
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    failed = False

    def check(name, condition):
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    try:
        with tempfile.TemporaryDirectory(prefix='dsh-diary-acceptance-') as temporary:
            harness = memory.Harness(binary, web_dir, temporary, mock)
            try:
                harness.start()
                report['commit'] = harness.commit
                for path in ['/diary/status', '/diary/entries', '/diary/events',
                             '/diary/export', '/diary/sources', '/diary/policies']:
                    harness.request('GET', PREFIX + path, expected=401)
                check('all diary reads and exports require authentication', True)
                harness.bootstrap()
                harness.request('POST', PREFIX + '/diary/events', {'content': 'synthetic'},
                                expected=403, with_csrf=False)
                harness.request('POST', PREFIX + '/diary/events', {'content': 'synthetic'},
                                expected=403, headers={'Origin': 'https://invalid.example'})
                check('diary mutations enforce CSRF and exact origin', True)
                conversation = harness.conversation('Synthetic diary source')
                if args.live_config_from:
                    configure_live(harness, args.live_config_from)
                else:
                    harness.configure(provider.server_port)
                policies, _, _ = harness.request('GET', PREFIX + '/diary/policies')
                check('automatic collection and model egress default off', not policies['items'])
                policy = {'source_owner_id': harness.owner, 'enabled': True,
                          'allow_model': True, 'timezone': 'Asia/Shanghai'}
                harness.request('PUT', PREFIX + '/diary/policies', policy,
                                headers={'If-Match': '0'})
                harness.request('PUT', PREFIX + '/diary/policies', policy,
                                headers={'If-Match': '0'}, expected=409)
                source_list, _, _ = harness.request('GET', PREFIX + '/diary/sources')
                check('versioned source authorization is discoverable',
                      any(v['source_owner_id'] == harness.owner and v['enabled'] for v in source_list['items']))
                now = int(time.time())
                note, _, _ = harness.request('POST', PREFIX + '/diary/events', {
                    'content': '合成验收：今天读了一首诗，明天再讨论其中的意象。',
                    'source_conversation_id': conversation, 'happened_at': now,
                    'visibility': 'private'}, expected=201)
                today = datetime.datetime.now(datetime.timezone(datetime.timedelta(hours=8))).date().isoformat()
                body = {'period': 'day', 'day': today, 'timezone': 'Asia/Shanghai',
                        'visibility': 'private', 'source_owner_id': harness.owner}
                entry, _, _ = harness.request('POST', PREFIX + '/diary/consolidate', body)
                same, _, _ = harness.request('POST', PREFIX + '/diary/consolidate', body)
                check('day is persisted with stable identity and idempotent revision',
                      entry['id'] == same['id'] and entry['revision'] == same['revision'] and entry['source_count'] == 1)
                # Only this isolated instance's scheduler clock is acknowledged.
                with harness.db() as db:
                    db.execute('UPDATE trigger_runtime SET clock_trusted=1,space_ok=1 WHERE id=1')
                # Existing scheduler will refresh from NTP/manual acknowledgement;
                # the normal clock endpoint keeps the in-memory proof consistent.
                # The API name below is the established trigger clock contract.
                harness.request('POST', PREFIX + '/task-triggers/clock/acknowledge',
                                {'observed_now': int(time.time()), 'confirmed': True})
                deadline = time.monotonic() + 75
                generated = None
                while time.monotonic() < deadline:
                    candidate, _, _ = harness.request('GET', PREFIX + '/diary/entries/' + entry['id'])
                    if candidate['generator'].startswith('model:'):
                        generated = candidate
                        break
                    time.sleep(.2)
                check('authorized provider creates a real persisted diary with separate feelings',
                      generated is not None and bool(generated['facts']) and isinstance(generated['feelings'], str))
                report['external_model_passed'] = bool(args.live_config_from)
                evidence, _, _ = harness.request('GET', PREFIX + '/diary/entries/' + entry['id'] + '/sources')
                check('generated diary retains exact source identity and revision',
                      len(evidence['items']) == 1 and evidence['items'][0]['id'] == note['id'])
                corrected, _, _ = harness.request('PATCH', PREFIX + '/diary/events/' + note['id'],
                    {'content': '合成更正：今天读的是两首诗。'}, headers={'If-Match': '1'})
                harness.request('PATCH', PREFIX + '/diary/events/' + note['id'],
                    {'content': 'stale'}, headers={'If-Match': '1'}, expected=409)
                harness.request('GET', PREFIX + '/diary/entries/' + entry['id'], expected=404)
                check('correction invalidates generated diary and rejects stale writes', corrected['revision'] == 2)
                harness.request('DELETE', PREFIX + '/diary/events/' + note['id'],
                                headers={'If-Match': '2'})
                harness.stop()
                harness.start()
                harness.login()
                harness.request('GET', PREFIX + '/diary/events/' + note['id'], expected=404)
                exported, raw, _ = harness.request('GET', PREFIX + '/diary/export')
                check('forget tombstone survives restart and export contains no forgotten text',
                      '合成更正'.encode() not in raw and note['id'] not in str(exported.get('events', {})))
                report['status'] = 'passed'
            finally:
                harness.close()
    except Exception as error:
        failed = True
        report['status'] = 'failed'
        report['error_type'] = type(error).__name__
        print('FAIL diary acceptance (' + type(error).__name__ + ')', flush=True)
    finally:
        provider.shutdown()
        report['finished_at'] = memory.utc_now()
        if args.report:
            pathlib.Path(args.report).write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
    return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
