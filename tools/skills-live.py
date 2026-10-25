#!/usr/bin/env python3
"""Bounded real-model Skill acceptance using a private temporary daemon.

Read only the existing model configuration and referenced credential. No real
conversation, memory, character or channel content is read or copied, and the
production database is never written. At most three synthetic text requests
exercise creator, reviewed Skill use and HTML drawing. Native image generation
is explicitly outside this check. Reports never contain outputs or credentials.
"""
import argparse
import copy
import hashlib
import html.parser
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


skills = module('live_skills_helpers', 'skills-smoke.py')
live = module('live_skills_daemon', 'memory-candidate-live.py')
PREFIX, SKILLS = skills.PREFIX, skills.SKILLS


def fenced(content, language):
    match = re.search(r'```' + language + r'\s*\n([\s\S]*?)```', content, re.IGNORECASE)
    if match is None:
        raise AssertionError('real Skill output is missing its declared code format')
    return match[1].strip()


class DrawingParser(html.parser.HTMLParser):
    def __init__(self):
        super().__init__()
        self.safe, self.visual = True, False

    def handle_starttag(self, tag, attrs):
        if tag in {'script', 'iframe', 'object', 'embed', 'form', 'link', 'base'}:
            self.safe = False
        if tag in {'svg', 'circle', 'rect', 'path', 'div'}:
            self.visual = True
        for name, value in attrs:
            if tag == 'use' and name in {'href', 'xlink:href'} and value and re.fullmatch(r'#[A-Za-z_][\w:.-]*', value):
                continue
            if name.startswith('on') or name in {'src', 'href', 'action', 'srcdoc'} or (name == 'http-equiv'):
                self.safe = False
            if value and external_css(value):
                self.safe = False

    handle_startendtag = handle_starttag


