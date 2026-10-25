#!/usr/bin/env python3
"""One synthetic real-model Rust Component generation in a fresh private daemon.

Copies only the existing model configuration and referenced key. Production
chats, memories, plugin source, user files and channel accounts are never copied.
Approval API tests affect only a temporary synthetic administrator and registry;
this does not approve or activate any production-generated plugin.
"""
import argparse
import copy
import hashlib
import importlib.util
import ipaddress
import json
import pathlib
import platform
import sqlite3
import sys
import tempfile
import threading
import time
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


factory = module('component_live_factory', 'plugin-factory-smoke.py')
live = module('component_live_daemon', 'memory-candidate-live.py')
ROOT, PREFIX = factory.ROOT, factory.helpers.PREFIX


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--source-data-dir', required=True)
    parser.add_argument('--report', required=True)
    parser.add_argument('--include-chat-command', action='store_true')
    args = parser.parse_args()
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    source = pathlib.Path(args.source_data_dir).resolve(strict=True)
    report = {'kind': 'isolated real-model Rust WIT Component acceptance',
              'started_at': factory.helpers.acceptance.utc_now(), 'commit': None,
              'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'platform': platform.system(), 'architecture': platform.machine(),
              'production_model_configuration_read': False, 'production_model_credential_read': False,
              'production_database_modified': False, 'real_user_content_accessed': False,
              'production_plugin_approved': False, 'human_approval_tested': False,
              'model_request_budget': 1, 'real_external_model_attempted': False,
              'real_external_model_tested': False, 'actual_component_worker_tested': False,
              'synthetic_approval_api_cycle_tested': False, 'chat_command_tested': False,
              'real_qq_tested': False, 'real_weixin_tested': False,
              'status': 'running', 'checks': []}
    key, code, stage = None, 0, 'private model setup'

    def check(name, condition):
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
        endpoint = urllib.parse.urlsplit(model['base_url'])
        if endpoint.scheme != 'https' or not endpoint.hostname or endpoint.username or endpoint.password or endpoint.query or endpoint.fragment or endpoint.hostname in {'localhost', 'localhost.localdomain'}:
            raise RuntimeError('real-model acceptance requires the configured external HTTPS endpoint')
        try:
            address = ipaddress.ip_address(endpoint.hostname)
        except ValueError:
            address = None
        if address is not None and not address.is_global:
            raise RuntimeError('private endpoint cannot be labelled a real external provider')
        reference = str(uuid.UUID(model['credential_ref']))
        store = (source / 'secrets').resolve(strict=True)
        path = (store / reference).resolve(strict=True)
        if not path.is_relative_to(store):
            raise RuntimeError('model credential escaped its private store')
        key = path.read_text()
        report['production_model_credential_read'] = True
        with tempfile.TemporaryDirectory(prefix='dsh-component-factory-live-') as temporary:
            fixture = types.SimpleNamespace(hold_release=threading.Event())
            harness = live.LiveHarness(str(binary), str(web), temporary, fixture)
            try:
                harness.start()
                harness.bootstrap()
                report['commit'] = report['deployment_sha'] = harness.commit
                page = harness.call('GET', ROOT)[0]
                if not page['available']:
                    report.update(status='unavailable', availability_code=page.get('availability_code'))
                    code = 2
                else:
                    current = harness.call('GET', PREFIX + '/config')[0]
                    credential = harness.call('POST', PREFIX + '/secrets', {'kind': 'model_api_key', 'value': key})[0]
                    key = None
                    config = copy.deepcopy(current['config'])
                    config['model'] = dict(model, credential_ref=credential['id'])
                    config['qq']['enabled'] = False
                    config['assistant']['timeout_seconds'] = 120
                    draft = harness.call('POST', PREFIX + '/config/drafts', {'base_revision': current['revision'], 'config': config})[0]
                    path = PREFIX + '/config/drafts/' + draft['id']
                    valid = harness.call('POST', path + '/validate', {})[0]
                    check('fresh isolated daemon validates only the copied external model configuration', valid['valid'])
                    harness.call('POST', path + '/apply', {}, expected=202, headers={'If-Match': '"' + str(current['revision']) + '"'})
                    spec = factory.specification()
                    spec['description'] = '这是临时合成验收。请生成纯Rust整数数组求和工具：input是一个严格JSON整数数组字符串，例如[2,3]，应返回求和结果的JSON整数字符串。允许负数和空数组；没有serde或任何第三方crate，只有标准库。可用trim、split、parse::<i64>()和sum，返回值必须为String。只生成固定函数plugin(input:&str)->String内部函数体，不要函数声明，不要Markdown。'
                    request = {'client_request_id': str(uuid.uuid4()), 'specification': spec, 'body': None}
                    report['real_external_model_attempted'] = True
                    item = harness.call('POST', ROOT, request, expected=201)[0]
                    deadline = time.monotonic() + 135
                    while time.monotonic() < deadline:
                        item = factory.refresh(harness, item)
                        if item['status'] not in {'specified', 'generating'}:
                            break
                        time.sleep(0.2)
                    report['real_external_model_tested'] = item['status'] == 'generated' and bool(item.get('body'))
                    if not report['real_external_model_tested']:
                        report.update(status='inconclusive', outcome='single_real_generation_did_not_produce_rust_source')
                        code = 2
                    else:
                        check('one real model request produces an unapproved private Rust source version', item['status'] == 'generated' and item['component_sha256'] is None)
                        item, built = factory.action(harness, item, 'build')
                        if built['status'] != 'completed':
                            report.update(status='inconclusive', outcome='single_real_generated_source_did_not_compile', error_code=built.get('error_code'))
                            code = 2
                        else:
                            item, tested = factory.action(harness, item, 'test')
                            if tested['status'] != 'completed':
                                report.update(status='inconclusive', outcome='single_real_generated_component_failed_contract_tests', error_code=tested.get('error_code'))
                                code = 2
                            else:
                                check('real generated Rust compiles to WIT Component and passes actual Wasmtime contract tests', item['status'] == 'eligible' and item['test_report']['passed'])
                                report['actual_component_worker_tested'] = True
                                item = factory.activate(harness, factory.approve(harness, item))
                                output = harness.call('POST', ROOT + '/plugins/user.synthetic-sum/invoke', {'input': [123, -46]})[0]
                                check('separate synthetic authenticated approval and activation enable unseen input result seventy seven', output['output'] == 77)
                                report['synthetic_approval_api_cycle_tested'] = True
                                if args.include_chat_command:
                                    conversation = harness.conversation('Synthetic explicit Component tool command')
                                    run = harness.submit(conversation, '/plugin user.synthetic-sum [123,-46]')
                                    result = live.wait_run(harness, run)
                                    check('explicit chat command routes the authenticated owners active component through the normal conversation result path', 'user.synthetic-sum' in result['response'] and '77' in result['response'])
                                    report['chat_command_tested'] = True
                                item = factory.refresh(harness, item)
                                harness.call('POST', factory.version_path(item) + '/quarantine', {}, headers=factory.tag(item))
                                harness.call('POST', ROOT + '/plugins/user.synthetic-sum/invoke', {'input': [123, -46]}, expected=404)
                                check('quarantine removes tool authority after the synthetic invocation', factory.refresh(harness, item)['status'] == 'quarantined')
                                report['status'] = 'passed'
                    with harness.db() as database:
                        calls = database.execute("SELECT count(*) FROM plugin_factory_jobs WHERE kind='generate' AND status NOT IN ('queued')").fetchone()[0]
                        memories = database.execute('SELECT count(*) FROM memory_records').fetchone()[0]
                        unfinished = database.execute('SELECT count(*) FROM plugin_factory_units WHERE finished_at IS NULL').fetchone()[0]
                    check('bounded real-model generation uses one request and no long-term memory or surviving worker', calls == 1 and memories == 0 and unfinished == 0)
                    report['factory_model_requests'] = calls
            finally:
                try:
                    factory.cleanup_units(harness)
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
        if isinstance(error, factory.helpers.ResponseStatusError):
            report.update(actual_http_status=error.actual, expected_http_statuses=error.expected)
        print('FAIL isolated real Component factory; inspect sanitized metadata', flush=True)
    finally:
        key = None
        report.update(finished_at=factory.helpers.acceptance.utc_now(), passed=len(report['checks']))
        destination = pathlib.Path(args.report).resolve()
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps({'status': report['status'], 'passed': report['passed'], 'real_external_model_tested': report['real_external_model_tested'], 'actual_component_worker_tested': report['actual_component_worker_tested']}))
    return code


if __name__ == '__main__':
    raise SystemExit(main())
