#!/usr/bin/env python3
"""Exercise real-model conversation summaries in a private temporary instance.

Read only the deployed active model configuration and its credential. Never read
production chats, memories, personas, or channel data; never modify production
SQLite. All conversations and facts sent to the provider are synthetic. Run only
after deploying the fixed source SHA. No QQ connection or message is created.
"""
import argparse
import copy
import hashlib
import importlib.util
import ipaddress
import json
import pathlib
import platform
import secrets
import sqlite3
import sys
import tempfile
import threading
import time
import types
import uuid
import urllib.parse
from contextlib import closing

sys.dont_write_bytecode = True


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
    spec = importlib.util.spec_from_file_location(
        'summary_live_harness', pathlib.Path(__file__).with_name('memory-smoke.py'))
    acceptance = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(acceptance)
    with binary.open('rb') as handle:
        binary_hash = hashlib.file_digest(handle, 'sha256').hexdigest()
    report = {'kind': 'conversation-summary isolated real configured-model acceptance',
              'started_at': acceptance.utc_now(), 'commit': None, 'binary_sha256': binary_hash,
              'platform': platform.system(), 'architecture': platform.machine(), 'status': 'running',
              'real_external_model_attempted': False, 'real_external_model_tested': False,
              'real_qq_tested': False, 'production_model_configuration_read': False,
              'production_model_credential_read': False,
              'production_database_modified': False, 'real_user_content_accessed': False,
              'synthetic_conversation_only': True, 'model_request_budget': 20, 'checks': []}
    stage = 'read-only production model setup'
    foreground_calls = 0

    def check(name, condition):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    failed = False
    key = None
    try:
        with closing(sqlite3.connect((source / 'dsh.db').as_uri() + '?mode=ro', uri=True)) as database:
            database.execute('PRAGMA query_only=ON')
            row = database.execute("SELECT json_extract(c.config_json,'$.model') FROM config_revisions c JOIN settings s "
                                   "ON s.key='active_revision' AND c.revision=CAST(s.value AS INTEGER)").fetchone()
        if row is None:
            raise RuntimeError('active provider configuration unavailable')
        model = json.loads(row[0])
        report['production_model_configuration_read'] = True
        endpoint = urllib.parse.urlsplit(model['base_url'])
        if endpoint.scheme != 'https' or not endpoint.hostname or endpoint.hostname in {'localhost', 'localhost.localdomain'}:
            raise RuntimeError('live acceptance requires an external HTTPS provider')
        try:
            address = ipaddress.ip_address(endpoint.hostname)
        except ValueError:
            address = None
        if address is not None and not address.is_global:
            raise RuntimeError('live acceptance cannot label a private mock as an external provider')
        credential_id = str(uuid.UUID(model['credential_ref']))
        store = (source / 'secrets').resolve(strict=True)
        credential_path = (store / credential_id).resolve(strict=True)
        if not credential_path.is_relative_to(store):
            raise RuntimeError('credential escaped its existing private store')
        key = credential_path.read_text()
        report['production_model_credential_read'] = True
        with tempfile.TemporaryDirectory(prefix='dsh-summary-live-') as temporary:
            fixture = types.SimpleNamespace(hold_release=threading.Event())
            harness = acceptance.Harness(str(binary), str(web), temporary, fixture)
            try:
                harness.start()
                report['commit'] = harness.commit
                harness.bootstrap()
                current, _, _ = harness.request('GET', acceptance.PREFIX + '/config')
                credential, _, _ = harness.request('POST', acceptance.PREFIX + '/secrets',
                                                  {'kind': 'model_api_key', 'value': key})
                key = None
                candidate = copy.deepcopy(current['config'])
                candidate['model'] = dict(model, credential_ref=credential['id'])
                candidate['qq']['enabled'] = False
                candidate['assistant']['timeout_seconds'] = 120
                candidate['assistant']['system_prompt'] = (
                    '你是一个隔离的合成验收助手。只根据当前请求所附的历史、摘要和记忆资料回答。'
                    '没有所问事实时输出UNKNOWN，不猜测，不声称访问其他会话。')
                draft, _, _ = harness.request('POST', acceptance.PREFIX + '/config/drafts',
                                             {'base_revision': current['revision'], 'config': candidate})
                path = acceptance.PREFIX + '/config/drafts/' + draft['id']
                validation, _, _ = harness.request('POST', path + '/validate', {})
                check('temporary instance validates only the copied real model configuration', validation['valid'])
                harness.request('POST', path + '/apply', {}, expected=202,
                                headers={'If-Match': '"' + str(current['revision']) + '"'})
                canary = 'DSHSUM' + secrets.token_hex(6).upper()
                chat = harness.conversation('synthetic real-model long conversation')
                question = ('请仅根据当前可用历史资料回答：这个合成验收项目最早约定的代号是什么？'
                            '只输出代号；没有可用记录时只输出UNKNOWN。')

                def turn(text):
                    nonlocal foreground_calls, stage
                    stage = 'bounded synthetic real-model conversation'
                    with harness.db() as database:
                        attempted = database.execute('SELECT count(*) FROM summary_jobs').fetchone()[0]
                        floor = database.execute('SELECT through_run_rowid FROM memory_context_watermarks '
                                                 'WHERE conversation_id=?', (chat,)).fetchone()
                        active = database.execute('SELECT through_run_rowid FROM conversation_summaries '
                                                  'WHERE conversation_id=? AND content IS NOT NULL AND invalidated_at IS NULL', (chat,)).fetchone()
                        start = max(floor[0] if floor else 0, active[0] if active else 0)
                        unsummarized = database.execute('SELECT count(*) FROM runs WHERE conversation_id=? '
                                                        'AND run_kind=\'model\' AND status=\'completed\' AND rowid>?',
                                                        (chat, start)).fetchone()[0]
                    # No automatic request retry. Summary requests are separately
                    # bounded by one attempt/Run, one slot, and the known trigger.
                    if foreground_calls + attempted + 1 + int(unsummarized >= 8) > 20:
                        raise RuntimeError('real model acceptance request budget exhausted')
                    foreground_calls += 1
                    report['real_external_model_attempted'] = True
                    run = harness.submit(chat, text)
                    deadline = time.monotonic() + 150
                    while time.monotonic() < deadline:
                        result, _, _ = harness.request('GET', acceptance.PREFIX + '/runs/' + run)
                        if result['status'] in acceptance.TERMINAL:
                            break
                        time.sleep(0.2)
                    else:
                        raise TimeoutError('bounded real-model run exceeded 150 seconds')
                    if result['status'] != 'completed':
                        raise AssertionError('synthetic real-model Run did not complete')
                    report['real_external_model_tested'] = True
                    with harness.db() as database:
                        row = database.execute('SELECT messages_json FROM run_prompt_snapshots WHERE run_id=?', (run,)).fetchone()
                        attempts = database.execute('SELECT count(*) FROM summary_jobs').fetchone()[0]
                    if row is None:
                        raise AssertionError('real model prompt snapshot unavailable')
                    report['summary_jobs_attempted'] = attempts
                    report['model_requests_upper_bound'] = foreground_calls + attempts
                    if foreground_calls + attempts > 20:
                        raise AssertionError('configured real model request budget exceeded')
                    return run, result['response'], json.loads(row[0])

                first_question = ('合成验收资料：这个项目的早期代号是' + canary +
                                  '。这不是密码或真实个人资料。请只回复“已收到”，不要重复代号。')
                first, _, _ = turn(first_question)
                for index in range(13):
                    turn('合成填充第' + str(index + 1) + '轮：请只回复“好”，不要复述先前代号。')
                recall_run, response, frozen = turn(question)
                state, _, _ = harness.request('GET', acceptance.PREFIX + '/conversations/' + chat + '/summary')
                summary = state['summary']
                check('real provider actually creates a bounded private incremental summary',
                      summary is not None and summary['source_count'] >= 8
                      and len(summary['content'].encode()) <= 6000 and canary in summary['content'])
                envelopes = [item for item in frozen if item['role'] == 'user' and '旧对话摘要' in item['content']]
                check('real model recalls an early fact beyond the original recent-message window',
                      canary in response and len(envelopes) == 1 and canary in envelopes[0]['content']
                      and first_question not in [item['content'] for item in frozen]
                      and all(canary not in item['content'] for item in frozen if item is not envelopes[0]))
                with harness.db() as database:
                    usage = database.execute('SELECT count(*) FROM run_summary_uses WHERE run_id=?', (recall_run,)).fetchone()[0]
                    original = database.execute('SELECT id FROM messages WHERE run_id=? AND role=\'user\'', (first,)).fetchone()[0]
                    revision = database.execute('SELECT revision FROM conversation_summaries WHERE conversation_id=?', (chat,)).fetchone()[0]
                check('real-model summary provenance and frozen prompt are durable', usage == 1)
                harness.stop(); harness.start(); harness.login()
                _, response, frozen = turn(question)
                restarted, _, _ = harness.request('GET', acceptance.PREFIX + '/conversations/' + chat + '/summary')
                check('real model retains the early summary fact after daemon restart',
                      canary in response and restarted['summary']['revision'] >= revision)
                # This explicit synthetic memory is a source-linked deletion
                # anchor created after recall; it never helped either recall.
                memory, _, _ = harness.request('POST', acceptance.PREFIX + '/memory', {
                    'content': '合成验收项目早期代号为' + canary,
                    'visibility': 'private', 'source_conversation_id': chat,
                    'source_message_id': original}, expected=201)
                harness.request('DELETE', acceptance.PREFIX + '/memory/' + memory['id'],
                                headers={'If-Match': '"1"'})
                forgotten, _, _ = harness.request('GET', acceptance.PREFIX + '/conversations/' + chat + '/summary')
                with harness.db() as database:
                    body = database.execute('SELECT content FROM conversation_summaries WHERE conversation_id=?', (chat,)).fetchone()[0]
                    retained = database.execute('SELECT content FROM messages WHERE id=?', (original,)).fetchone()[0]
                check('forgetting its explicit source-linked record invalidates the real summary',
                      forgotten['summary'] is None and body is None and canary in retained)
                _, response, frozen = turn(question)
                check('real model returns UNKNOWN after forgetting and receives no old fact or echo',
                      'UNKNOWN' in response.upper() and canary not in response
                      and canary not in json.dumps(frozen, ensure_ascii=False))
                harness.stop(); harness.start(); harness.login()
                _, response, frozen = turn(question)
                check('real model cannot recover forgotten summary context after restart',
                      'UNKNOWN' in response.upper() and canary not in response
                      and canary not in json.dumps(frozen, ensure_ascii=False))
                check('real provider calls stay within the explicit short-call acceptance budget',
                      report['model_requests_upper_bound'] <= 20 and foreground_calls == 18)
                report['status'] = 'passed'
            finally:
                harness.close()
    except Exception as error:
        failed = True
        report.update(status='failed', failure_after_check=stage, error_type=type(error).__name__)
        print('FAIL real-model summary acceptance; inspect check metadata only', flush=True)
    finally:
        key = None
        report.update(finished_at=acceptance.utc_now(), passed=len(report['checks']),
                      foreground_model_requests=foreground_calls)
        destination = pathlib.Path(args.report).resolve()
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps({'status': report['status'], 'passed': report['passed'],
                          'real_external_model_tested': report['real_external_model_tested'],
                          'real_qq_tested': False, 'production_database_modified': False}))
    return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
