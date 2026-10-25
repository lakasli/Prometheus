#!/usr/bin/env python3
"""Read-only QQ API acceptance with the configured account, in a private daemon.

Only the QQ account configuration and referenced credential are read from the
production deployment. Webhook mode prevents a second gateway consumer. No real
messages, conversations or memories are copied, and no QQ mutation is executed.
Per-operation status distinguishes acceptance, denial, missing resources and
unexecuted writes. The report contains no user IDs, content or credentials.
"""
import argparse
import copy
import datetime
import hashlib
import importlib.util
import json
import pathlib
import platform
import sqlite3
import tempfile
import threading
import time
import types
import urllib.error
import urllib.request
import uuid
from contextlib import closing


PREFIX = '/api/admin/v1'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--source-data-dir', required=True)
    parser.add_argument('--report', required=True)
    parser.add_argument('--parameters-file', help='Optional private JSON file of authorized platform resource IDs; never included in the report')
    args = parser.parse_args()
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    source = pathlib.Path(args.source_data_dir).resolve(strict=True)
    report_path = pathlib.Path(args.report).resolve()
    report = {
        'kind': 'QQ non-group read-only configured-account API acceptance',
        'started_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'platform': platform.system(), 'architecture': platform.machine(),
        'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
        'commit': None, 'production_database_modified': False,
        'production_qq_configuration_read': False, 'production_credential_read': False,
        'real_user_content_accessed': False, 'real_messages_sent': False,
        'mutations_executed': False, 'complete_live_coverage': False,
        'checks': [], 'operations': [], 'status': 'running',
    }
    key = None
    harness = None
    temporary = None
    failed = False
    stage = 'read configured account'

    def check(name, condition=True):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    try:
        spec = importlib.util.spec_from_file_location('qq_acceptance_helpers', pathlib.Path(__file__).with_name('memory-smoke.py'))
        helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(helper)
        with closing(sqlite3.connect((source / 'dsh.db').as_uri() + '?mode=ro', uri=True)) as db:
            db.execute('PRAGMA query_only=ON')
            row = db.execute("SELECT c.config_json FROM config_revisions c JOIN settings s ON s.key='active_revision' AND c.revision=CAST(s.value AS INTEGER)").fetchone()
        if row is None:
            raise RuntimeError('active configuration unavailable')
        qq = json.loads(row[0])['qq']
        if not qq.get('enabled') or not qq.get('credential_ref'):
            raise RuntimeError('configured QQ account unavailable')
        report['production_qq_configuration_read'] = True
        credential = str(uuid.UUID(qq['credential_ref']))
        store = (source / 'secrets').resolve(strict=True)
        secret_path = (store / credential).resolve(strict=True)
        if not secret_path.is_relative_to(store):
            raise RuntimeError('credential escaped its private store')
        key = secret_path.read_text()
        report['production_credential_read'] = True
        resources = {}
        if args.parameters_file:
            resources = json.loads(pathlib.Path(args.parameters_file).read_text())
            if not isinstance(resources, dict) or not all(isinstance(k, str) and isinstance(v, str) for k, v in resources.items()):
                raise ValueError('authorized resource map must contain strings')

        temporary = tempfile.TemporaryDirectory(prefix='dsh-qq-api-live-')
        temp = temporary.name
        fixture = types.SimpleNamespace(hold_release=threading.Event())
        harness = helper.Harness(str(binary), str(web), temp, fixture)
        harness.start()
        report['commit'] = report['deployment_sha'] = harness.commit
        harness.bootstrap()
        route = PREFIX + '/qq/api/catalog'
        harness.request('GET', route, authenticated=False, expected=401)
        check('anonymous QQ API catalog access is denied')
        harness.request('POST', PREFIX + '/qq/api/read-probe', {}, with_csrf=False, expected=403)
        check('QQ operation calls require CSRF protection')
        current = harness.request('GET', PREFIX + '/config')[0]
        stored = harness.request('POST', PREFIX + '/secrets', {'kind': 'qq_app_secret', 'value': key})[0]
        key = None
        config = copy.deepcopy(current['config'])
        config['qq'] = dict(qq, enabled=True, transport='webhook', credential_ref=stored['id'])
        config['model']['credential_ref'] = None
        draft = harness.request('POST', PREFIX + '/config/drafts', {'base_revision': current['revision'], 'config': config})[0]
        path = PREFIX + '/config/drafts/' + draft['id']
        check('isolated account configuration validates', harness.request('POST', path + '/validate', {})[0]['valid'])
        harness.request('POST', path + '/apply', {}, expected=202, headers={'If-Match': '"' + str(current['revision']) + '"'})
        catalog = harness.request('GET', route)[0]
        check('fixed QQ API catalog excludes group endpoints and model tool access', catalog['operation_count'] >= 77 and not catalog['group_apis'] and not catalog['model_tool_access'])
        check('C2C reply contract uses current four-reply bound', catalog['c2c_reply_limit'] == 4)
        operations = catalog['operations']
        # Header-only metadata reports are deliberately built separately from
        # actual platform response bodies, which may contain personal data.
        for operation in operations:
            op_id = operation['id']
            item = {'operation': op_id, 'method': operation['method'], 'path': operation['path']}
            if not operation['read_only']:
                item['outcome'] = 'not_executed_write'
                report['operations'].append(item)
                continue
            missing = [p for p in operation['parameters'] if p not in resources]
            if missing:
                item['outcome'] = 'requires_resource'
                item['missing_parameters'] = missing
                report['operations'].append(item)
                continue
            parameters = {name: resources[name] for name in operation['parameters']}
            # Use a longer HTTP timeout than the daemon's bounded QQ request.
            headers = {'Origin': harness.base, 'Content-Type': 'application/json', 'X-CSRF-Token': harness.csrf}
            request = urllib.request.Request(harness.base + PREFIX + '/qq/api/operations/' + op_id,
                json.dumps({'parameters': parameters}).encode(), headers, method='POST')
            with harness.client.open(request, timeout=55) as response:
                result = json.load(response)
            item.update({name: result.get(name) for name in ['outcome', 'http_status', 'business_code', 'trace_id']})
            report['operations'].append(item)
            data = result.get('response')
            if result.get('platform_accepted') and op_id == 'get_users_me' and isinstance(data, dict) and isinstance(data.get('id'), str):
                resources.setdefault('user_id', data['id'])
            if result.get('platform_accepted') and op_id == 'get_users_me_guilds' and isinstance(data, list) and data:
                if isinstance(data[0].get('id'), str):
                    resources.setdefault('guild_id', data[0]['id'])
            print('QQ ' + op_id + ': ' + str(item['outcome']) + ' HTTP=' + str(item.get('http_status')) + ' code=' + str(item.get('business_code')), flush=True)
            time.sleep(.4)
        report['summary'] = {name: sum(item['outcome'] == name for item in report['operations']) for name in sorted({item['outcome'] for item in report['operations']})}
        report['status'] = 'completed_with_explicit_untested_scope'
        check('each catalog operation has an explicit evidence classification', len(report['operations']) == len(operations))
    except Exception as error:
        failed = True
        report['status'] = 'failed'
        report['failure'] = {'stage': stage, 'exception_type': type(error).__name__}
        print('FAIL ' + stage + ' (' + type(error).__name__ + ')', flush=True)
    finally:
        key = None
        if harness:
            harness.close()
        if temporary:
            temporary.cleanup()
        report['finished_at'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        report_path.parent.mkdir(parents=True, exist_ok=True)
        report_path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
