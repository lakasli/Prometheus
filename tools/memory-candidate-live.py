#!/usr/bin/env python3
"""Optional real-model candidate extraction in an isolated temporary instance.

Read only the deployed model configuration and its existing secret reference.
Never read production conversations, memories, personas or channel accounts.
Only synthetic facts are sent to the model; no QQ/WeChat connection is enabled.
An empty valid extraction is reported as inconclusive, not as review coverage.
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
import subprocess
import sys
import tempfile
import threading
import time
import types
import urllib.parse
import uuid
from contextlib import closing

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    'candidate_live_helpers', pathlib.Path(__file__).with_name('memory-candidate-smoke.py'))
candidate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(candidate)
PREFIX = candidate.PREFIX


class LiveHarness(candidate.Harness):
    def start(self):
        self.process = subprocess.Popen([
            self.binary, 'serve', '--data-dir', str(self.data_dir),
            '--listen', '127.0.0.1:' + str(self.server_port), '--origin', self.base],
            env=self.environment, stdout=self.log, stderr=self.log)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError('temporary live-model daemon exited before readiness')
            try:
                health, _, _ = self.call('GET', '/health/ready')
                self.commit = health['commit']
                return
            except (OSError, AssertionError):
                time.sleep(0.05)
        raise TimeoutError('temporary live-model daemon did not become ready')


def wait_run(harness, run_id, via_api=True):
    deadline = time.monotonic() + 150
    while time.monotonic() < deadline:
        if via_api:
            result, _, _ = harness.call('GET', PREFIX + '/runs/' + run_id)
        else:
            with harness.db() as database:
                row = database.execute('SELECT status,response FROM runs WHERE id=?', (run_id,)).fetchone()
            result = dict(row) if row else None
        if result and result['status'] in candidate.acceptance.TERMINAL:
            if result['status'] != 'completed':
                raise AssertionError('synthetic live-model Run did not complete')
            return result
        time.sleep(0.2)
    raise TimeoutError('synthetic live-model Run exceeded its bounded wait')


def snapshot(harness, run_id):
    with harness.db() as database:
        row = database.execute('SELECT messages_json FROM run_prompt_snapshots WHERE run_id=?', (run_id,)).fetchone()
    if row is None:
        raise AssertionError('real-model Run has no persisted prompt snapshot')
    return json.loads(row[0])


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
    with binary.open('rb') as handle:
        binary_hash = hashlib.file_digest(handle, 'sha256').hexdigest()
    report = {'kind': 'candidate-memory isolated real configured-model acceptance',
              'started_at': candidate.acceptance.utc_now(), 'commit': None, 'deployment_sha': None,
              'binary_sha256': binary_hash, 'platform': platform.system(),
              'architecture': platform.machine(), 'status': 'running',
              'production_model_configuration_read': False, 'production_model_credential_read': False,
              'production_database_modified': False, 'real_user_content_accessed': False,
              'real_external_model_attempted': False, 'real_external_model_tested': False,
              'real_candidate_extraction_tested': False, 'real_candidate_approval_tested': False,
              'real_cross_source_acl_tested': False, 'real_forget_tested': False,
              'real_qq_tested': False, 'real_weixin_private_chat_tested': False,
              'synthetic_conversation_only': True, 'model_request_budget': 8, 'checks': []}
    stage, foreground_calls, result_code = 'read-only model setup', 0, 0
    key = None

    def check(name, condition):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    def reserve(harness):
        nonlocal foreground_calls
        with harness.db() as database:
            jobs = database.execute('SELECT count(*) FROM memory_candidate_jobs').fetchone()[0]
            summaries = database.execute('SELECT count(*) FROM summary_jobs').fetchone()[0]
        if foreground_calls + jobs + summaries + 1 > report['model_request_budget']:
            raise RuntimeError('isolated real-model request budget exhausted')
        foreground_calls += 1
        report['real_external_model_attempted'] = True

    try:
        with closing(sqlite3.connect((source / 'dsh.db').as_uri() + '?mode=ro', uri=True)) as database:
            database.execute('PRAGMA query_only=ON')
            row = database.execute(
                "SELECT json_extract(c.config_json,'$.model') FROM config_revisions c JOIN settings s "
                "ON s.key='active_revision' AND c.revision=CAST(s.value AS INTEGER)").fetchone()
        if row is None:
            raise RuntimeError('active model configuration unavailable')
        model = json.loads(row[0])
        report['production_model_configuration_read'] = True
        endpoint = urllib.parse.urlsplit(model['base_url'])
        if (endpoint.scheme != 'https' or not endpoint.hostname or endpoint.username or endpoint.password
                or endpoint.hostname in {'localhost', 'localhost.localdomain'}):
            raise RuntimeError('live candidate acceptance requires an external HTTPS model')
        try:
            address = ipaddress.ip_address(endpoint.hostname)
        except ValueError:
            address = None
        if address is not None and not address.is_global:
            raise RuntimeError('private provider cannot be labelled a real external model')
        credential_id = str(uuid.UUID(model['credential_ref']))
        secret_root = (source / 'secrets').resolve(strict=True)
        credential_path = (secret_root / credential_id).resolve(strict=True)
        if not credential_path.is_relative_to(secret_root):
            raise RuntimeError('credential reference escaped its private store')
        key = credential_path.read_text()
        report['production_model_credential_read'] = True
        with tempfile.TemporaryDirectory(prefix='dsh-candidate-live-') as temporary:
            fixture = types.SimpleNamespace(hold_release=threading.Event())
            harness = LiveHarness(str(binary), str(web), temporary, fixture)
            try:
                harness.start()
                report['commit'] = report['deployment_sha'] = harness.commit
                harness.bootstrap()
                current, _, _ = harness.call('GET', PREFIX + '/config')
                credential, _, _ = harness.call('POST', PREFIX + '/secrets',
                                               {'kind': 'model_api_key', 'value': key})
                key = None
                configured = copy.deepcopy(current['config'])
                configured['model'] = dict(model, credential_ref=credential['id'])
                configured['qq']['enabled'] = False
                configured['assistant']['timeout_seconds'] = 120
                configured['assistant']['system_prompt'] = (
                    '你正在参与独立临时实例的合成验收。资料仅属于本次合成会话。'
                    '只根据实际提供的历史和长期记忆资料回答；缺少所问事实时输出UNKNOWN。')
                draft, _, _ = harness.call('POST', PREFIX + '/config/drafts',
                                           {'base_revision': current['revision'], 'config': configured})
                config_path = PREFIX + '/config/drafts/' + draft['id']
                validation, _, _ = harness.call('POST', config_path + '/validate', {})
                check('temporary instance validates only the copied external model configuration', validation['valid'])
                harness.call('POST', config_path + '/apply', {}, expected=202,
                             headers={'If-Match': '"' + str(current['revision']) + '"'})
                applied, _, _ = harness.call('GET', PREFIX + '/config')
                harness.applied = applied['config']
                with harness.db() as database:
                    channel_accounts = database.execute('SELECT count(*) FROM weixin_account').fetchone()[0]
                check('live extraction instance has no WeChat account and QQ remains disabled',
                      channel_accounts == 0 and not harness.applied['qq']['enabled'])

                canary = 'CAND' + secrets.token_hex(6).upper()
                source_chat = harness.conversation('synthetic real-model durable preference')
                reserve(harness)
                source_run = harness.submit(source_chat,
                    '以下完全是临时合成验收人物的信息：我长期喜欢在安静的环境阅读纸质历史书，'
                    '这是我明确希望今后沿用的稳定偏好，我把这个阅读偏好称为“' + canary + '”。'
                    '这条消息本身不要求你写入长期记忆，本轮只确认已理解即可。')
                wait_run(harness, source_run)
                report['real_external_model_tested'] = True
                source_state, source_tag = candidate.state(harness, source_chat)
                ids = [item['id'] for item in source_state['sources']]
                check('real synthetic conversation exposes selectable provenance without automatic extraction',
                      bool(ids) and source_state['job'] is None and source_state['items'] == [])
                # One bounded background request; no retry if the provider yields
                # an empty set, rejects JSON, times out or fails transport.
                with harness.db() as database:
                    used = foreground_calls + database.execute('SELECT count(*) FROM memory_candidate_jobs').fetchone()[0]
                if used + 1 > report['model_request_budget']:
                    raise RuntimeError('real candidate request budget exhausted')
                report['real_external_model_attempted'] = True
                job, _, _ = harness.call('POST', candidate.route(source_chat) + '/extract',
                                         {'source_message_ids': ids}, expected=202,
                                         headers={'If-Match': source_tag})
                completed = candidate.wait_job(harness, job['id'])
                check('real configured provider completes its strict candidate extraction job',
                      completed['status'] == 'completed')
                report['real_candidate_extraction_tested'] = True
                pending = candidate.pending_items(harness, source_chat)
                if not pending:
                    report.update(status='inconclusive', outcome='valid_empty_extraction',
                                  approval_and_acl_coverage='not_exercised')
                    result_code = 2
                else:
                    item = next((value for value in pending if canary in value.get('content', '')), pending[0])
                    derived_content = item['content']
                    check('real model proposes nonempty bounded facts with only selected user-message provenance',
                          0 < len(derived_content.encode()) <= 1000
                          and bool(item['source_message_ids']) and set(item['source_message_ids']) <= set(ids))
                    path = PREFIX + '/memory-candidates/' + item['id']
                    before = candidate.memory_count(harness)
                    approved, _, _ = harness.call('POST', path + '/accept', {},
                        headers={'If-Match': '"' + str(item['revision']) + '"'})
                    memory, _, _ = harness.call('GET', PREFIX + '/memory/' + approved['memory_id'])
                    check('review stores the actual model-derived fact as one source-private memory',
                          candidate.memory_count(harness) == before + 1
                          and memory['content'] == derived_content and memory['visibility'] == 'private'
                          and memory['source_owner_id'] == harness.owner
                          and memory['source_conversation_id'] == source_chat
                          and memory['source_message_id'] in ids and candidate.body_cleared(harness, item['id']))
                    report['real_candidate_approval_tested'] = True
                    recall = harness.conversation('synthetic real-model private recall')
                    reserve(harness)
                    recall_run = harness.submit(recall, '请根据当前可用的已审核长期记忆回答我的稳定阅读偏好。没有资料时只回答UNKNOWN。')
                    wait_run(harness, recall_run)
                    with harness.db() as database:
                        use_count = database.execute('SELECT count(*) FROM memory_run_uses WHERE run_id=? AND memory_id=?',
                                                     (recall_run, memory['id'])).fetchone()[0]
                    check('actual real-model recall prompt receives the approved source-private memory',
                          use_count == 1 and derived_content in json.dumps(snapshot(harness, recall_run), ensure_ascii=False))

                    foreign = str(uuid.uuid4())
                    foreign_owner = 'qq:synthetic:c2c:candidate-live-other-owner'
                    foreign_run, foreign_message = str(uuid.uuid4()), str(uuid.uuid4())
                    now = int(time.time())
                    reserve(harness)
                    with harness.db() as database:
                        database.execute('INSERT INTO conversations(id,owner_id,title,source_key,created_at) VALUES(?,?,?,?,?)',
                                         (foreign, foreign_owner, 'synthetic foreign-owner source', foreign_owner, now))
                        database.execute('INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,'
                                         'status,created_at,updated_at,run_kind) VALUES(?,?,?,?,?,?,\'queued\',?,?,\'model\')',
                                         (foreign_run, foreign, foreign_owner, foreign_run,
                                          '请回答当前资料中这个使用者的阅读偏好；没有可用记录时只输出UNKNOWN。',
                                          json.dumps(harness.applied), now, now))
                        database.execute('INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) '
                                         'VALUES(?,?,?,\'user\',?,?)',
                                         (foreign_message, foreign, foreign_run,
                                          '请回答当前资料中这个使用者的阅读偏好；没有可用记录时只输出UNKNOWN。', now))
                    wait_run(harness, foreign_run, via_api=False)
                    foreign_prompt = json.dumps(snapshot(harness, foreign_run), ensure_ascii=False)
                    with harness.db() as database:
                        foreign_uses = database.execute('SELECT count(*) FROM memory_run_uses WHERE run_id=? AND memory_id=?',
                                                        (foreign_run, memory['id'])).fetchone()[0]
                    harness.call('GET', candidate.route(foreign), expected=404)
                    check('foreign-owner native Run excludes the reviewed private fact and provenance',
                          foreign_uses == 0 and derived_content not in foreign_prompt and canary not in foreign_prompt)
                    report['real_cross_source_acl_tested'] = True

                    harness.call('DELETE', PREFIX + '/memory/' + memory['id'],
                                 headers={'If-Match': '"' + str(memory['revision']) + '"'})
                    reserve(harness)
                    forgotten_run = harness.submit(recall, '请回答先前已经遗忘的阅读偏好；当前无可用记录时只回答UNKNOWN，不推测。')
                    wait_run(harness, forgotten_run)
                    forgotten_prompt = json.dumps(snapshot(harness, forgotten_run), ensure_ascii=False)
                    with harness.db() as database:
                        forgotten_uses = database.execute('SELECT count(*) FROM memory_run_uses WHERE run_id=? AND memory_id=?',
                                                          (forgotten_run, memory['id'])).fetchone()[0]
                    check('forget removes the reviewed fact and old recall echoes from subsequent real-model context',
                          forgotten_uses == 0 and derived_content not in forgotten_prompt and canary not in forgotten_prompt)
                    report.update(real_forget_tested=True, status='passed')
                with harness.db() as database:
                    report['candidate_model_requests'] = database.execute('SELECT count(*) FROM memory_candidate_jobs').fetchone()[0]
                    report['summary_model_requests'] = database.execute('SELECT count(*) FROM summary_jobs').fetchone()[0]
                report['foreground_model_requests'] = foreground_calls
            finally:
                harness.close()
    except Exception as error:
        result_code = 1
        report.update(status='failed', failure_after_check=stage, error_type=type(error).__name__)
        print('FAIL isolated real-model candidate acceptance; inspect sanitized metadata', flush=True)
    finally:
        key = None
        report.update(finished_at=candidate.acceptance.utc_now(), passed=len(report['checks']))
        destination = pathlib.Path(args.report).resolve()
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps({'status': report['status'], 'passed': report['passed'],
                          'real_candidate_extraction_tested': report['real_candidate_extraction_tested'],
                          'real_candidate_approval_tested': report['real_candidate_approval_tested']}))
    return result_code


if __name__ == '__main__':
    raise SystemExit(main())
