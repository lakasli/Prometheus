#!/usr/bin/env python3
"""Validate the Liu Jiahui personality with an isolated daemon and mock model.

Uses generated credentials and disposable data only. Reports contain metadata
and check names, never account credentials, request bodies or memory contents.
"""
import argparse
import datetime
import hashlib
import http.server
import importlib.util
import json
import pathlib
import platform
import re
import tempfile
import threading


def load_harness():
    filename = pathlib.Path(__file__).with_name('memory-smoke.py')
    spec = importlib.util.spec_from_file_location('personality_presentation_harness', filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--personality', default=str(
        pathlib.Path(__file__).resolve().parents[1] / 'docs/examples/liu-jiahui-personality-v1.json'))
    parser.add_argument('--report')
    args = parser.parse_args()
    binary = str(pathlib.Path(args.binary).resolve(strict=True))
    web = str(pathlib.Path(args.web_dir).resolve(strict=True))
    personality_path = pathlib.Path(args.personality).resolve(strict=True)
    personality = json.loads(personality_path.read_text(encoding='utf-8'))
    checks = []

    def check(name, condition=True):
        if not condition:
            raise AssertionError(name)
        checks.append(name)
        print('PASS ' + name, flush=True)

    expected_fields = {'name', 'description', 'personality', 'scenario', 'system_prompt', 'mes_example'}
    check('personality asset has six nonempty text fields and the requested name',
          set(personality) == expected_fields and personality['name'] == '刘嘉卉'
          and all(isinstance(value, str) and value.strip() for value in personality.values()))
    check('displayed personality fields avoid mechanical identity wording',
          not any(re.search(r'助手|工具|\bAI\b|\bassistant\b', value, re.I)
                  for value in personality.values()))
    base = load_harness()
    api = base.PREFIX
    route = api + '/assistant/personality'
    mock = base.MockState()
    provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), base.Provider)
    provider.daemon_threads = True
    provider.mock_state = mock
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-personality-presentation-') as temporary:
            harness = base.Harness(binary, web, temporary, mock)
            try:
                harness.start()
                harness.request('GET', route, expected=401, authenticated=False)
                check('current personality requires authentication')
                harness.bootstrap()
                harness.configure(provider.server_port)
                initial, _, _ = harness.request('GET', route)
                memory = {'content': 'synthetic-preserved-memory', 'visibility': 'shared', 'publish': True}
                harness.request('POST', api + '/memory', memory, expected=201)
                before_memory, _, _ = harness.request('GET', api + '/memory')
                conversation = harness.conversation('personality presentation fixture')
                before_run, _ = harness.model_turn(conversation, 'before personality update')
                with harness.db() as database:
                    before_snapshot = database.execute(
                        'SELECT character_snapshot_json FROM runs WHERE id=?', (before_run,)).fetchone()[0]
                harness.request('PUT', route, {'personality': personality}, expected=403,
                                with_csrf=False, headers={'If-Match': initial['etag']})
                check('personality update requires CSRF')
                saved, _, _ = harness.request('PUT', route, {'personality': personality},
                                            headers={'If-Match': initial['etag']})
                check('requested personality is saved and applied as a new revision',
                      saved['personality'] == personality and saved['revision'] > initial['revision']
                      and saved['applied'] is True and saved['effective_from'] == 'next_run')
                harness.request('PUT', route, {'personality': personality}, expected=409,
                                headers={'If-Match': initial['etag']})
                check('stale personality update is rejected')
                after_run, payload = harness.model_turn(conversation, 'after personality update')
                systems = '\n'.join(str(item['content']) for item in payload['messages']
                                    if item['role'] == 'system')
                example_responses = [line.replace('{{char}}', personality['name'])
                                     for line in personality['mes_example'].splitlines()
                                     if line.startswith('{{char}}')]
                check('existing conversation uses new personality and example responses in its next HTTP request',
                      all(personality[field] in systems for field in expected_fields - {'mes_example'})
                      and bool(example_responses) and all(line in systems for line in example_responses))
                after_memory, _, _ = harness.request('GET', api + '/memory')
                with harness.db() as database:
                    retained_snapshot = database.execute(
                        'SELECT character_snapshot_json FROM runs WHERE id=?', (before_run,)).fetchone()[0]
                    new_snapshot = json.loads(database.execute(
                        'SELECT character_snapshot_json FROM runs WHERE id=?', (after_run,)).fetchone()[0])
                check('completed run snapshot is preserved and new run freezes the requested name',
                      retained_snapshot == before_snapshot and new_snapshot['name'] == personality['name'])
                check('personality update preserves stable identity and all existing memory records',
                      before_memory == after_memory)
                history, _, _ = harness.request('GET', route + '/history')
                prior = next(item for item in history['items'] if item['revision'] == initial['revision'])
                restored, _, _ = harness.request('PUT', route, {'personality': prior['personality']},
                                               headers={'If-Match': saved['etag']})
                check('previous personality can be restored through the authenticated versioned API',
                      restored['personality'] == initial['personality']
                      and restored['revision'] > saved['revision'])
                harness.stop()
                harness.start()
                harness.login()
                persisted, _, _ = harness.request('GET', route)
                final_memory, _, _ = harness.request('GET', api + '/memory')
                check('restored personality and existing memory survive a real daemon restart',
                      persisted == restored and final_memory == before_memory)
                report = {
                    'kind': 'personality presentation isolated acceptance',
                    'commit': harness.commit,
                    'binary_sha256': hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest(),
                    'personality_sha256': hashlib.sha256(personality_path.read_bytes()).hexdigest(),
                    'tested_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
                    'platform': platform.machine(), 'passed': len(checks), 'checks': checks,
                    'production_data_accessed': False, 'external_model_tested': False,
                    'real_qq_tested': False, 'real_weixin_tested': False,
                }
                if args.report:
                    pathlib.Path(args.report).write_text(
                        json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
                print(json.dumps({'passed': len(checks), 'commit': harness.commit}))
            finally:
                harness.close()
    finally:
        provider.shutdown()
        provider.server_close()


if __name__ == '__main__':
    main()