def external_css(value):
    return (bool(re.search(r'@import|javascript:', value, re.IGNORECASE))
            or any(not target.strip().strip('\"\'').startswith('#')
                   for target in re.findall(r'url\s*\(([^)]*)\)', value, re.IGNORECASE)))


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
    report = {
        'kind': 'Skill isolated real configured-model acceptance',
        'started_at': skills.helpers.acceptance.utc_now(), 'commit': None,
        'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
        'platform': platform.system(), 'architecture': platform.machine(),
        'production_model_configuration_read': False, 'production_model_credential_read': False,
        'production_database_modified': False, 'real_user_content_accessed': False,
        'synthetic_conversations_only': True, 'model_request_budget': 3,
        'real_external_model_attempted': False, 'real_external_model_tested': False,
        'real_skill_creator_tested': False, 'real_created_skill_use_tested': False,
        'real_html_drawing_generated': False, 'html_browser_rendering_tested': False,
        'real_external_image_generation_tested': False,
        'html_mode_selected_in_isolated_config': True,
        'real_qq_tested': False, 'real_weixin_private_chat_tested': False,
        'status': 'running', 'checks': [],
    }
    stage, count, code, key = 'read-only model setup', 0, 0, None

    def check(name, condition=True):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    def submit(harness, conversation, prompt, identifier):
        nonlocal count, stage
        stage = 'bounded real-model Skill dispatch'
        if count >= report['model_request_budget']:
            raise RuntimeError('real-model Skill acceptance request budget exhausted')
        count += 1
        report['real_external_model_attempted'] = True
        return skills.submit(harness, conversation, prompt, identifier)['run_id']

    try:
        with closing(sqlite3.connect((source / 'dsh.db').as_uri() + '?mode=ro', uri=True)) as database:
            database.execute('PRAGMA query_only=ON')
            row = database.execute(
                "SELECT json_extract(c.config_json,'$.model') FROM config_revisions c JOIN settings s "
                "ON s.key='active_revision' AND c.revision=CAST(s.value AS INTEGER)").fetchone()
        if row is None:
            raise RuntimeError('existing model configuration unavailable')
        model = json.loads(row[0])
        report['production_model_configuration_read'] = True
        endpoint = urllib.parse.urlsplit(model['base_url'])
        if (endpoint.scheme != 'https' or not endpoint.hostname or endpoint.username or endpoint.password
                or endpoint.hostname in {'localhost', 'localhost.localdomain'}):
            raise RuntimeError('real-model Skill acceptance requires an external HTTPS provider')
        try:
            address = ipaddress.ip_address(endpoint.hostname)
        except ValueError:
            address = None
        if address is not None and not address.is_global:
            raise RuntimeError('private endpoint cannot be labelled a real external provider')
        credential_id = str(uuid.UUID(model['credential_ref']))
        store = (source / 'secrets').resolve(strict=True)
        credential_path = (store / credential_id).resolve(strict=True)
        if not credential_path.is_relative_to(store):
            raise RuntimeError('model credential escaped its existing private store')
        key = credential_path.read_text()
        report['production_model_credential_read'] = True
        with tempfile.TemporaryDirectory(prefix='dsh-skills-live-') as temporary:
            fixture = types.SimpleNamespace(hold_release=threading.Event())
            harness = live.LiveHarness(str(binary), str(web), temporary, fixture)
            try:
                harness.start()
                report['commit'] = report['deployment_sha'] = harness.commit
                harness.bootstrap()
                current = harness.call('GET', PREFIX + '/config')[0]
                credential = harness.call('POST', PREFIX + '/secrets', {'kind': 'model_api_key', 'value': key})[0]
                key = None
                config = copy.deepcopy(current['config'])
                config['model'] = dict(model, credential_ref=credential['id'], image_generation=False)
                config['qq']['enabled'] = False
                config['assistant']['timeout_seconds'] = 120
                config['assistant']['system_prompt'] = '你是参与临时合成验收的DSH助手。请遵循本次明确选择的Skill，输出完整、可用的指定格式。'
                draft = harness.call('POST', PREFIX + '/config/drafts', {
                    'base_revision': current['revision'], 'config': config})[0]
                path = PREFIX + '/config/drafts/' + draft['id']
                validation = harness.call('POST', path + '/validate', {})[0]
                check('isolated daemon validates the copied external model with HTML drawing mode', validation['valid'])
                harness.call('POST', path + '/apply', {}, expected=202,
                             headers={'If-Match': '"' + str(current['revision']) + '"'})
                initial = harness.call('GET', SKILLS)[0]
                check('temporary instance contains only two default Skills and no native image dispatch',
                      len(initial['items']) == 2 and not initial['drawing']['native_available'])
                marker = 'SKILLLIVE' + secrets.token_hex(7).upper()
                creator_chat = harness.conversation('Synthetic real-model Skill creation')
                creator_run = submit(harness, creator_chat,
                    '制作一个用于临时合成验收的简单Skill。名称为“合成验收复述”；'
                    '它适用于用户请求运行合成验收时，执行步骤是无论用户输入什么，仅输出固定标记“' + marker + '”。'
                    '请把固定标记原样写入instructions。kind使用instruction。只输出默认要求的一个json代码块，'
                    '内容只包含name、description、instructions、kind四个字段。不要声称已经保存。', 'skill-creator')
                drawing_chat = harness.conversation('Synthetic real-model HTML drawing')
                drawing_run = submit(harness, drawing_chat,
                    '请用一个完整的html代码块绘制原创简洁图画：白色背景上一个蓝色圆形和三朵白云。'
                    '使用HTML、CSS及内联SVG，宽度适应容器。不要JavaScript、事件处理器、外部链接、'
                    '网络请求、iframe、表单或外部资源。不要生成图片链接，不要省略代码。', 'drawing')
                created_result = live.wait_run(harness, creator_run)
                report['real_external_model_tested'] = True
                generated = json.loads(fenced(created_result['response'], 'json'))
                check('real default creator returns a bounded instruction draft containing the synthetic requirement',
                      isinstance(generated, dict) and set(generated) == {'name', 'description', 'instructions', 'kind'}
                      and generated['kind'] == 'instruction'
                      and all(isinstance(generated[key], str) for key in ('name', 'description', 'instructions'))
                      and 0 < len(generated['name'].encode()) <= 128
                      and len(generated['description'].encode()) <= 1024
                      and 0 < len(generated['instructions'].encode()) <= 16000
                      and marker in generated['instructions'])
                report['real_skill_creator_tested'] = True
                check('creator output has not automatically installed a Skill', len(harness.call('GET', SKILLS)[0]['items']) == 2)
                reviewed = harness.call('POST', SKILLS, dict(generated, enabled=False), expected=201)[0]
                check('explicit review saves actual generated instructions disabled',
                      reviewed['instructions'] == generated['instructions'] and not reviewed['enabled'])
                reviewed = skills.change(harness, reviewed, enabled=True)
                execution_chat = harness.conversation('Synthetic real-model reviewed Skill execution')
                execution_run = submit(harness, execution_chat, '请运行这个已审核的合成验收技能。', reviewed['id'])
                execution_result = live.wait_run(harness, execution_run)
                check('explicitly enabled model-generated Skill executes its actual synthetic instruction',
                      marker in execution_result['response'])
                report['real_created_skill_use_tested'] = True
                drawing_result = live.wait_run(harness, drawing_run)
                drawing = fenced(drawing_result['response'], 'html')
                parsed = DrawingParser()
                parsed.feed(drawing)
                check('real default drawing Skill returns self-contained HTML visual code without active external content',
                      parsed.safe and parsed.visual
                      and bool(re.search(r'<(?:html|svg)\b', drawing, re.IGNORECASE))
                      and not external_css(drawing)
                      and not re.search(r'https?://(?!www\.w3\.org/2000/svg)', drawing, re.IGNORECASE))
                report['real_html_drawing_generated'] = True
                hashes = {creator_run: hashlib.sha256(created_result['response'].encode()).hexdigest(),
                          execution_run: hashlib.sha256(execution_result['response'].encode()).hexdigest(),
                          drawing_run: hashlib.sha256(drawing_result['response'].encode()).hexdigest()}
                harness.stop()
                harness.start()
                harness.login()
                restored = {item['id']: item for item in harness.call('GET', SKILLS)[0]['items']}
                check('reviewed generated Skill and all three actual model outputs survive daemon restart',
                      restored[reviewed['id']]['instructions'] == generated['instructions']
                      and restored[reviewed['id']]['enabled']
                      and all(hashlib.sha256(harness.call('GET', PREFIX + '/runs/' + identifier)[0]['response'].encode()).hexdigest() == digest
                              for identifier, digest in hashes.items()))
                with harness.db() as database:
                    no_background = all(database.execute('SELECT count(*) FROM ' + table).fetchone()[0] == 0
                                        for table in ('summary_jobs', 'memory_records', 'memory_candidate_jobs', 'weixin_account', 'outbox'))
                    actual_runs = database.execute('SELECT count(*) FROM runs').fetchone()[0]
                check('three bounded synthetic Skill Runs create no implicit memory summaries or channel work',
                      count == actual_runs == 3 and no_background)
                report['status'] = 'passed'
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
        if isinstance(error, skills.helpers.ResponseStatusError):
            report.update(actual_http_status=error.actual, expected_http_statuses=error.expected)
        print('FAIL isolated real-model Skill acceptance; see sanitized report', flush=True)
    finally:
        key = None
        report.update(finished_at=skills.helpers.acceptance.utc_now(), passed=len(report['checks']),
                      synthetic_model_requests=count)
        target = pathlib.Path(args.report).resolve()
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps({'status': report['status'], 'passed': report['passed'],
                          'real_external_image_generation_tested': False, 'report': str(target)}))
    return code


if __name__ == '__main__':
    raise SystemExit(main())
