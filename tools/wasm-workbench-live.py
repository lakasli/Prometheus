#!/usr/bin/env python3
"""One real DeepSeek WAT generation and actual isolated-worker review cycle.

Only existing model configuration/credential are read from the deployment.
Inputs are synthetic integer-addition requirements. No production chats, memory,
personas, artifacts or channel accounts are read or modified. No automatic retry.
"""
import argparse
import copy
import hashlib
import importlib.util
import json
import pathlib
import platform
import sqlite3
import sys
import tempfile
import threading
import types
import uuid
from contextlib import closing

sys.dont_write_bytecode = True


def local_module(name, filename):
    spec = importlib.util.spec_from_file_location(name, pathlib.Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


workbench = local_module('wasm_live_workbench', 'wasm-workbench-smoke.py')
live_helpers = local_module('wasm_live_daemon', 'memory-candidate-live.py')
preflight = local_module('wasm_live_private_key', 'vision-preflight.py')
PREFIX, ROOT = workbench.PREFIX, workbench.ROOT


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--source-data-dir', required=True)
    parser.add_argument('--report', required=True)
    args = parser.parse_args()
    binary, web = pathlib.Path(args.binary).resolve(strict=True), pathlib.Path(args.web_dir).resolve(strict=True)
    source = pathlib.Path(args.source_data_dir).resolve(strict=True)
    with binary.open('rb') as handle:
        binary_hash = hashlib.file_digest(handle, 'sha256').hexdigest()
    report = {'kind': 'WAT workbench isolated real DeepSeek generation and systemd-worker acceptance',
              'started_at': workbench.helpers.acceptance.utc_now(), 'commit': None, 'deployment_sha': None,
              'binary_sha256': binary_hash, 'platform': platform.system(), 'architecture': platform.machine(),
              'status': 'running', 'real_external_model_attempted': False, 'real_external_model_tested': False,
              'actual_systemd_worker_tested': False, 'manual_approval_cycle_tested': False,
              'production_model_configuration_read': False, 'production_model_credential_read': False,
              'production_database_modified': False, 'real_user_content_accessed': False,
              'real_qq_tested': False, 'real_weixin_private_chat_tested': False,
              'model_request_budget': 1, 'resource_stress_probe_tested': False, 'checks': []}
    stage, key, exit_code = 'private model setup', None, 0

    def check(name, condition):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    try:
        endpoint, key = preflight.configured_key(source)
        with closing(sqlite3.connect((source / 'dsh.db').as_uri() + '?mode=ro', uri=True)) as database:
            database.execute('PRAGMA query_only=ON')
            row = database.execute("SELECT json_extract(c.config_json,'$.model') FROM config_revisions c JOIN settings s ON s.key='active_revision' AND c.revision=CAST(s.value AS INTEGER)").fetchone()
        model = json.loads(row[0])
        if model['base_url'].rstrip('/') != endpoint:
            raise RuntimeError('model configuration changed during private setup')
        report.update(production_model_configuration_read=True, production_model_credential_read=True)
        with tempfile.TemporaryDirectory(prefix='dsh-wasm-workbench-live-') as temporary:
            fixture = types.SimpleNamespace(hold_release=threading.Event())
            harness = live_helpers.LiveHarness(str(binary), str(web), temporary, fixture)
            try:
                harness.start()
                report['commit'] = report['deployment_sha'] = harness.commit
                harness.bootstrap()
                page, _, _ = harness.call('GET', ROOT)
                if not page['available']:
                    report.update(status='unavailable', availability_code=page.get('availability_code'))
                    exit_code = 2
                else:
                    current, _, _ = harness.call('GET', PREFIX + '/config')
                    credential, _, _ = harness.call('POST', PREFIX + '/secrets', {'kind': 'model_api_key', 'value': key})
                    key = None
                    config = copy.deepcopy(current['config'])
                    config['model'] = dict(model, credential_ref=credential['id'])
                    config['qq']['enabled'] = False
                    draft, _, _ = harness.call('POST', PREFIX + '/config/drafts', {'base_revision': current['revision'], 'config': config})
                    path = PREFIX + '/config/drafts/' + draft['id']
                    valid, _, _ = harness.call('POST', path + '/validate', {})
                    check('fresh isolated daemon validates only the copied official model credential', valid['valid'])
                    harness.call('POST', path + '/apply', {}, expected=202, headers={'If-Match': '"' + str(current['revision']) + '"'})
                    baseline = workbench.unrelated_counts(harness)
                    request = workbench.generation('这是一次临时合成验收。请生成无任何导入或内存的纯i32加法模块：calc(a,b)返回a与b的32位回绕和。只返回严格JSON对象{"wat":"WAT正文"}，不使用Markdown。')
                    report['real_external_model_attempted'] = True
                    queued, _, _ = harness.call('POST', ROOT + '/generate', request, expected=202)
                    generated = workbench.wait_job(harness, queued['id'])
                    private = workbench.wait_released(harness, queued['id'])
                    report['real_external_model_tested'] = generated['status'] == 'completed' and private['model_started_at'] is not None and private['model_finished_at'] is not None
                    if generated['status'] != 'completed':
                        report.update(status='inconclusive', outcome='single_real_generation_did_not_produce_a_tested_artifact', error_code=generated.get('error_code'))
                        exit_code = 2
                    else:
                        artifact, _, _ = harness.call('GET', workbench.artifact_path(generated['artifact_id']))
                        check('real model WAT passes every synthetic user case in the actual bounded worker',
                              artifact['status'] == 'ready' and len(artifact['test_results']) == 2
                              and all(item['passed'] for item in artifact['test_results']))
                        duplicate, _, _ = harness.call('POST', ROOT + '/generate', request, expected=202)
                        check('same real-generation request returns existing artifact without a second model call', duplicate['id'] == queued['id'])
                        approved = workbench.approve(harness, artifact)
                        execution = workbench.execute(harness, approved, a=123, b=-46)
                        result = workbench.wait_job(harness, execution['id'])
                        execution_private = workbench.wait_released(harness, execution['id'])
                        check('explicit hash approval enables real pure-i32 execution on an unseen input',
                              result['status'] == 'completed' and result['result'] == 77
                              and execution_private['worker_started_at'] is not None and execution_private['worker_finished_at'] is not None)
                        report.update(actual_systemd_worker_tested=True, manual_approval_cycle_tested=True)
                        revoked, _, _ = harness.call('POST', workbench.artifact_path(approved['id']) + '/revoke', {}, headers={'If-Match': '"' + str(approved['revision']) + '"'})
                        workbench.execute(harness, revoked, expected=409)
                        history, _, _ = harness.call('GET', workbench.job_path(execution['id']))
                        check('revocation removes new execution authority and preserves prior private computation evidence', history['result'] == 77 and revoked['status'] == 'revoked')
                        deleted, _, _ = harness.call('DELETE', workbench.job_path(execution['id']), headers={'If-Match': '"' + str(history['revision']) + '"'})
                        row = workbench.job_row(harness, execution['id'])
                        check('explicit deletion clears private execution arguments and result', deleted['status'] == 'deleted' and row['args_json'] is None and row['result'] is None)
                        report['status'] = 'passed'
                    with harness.db() as database:
                        calls = database.execute("SELECT count(*) FROM wasm_workbench_jobs WHERE model_started_at IS NOT NULL").fetchone()[0]
                    check('single-attempt generation and standalone execution never populate chat or memory', calls == 1 and workbench.unrelated_counts(harness) == baseline)
                    report['model_requests'] = calls
            finally:
                try:
                    workbench.cleanup_units(harness)
                finally:
                    harness.close()
    except Exception as error:
        exit_code = 1
        report.update(status='failed', failure_after_check=stage, error_type=type(error).__name__)
        trace = error.__traceback__
        while trace:
            if trace.tb_frame.f_code.co_filename == __file__:
                report['failure_line'] = trace.tb_lineno
            trace = trace.tb_next
        if isinstance(error, workbench.helpers.ResponseStatusError):
            report.update(actual_http_status=error.actual, expected_http_statuses=error.expected)
        print('FAIL isolated real WAT workbench; inspect sanitized metadata', flush=True)
    finally:
        key = None
        report.update(finished_at=workbench.helpers.acceptance.utc_now(), passed=len(report['checks']))
        path = pathlib.Path(args.report).resolve()
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps({'status': report['status'], 'passed': report['passed'], 'real_external_model_tested': report['real_external_model_tested'], 'actual_systemd_worker_tested': report['actual_systemd_worker_tested']}))
    return exit_code


if __name__ == '__main__':
    raise SystemExit(main())
