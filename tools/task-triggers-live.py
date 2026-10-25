#!/usr/bin/env python3
"""Bounded real-model task setup and scheduled execution in a private daemon.

Reads only the current production model configuration and its referenced key.
No real conversations, memories, channels, personas or existing jobs are copied.
Defaults to two model calls; --include-html permits one additional drawing call.
Reports never include prompts, outputs, cookies or credentials.
"""
import argparse
import copy
import hashlib
import importlib.util
import ipaddress
import json
import pathlib
import platform
import re
import secrets
import sqlite3
import sys
import tempfile
import threading
import types
import urllib.parse
import uuid
from contextlib import closing

sys.dont_write_bytecode = True


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, pathlib.Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


trigger = module('live_trigger_helpers', 'task-triggers-smoke.py')
skills_live = module('live_trigger_skill_checks', 'skills-live.py')
live = skills_live.live
PREFIX, ROOT = trigger.PREFIX, trigger.ROOT


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--source-data-dir', required=True)
    parser.add_argument('--report', required=True)
    parser.add_argument('--include-html', action='store_true')
    args = parser.parse_args()
    binary, web = pathlib.Path(args.binary).resolve(strict=True), pathlib.Path(args.web_dir).resolve(strict=True)
    source = pathlib.Path(args.source_data_dir).resolve(strict=True)
    report = {'kind': 'task triggers isolated configured real-model acceptance',
              'started_at': trigger.helpers.acceptance.utc_now(), 'commit': None,
              'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'platform': platform.system(), 'architecture': platform.machine(),
              'production_model_configuration_read': False, 'production_model_credential_read': False,
              'production_database_modified': False, 'real_user_content_accessed': False,
              'model_request_budget': 3 if args.include_html else 2,
              'synthetic_conversations_only': True, 'real_external_model_attempted': False,
              'real_external_model_tested': False, 'real_setup_skill_tested': False,
              'real_scheduled_execution_tested': False, 'real_scheduled_html_tested': False,
              'real_external_image_generation_tested': False,
              'real_qq_tested': False, 'real_weixin_private_chat_tested': False,
              'status': 'running', 'checks': []}
    stage, code, key = 'read-only model setup', 0, None

    def check(name, condition=True):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    try:
        with closing(sqlite3.connect((source / 'dsh.db').as_uri() + '?mode=ro', uri=True)) as database:
            database.execute('PRAGMA query_only=ON')
            row = database.execute("SELECT json_extract(c.config_json,'$.model') FROM config_revisions c JOIN settings s ON s.key='active_revision' AND c.revision=CAST(s.value AS INTEGER)").fetchone()
        if row is None:
            raise RuntimeError('existing model configuration unavailable')
        model = json.loads(row[0])
        report['production_model_configuration_read'] = True
        url = urllib.parse.urlsplit(model['base_url'])
        if url.scheme != 'https' or not url.hostname or url.username or url.password or url.query or url.fragment:
            raise RuntimeError('real-model acceptance requires the configured external HTTPS endpoint')
        if url.hostname in {'localhost', 'localhost.localdomain'}:
            raise RuntimeError('loopback cannot be labelled a real provider')
        try:
            address = ipaddress.ip_address(url.hostname)
        except ValueError:
            address = None
        if address is not None and not address.is_global:
            raise RuntimeError('private endpoint cannot be labelled a real provider')
        credential = str(uuid.UUID(model['credential_ref']))
        store = (source / 'secrets').resolve(strict=True)
        credential_path = (store / credential).resolve(strict=True)
        if not credential_path.is_relative_to(store):
            raise RuntimeError('credential escaped its private store')
        key = credential_path.read_text()
        report['production_model_credential_read'] = True
        with tempfile.TemporaryDirectory(prefix='dsh-triggers-live-') as temporary:
            fixture = types.SimpleNamespace(hold_release=threading.Event())
            harness = live.LiveHarness(str(binary), str(web), temporary, fixture)
            try:
                harness.start(); harness.bootstrap()
                report['commit'] = report['deployment_sha'] = harness.commit
                trigger.acknowledge_clock(harness)
                current = harness.call('GET', PREFIX + '/config')[0]
                secret = harness.call('POST', PREFIX + '/secrets', {'kind': 'model_api_key', 'value': key})[0]
                key = None
                config = copy.deepcopy(current['config'])
                config['model'] = dict(model, credential_ref=secret['id'], image_generation=False)
                config['qq']['enabled'] = False
                config['assistant']['timeout_seconds'] = 120
                config['assistant']['system_prompt'] = '你是合成任务验收助手，按当前明确选择的Skill输出所需格式，不声称执行不存在的工具。'
                draft = harness.call('POST', PREFIX + '/config/drafts', {'base_revision': current['revision'], 'config': config})[0]
                path = PREFIX + '/config/drafts/' + draft['id']
                check('isolated real-model configuration validates without enabling channels', harness.call('POST', path + '/validate', {})[0]['valid'])
                harness.call('POST', path + '/apply', {}, expected=202, headers={'If-Match': '"' + str(current['revision']) + '"'})
                marker = 'TRIGGERLIVE' + secrets.token_hex(7).upper()
                source_only = 'SOURCEONLY' + secrets.token_hex(7).upper()
                intended = trigger.model_definition('请只回复这个固定合成标记，不要其他文字：' + marker, delay=2)
                intended['title'] = '合成触发验收'
                intended['budget']['max_output_tokens'] = 128
                intended['budget']['daily_model_runs'] = 2
                conversation = harness.conversation('Synthetic real-model trigger setup')
                report['real_external_model_attempted'] = True
                run = trigger.skills.submit(harness, conversation,
                    '这是合成验收，请生成可审核的任务草稿，不要声称已创建。来源会话标记' + source_only + '只属于本消息，不能放入任务。'
                    '首次启用后2秒执行一次模型任务，结果进入Web。请严格输出一个json代码块，以task_trigger_draft为唯一外层键。'
                    '任务内容仅回复指定固定标记，使用以下完整且已明确的计划，不添加enabled、owner或其他字段：\n' + json.dumps({'task_trigger_draft': intended}, ensure_ascii=False),
                    'task-trigger-setup')['run_id']
                generated_result = live.wait_run(harness, run)
                generated = json.loads(skills_live.fenced(generated_result['response'], 'json'))
                check('real setup Skill returns a task proposal and has not created or enabled a job',
                      set(generated) == {'task_trigger_draft'} and isinstance(generated['task_trigger_draft'], dict)
                      and harness.call('GET', ROOT + '/jobs')[0]['total'] == 0)
                actual = generated['task_trigger_draft']
                check('real proposal preserves isolated one-shot action and the synthetic instruction',
                      actual.get('action', {}).get('type') == 'assistant_run'
                      and marker in actual['action'].get('prompt', '')
                      and source_only not in json.dumps(actual, ensure_ascii=False)
                      and actual.get('trigger') == intended['trigger']
                      and actual.get('delivery') == {'kind': 'web_inbox'}
                      and actual['action'].get('context_policy') == 'isolated')
                reviewed = trigger.create(harness, actual)
                check('explicit review saves actual model-generated proposal disabled', reviewed['lifecycle'] == 'draft' and not trigger.occurrences(harness, reviewed))
                approved = trigger.activate(harness, reviewed, actual)
                result = trigger.wait_occurrence(harness, approved, timeout=150)
                check('reviewed real-model task executes after activation and persists the requested answer',
                      result['status'] == 'completed' and result['delivery_status'] == 'stored' and marker in result.get('response', ''))
                with harness.db() as database:
                    snapshot = database.execute('SELECT messages_json FROM run_prompt_snapshots WHERE run_id=?', (result['run_id'],)).fetchone()
                check('scheduled actual model context does not inherit its setup conversation', snapshot is not None and source_only not in snapshot[0])
                report.update(real_external_model_tested=True, real_setup_skill_tested=True, real_scheduled_execution_tested=True)
                expected_runs = 2
                if args.include_html:
                    drawing = next(item for item in harness.call('GET', PREFIX + '/skills')[0]['items'] if item['id'] == 'drawing')
                    html = trigger.model_definition('请只输出一个完整html代码块，用HTML/CSS和内联SVG画一个蓝色圆形。不要脚本、事件处理器、外链或外部资源。', skill={'id': drawing['id'], 'revision': drawing['revision']})
                    html['action']['output_mode'] = 'html_drawing'
                    html_job = trigger.activate(harness, trigger.create(harness, html), html)
                    html_result = trigger.wait_occurrence(harness, html_job, timeout=150)
                    code_html = skills_live.fenced(html_result.get('response', ''), 'html')
                    parsed = skills_live.DrawingParser(); parsed.feed(code_html); parsed.close()
                    check('approved scheduled drawing uses real self-contained HTML output', html_result['status'] == 'completed' and parsed.safe and parsed.visual and not skills_live.external_css(code_html))
                    report['real_scheduled_html_tested'] = True
                    expected_runs = 3
                original_hash = hashlib.sha256(result['response'].encode()).hexdigest()
                harness.stop(); harness.start(); harness.login(); trigger.acknowledge_clock(harness)
                restored = trigger.occurrences(harness, approved)
                check('restart preserves the settled real-model result without executing the one-shot again',
                      len(restored) == 1 and hashlib.sha256(restored[0].get('response', '').encode()).hexdigest() == original_hash)
                with harness.db() as database:
                    counts = {table: database.execute('SELECT count(*) FROM ' + table).fetchone()[0]
                              for table in ('runs', 'memory_records', 'memory_candidate_jobs', 'summary_jobs', 'outbox', 'weixin_account')}
                check('bounded synthetic live runs create no memory summaries candidates or channel side effects',
                      counts['runs'] == expected_runs and all(counts[name] == 0 for name in counts if name != 'runs'))
                report.update(status='passed', synthetic_model_requests=expected_runs)
            finally:
                harness.close()
    except Exception as error:
        code = 1
        report.update(status='failed', failure_after_check=stage, error_type=type(error).__name__)
        trace = error.__traceback__
        while trace:
            if trace.tb_frame.f_code.co_filename == __file__:
                report['failure_line'] = trace.tb_lineno
            trace = trace.tb_next
        if isinstance(error, trigger.helpers.ResponseStatusError):
            report.update(actual_http_status=error.actual, expected_http_statuses=error.expected)
        print('FAIL isolated task-trigger real-model acceptance; see sanitized report', flush=True)
    finally:
        key = None
        report.update(finished_at=trigger.helpers.acceptance.utc_now(), passed=len(report['checks']))
        pathlib.Path(args.report).write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps({'status': report['status'], 'passed': report['passed'], 'report': args.report}))
    return code


if __name__ == '__main__':
    raise SystemExit(main())
