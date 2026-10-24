#!/usr/bin/env python3
"""Verify synthetic assistant memories with the configured real model, in isolation.

Reads only the active model configuration/credential from the supplied deployment.
No production conversation, memory, account, or channel is read or modified. The
credential copy exists only in the private temporary acceptance instance.
"""
import argparse
import copy
import datetime
import hashlib
import importlib.util
import json
import pathlib
import secrets
import sqlite3
import tempfile
import threading
import time
import types
import uuid
from contextlib import closing


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--source-data-dir', required=True)
    parser.add_argument('--report', required=True)
    args = parser.parse_args()
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    source = pathlib.Path(args.source_data_dir).resolve(strict=True)
    spec = importlib.util.spec_from_file_location('memory_acceptance', pathlib.Path(__file__).with_name('memory-smoke.py'))
    acceptance = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(acceptance)
    with closing(sqlite3.connect((source / 'dsh.db').as_uri() + '?mode=ro', uri=True)) as db:
        text = db.execute("SELECT c.config_json FROM config_revisions c JOIN settings s ON s.key='active_revision' AND c.revision=CAST(s.value AS INTEGER)").fetchone()[0]
    model = json.loads(text)['model']
    credential_id = str(uuid.UUID(model['credential_ref']))
    credential_path = (source / 'secrets' / credential_id).resolve(strict=True)
    if not credential_path.is_relative_to(source / 'secrets'):
        raise RuntimeError('model credential escaped its private store')
    key = credential_path.read_text()
    with binary.open('rb') as handle:
        binary_hash = hashlib.file_digest(handle, 'sha256').hexdigest()
    report = {
        'kind': 'assistant-memory isolated real-model acceptance',
        'started_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'status': 'running', 'commit': None, 'binary_sha256': binary_hash,
        'real_external_model_tested': True, 'real_qq_tested': False,
        'production_model_configuration_read': True,
        'production_database_modified': False, 'real_user_content_accessed': False,
        'checks': [],
    }

    def check(name, condition):
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    failed = False
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-memory-live-') as temporary:
            fixture = types.SimpleNamespace(hold_release=threading.Event())
            harness = acceptance.Harness(str(binary), str(web), temporary, fixture)
            try:
                harness.start()
                report['commit'] = harness.commit
                harness.bootstrap()
                current, _, _ = harness.request('GET', acceptance.PREFIX + '/config')
                credential, _, _ = harness.request('POST', acceptance.PREFIX + '/secrets', {'kind': 'model_api_key', 'value': key})
                key = None
                candidate = copy.deepcopy(current['config'])
                candidate['model'] = dict(model, credential_ref=credential['id'])
                candidate['qq']['enabled'] = False
                candidate['assistant']['timeout_seconds'] = 120
                draft, _, _ = harness.request('POST', acceptance.PREFIX + '/config/drafts', {'base_revision': current['revision'], 'config': candidate})
                route = acceptance.PREFIX + '/config/drafts/' + draft['id']
                validation, _, _ = harness.request('POST', route + '/validate', {})
                check('isolated instance accepts the existing real provider configuration', validation['valid'])
                harness.request('POST', route + '/apply', {}, expected=202, headers={'If-Match': '"' + str(current['revision']) + '"'})
                old = 'DSH' + secrets.token_hex(6).upper()
                new = 'DSH' + secrets.token_hex(6).upper()
                memory, _, _ = harness.request('POST', acceptance.PREFIX + '/memory', {
                    'content': '助手当前的临时验收代号是' + old + '。这是合成测试事实。',
                    'visibility': 'shared', 'publish': True,
                }, expected=201)
                first = harness.conversation('synthetic real-model memory acceptance')
                question = '助手当前的临时验收代号是什么？只输出代号；没有可用记录时只输出UNKNOWN。'

                def turn(conversation):
                    run = harness.submit(conversation, question)
                    deadline = time.monotonic() + 150
                    while time.monotonic() < deadline:
                        result, _, _ = harness.request('GET', acceptance.PREFIX + '/runs/' + run)
                        if result['status'] in acceptance.TERMINAL:
                            break
                        time.sleep(0.2)
                    else:
                        raise TimeoutError('real model acceptance exceeded the configured timeout')
                    if result['status'] != 'completed':
                        raise AssertionError('real model run did not complete')
                    with harness.db() as database:
                        snapshot = database.execute('SELECT messages_json FROM run_prompt_snapshots WHERE run_id=?', (run,)).fetchone()[0]
                    return result['response'], snapshot

                response, snapshot = turn(first)
                check('real model recalls a synthetic shared assistant memory', old in response and old in snapshot)
                response, snapshot = turn(harness.conversation('synthetic new channel-independent session'))
                check('real model retains assistant memory across a new conversation', old in response and old in snapshot)
                corrected, _, _ = harness.request('PATCH', acceptance.PREFIX + '/memory/' + memory['id'], {
                    'content': '助手当前的临时验收代号是' + new + '。这是更正后的合成测试事实。',
                }, headers={'If-Match': '"1"'})
                response, snapshot = turn(first)
                check('real model uses a corrected fact and excludes its former echo', new in response and old not in response and old not in snapshot)
                harness.request('DELETE', acceptance.PREFIX + '/memory/' + memory['id'], headers={'If-Match': '"' + str(corrected['revision']) + '"'})
                response, snapshot = turn(first)
                check('real model reports no fact after forgetting in the original conversation', 'UNKNOWN' in response.upper() and old not in snapshot and new not in snapshot)
                harness.stop()
                harness.start()
                harness.login()
                response, snapshot = turn(harness.conversation('synthetic post-restart forgotten memory'))
                check('real model cannot recover the forgotten fact after daemon restart', 'UNKNOWN' in response.upper() and old not in snapshot and new not in snapshot)
                report['status'] = 'passed'
            finally:
                harness.close()
    except Exception as error:
        failed = True
        report['status'] = 'failed'
        # Never include response bodies, source configuration or exception text.
        report['error_type'] = type(error).__name__
        print('FAIL real-model memory acceptance; inspect check names and private daemon diagnostics', flush=True)
    report['finished_at'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    report['passed'] = len(report['checks'])
    destination = pathlib.Path(args.report)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
    print(json.dumps({'status': report['status'], 'passed': report['passed'], 'real_external_model_tested': True, 'real_qq_tested': False}))
    return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
