#!/usr/bin/env python3
"""Exercise only newly created unbound QQ panels and synthetic image materials.

Existing menu/panel/image IDs are never mutated. New panels are C2C-specific
with an empty user list; a refusal never falls back to a global configuration.
Each created ID is recorded in a mode-0600 ownership journal before proceeding.
No message is sent. Unknown writes are never repeated. Only resources created
by this invocation, absent from the initial snapshot, are eligible for cleanup.
"""
import argparse
import base64
import copy
import datetime
import hashlib
import importlib.util
import json
import os
import pathlib
import re
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


class ResourceApiFailure(RuntimeError):
    def __init__(self, status, code=None, upstream_status=None):
        self.http_status, self.error_code, self.upstream_http_status = status, code, upstream_status
        super().__init__('isolated resource API failed')


def load_module(name, filename):
    spec = importlib.util.spec_from_file_location(name, pathlib.Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def owned_resource(kind, resource_id, initial_ids, journal, account_hash):
    """Never let a caller substitute an existing or another run's resource ID."""
    return (bool(resource_id) and resource_id not in initial_ids and
            journal['account_sha256'] == account_hash and
            journal.get(kind, {}).get('id') == resource_id and
            journal.get(kind, {}).get('created_by_this_invocation') is True)


def cleanup_needed(journal, kind):
    resource = journal.get(kind, {})
    return bool(resource.get('id') and resource.get('created_by_this_invocation') and
                not resource.get('deleted') and not resource.get('delete_intent_recorded'))


def write_private(path, value):
    temporary = path.with_name(path.name + '.' + uuid.uuid4().hex + '.tmp')
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(fd, 'w', encoding='utf-8') as output:
            json.dump(value, output, ensure_ascii=False, indent=2)
            output.write('\n')
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    except BaseException:
        # Preserve a partial journal as evidence; never issue external writes
        # again merely because reporting or cleanup failed.
        raise


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
    report_path = pathlib.Path(args.report).resolve()
    journal_path = report_path.with_suffix(report_path.suffix + '.ownership.json')
    if report_path.exists() or journal_path.exists():
        print('BLOCKED prior resource acceptance report or ownership journal exists', flush=True)
        return 2
    report_path.parent.mkdir(parents=True, exist_ok=True)
    marker = 'DSH_ACCEPTANCE_' + uuid.uuid4().hex
    report = {'kind': 'QQ isolated synthetic-resource API acceptance',
              'started_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
              'commit': None, 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'existing_menu_modified': False, 'existing_panels_modified': False,
              'existing_materials_modified': False, 'real_messages_sent': False,
              'production_database_modified': False, 'operations': [], 'checks': [],
              'status': 'running', 'cleanup_pending': False}
    journal = {'version': 1, 'marker': marker, 'account_sha256': None}
    # Exclusive reservation prevents two processes sharing one report path from
    # overwriting each other's recovery journal before the first external write.
    reservation = os.open(journal_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(reservation, 'w', encoding='utf-8') as output:
        json.dump(journal, output)
    key, harness, temporary = None, None, None
    failed = False
    stage = 'read configured account'
    initial_panels, initial_materials = set(), set()

    def save():
        write_private(report_path, report)
        write_private(journal_path, journal)

    def check(name, condition):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    def call(method, path, body=None, expected=200):
        headers = {'Origin': harness.base, 'Content-Type': 'application/json'}
        if method != 'GET':
            headers['X-CSRF-Token'] = harness.csrf
        request = urllib.request.Request(harness.base + path,
            None if body is None else json.dumps(body).encode(), headers, method=method)
        try:
            response = harness.client.open(request, timeout=60)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            raw, code = response.read(), response.code
        value = json.loads(raw)
        if code != expected:
            error = value.get('error', {}) if isinstance(value, dict) else {}
            match = re.search(r'HTTP (\d{3})', str(error.get('message', '')))
            raise ResourceApiFailure(code, error.get('code'), int(match.group(1)) if match else None)
        return value

    def operation(identifier, *, parameters=None, query=None, body=None):
        nonlocal stage, failed
        stage = identifier
        entry = {'operation': identifier, 'intent_recorded': True, 'outcome': 'request_pending'}
        report['operations'].append(entry)
        save()
        try:
            result = call('POST', PREFIX + '/qq/api/operations/' + identifier,
                {'parameters': parameters or {}, 'query': query or {}, 'body': body,
                 'acknowledge_effects': not identifier.startswith('get_')})
        except Exception:
            entry['outcome'] = 'result_unknown'
            failed = True
            save()
            raise
        entry.update({key: result.get(key) for key in ['outcome', 'http_status', 'business_code']})
        if not result.get('platform_accepted'):
            failed = True
        save()
        print('QQ ' + identifier + ': ' + str(entry['outcome']) + ' HTTP=' + str(entry.get('http_status')) + ' code=' + str(entry.get('business_code')), flush=True)
        time.sleep(1)
        return result

    def remember(kind, identifier, before):
        check('new ' + kind + ' ID is not an existing resource', isinstance(identifier, str) and bool(identifier) and identifier not in before)
        journal[kind] = {'id': identifier, 'created_by_this_invocation': True, 'deleted': False}
        journal.pop(kind + '_create_unresolved', None)
        report[kind + '_id_sha256'] = hashlib.sha256(identifier.encode()).hexdigest()
        report['cleanup_pending'] = True
        save()

    def cleanup(kind, before, operation_id, parameter):
        identifier = journal.get(kind, {}).get('id')
        if not identifier or journal[kind].get('deleted') or journal[kind].get('delete_intent_recorded'):
            return
        check('cleanup targets only this invocation owned ' + kind, owned_resource(kind, identifier, before, journal, account_hash))
        journal[kind]['delete_intent_recorded'] = True
        save()
        result = operation(operation_id, parameters={parameter: identifier})
        journal[kind]['delete_outcome'] = result['outcome']
        journal[kind]['deleted'] = bool(result.get('platform_accepted'))
        save()

    try:
        helpers = load_module('qq_resource_helpers', 'memory-smoke.py')
        media_helpers = load_module('qq_synthetic_media_helpers', 'qq-media-live.py')
        with closing(sqlite3.connect((source / 'dsh.db').as_uri() + '?mode=ro', uri=True)) as database:
            database.execute('PRAGMA query_only=ON')
            raw = database.execute("SELECT c.config_json FROM config_revisions c JOIN settings s ON s.key='active_revision' AND c.revision=CAST(s.value AS INTEGER)").fetchone()
        if raw is None:
            raise RuntimeError('active configuration unavailable')
        qq = json.loads(raw[0])['qq']
        if not qq.get('enabled') or not qq.get('credential_ref'):
            raise RuntimeError('configured QQ account unavailable')
        account_hash = hashlib.sha256(qq['app_id'].encode()).hexdigest()
        journal['account_sha256'] = account_hash
        credential = str(uuid.UUID(qq['credential_ref']))
        store = (source / 'secrets').resolve(strict=True)
        secret = (store / credential).resolve(strict=True)
        if not secret.is_relative_to(store):
            raise RuntimeError('credential escaped private store')
        key = secret.read_text()
        temporary = tempfile.TemporaryDirectory(prefix='dsh-qq-resource-live-')
        harness = helpers.Harness(str(binary), str(web), temporary.name, types.SimpleNamespace(hold_release=threading.Event()))
        harness.start()
        harness.bootstrap()
        report['commit'] = report['deployment_sha'] = harness.commit
        current = call('GET', PREFIX + '/config')
        stored = call('POST', PREFIX + '/secrets', {'kind': 'qq_app_secret', 'value': key})
        key = None
        config = copy.deepcopy(current['config'])
        config['qq'] = dict(qq, enabled=True, transport='webhook', credential_ref=stored['id'])
        config['model']['credential_ref'] = None
        draft = call('POST', PREFIX + '/config/drafts', {'base_revision': current['revision'], 'config': config})
        route = PREFIX + '/config/drafts/' + draft['id']
        check('isolated webhook-only account configuration validates', call('POST', route + '/validate', {})['valid'])
        harness.request('POST', route + '/apply', {}, expected=202, headers={'If-Match': '"' + str(current['revision']) + '"'})
        catalog = call('GET', PREFIX + '/qq/api/catalog')
        panel_contract = next(op for op in catalog['operations'] if op['id'] == 'get_v2_panels')
        check('deployed runtime implements mandatory panel scope', 'scope' in panel_contract['query_fields'] and 'scope' in panel_contract.get('required_query_fields', []))
        save()
        panels = operation('get_v2_panels', query={'scope': 'c2c', 'limit': '50'})
        materials = operation('get_v2_media_images')
        check('initial panel and material snapshots are accepted', panels.get('platform_accepted') and materials.get('platform_accepted'))
        panel_data = panels.get('response', {})
        material_data = materials.get('response', {})
        check('initial snapshot response contracts are recognized', isinstance(panel_data.get('records'), list) and isinstance(material_data.get('images'), list))
        initial_panels = {entry['panel_id'] for entry in panel_data['records']}
        initial_materials = {entry['image_id'] for entry in material_data['images']}
        check('initial snapshots cover all existing resource IDs', not panel_data.get('next_cursor') and not material_data.get('next_cursor'))
        report['initial_panel_count'] = len(initial_panels)
        report['initial_material_count'] = len(initial_materials)
        journal['panel_create_unresolved'] = True
        save()
        created = operation('post_v2_panels', body={'scope': 'c2c', 'target_type': 'specific', 'user_openids': [], 'panel': {'items': [{'type': 'command', 'name': 'DSH验收', 'desc': '临时合成接口验收'}], 'remark': marker}})
        if created.get('outcome') not in {'result_unknown', 'asynchronous_pending'} and not created.get('platform_accepted'):
            journal.pop('panel_create_unresolved', None)
            save()
        if created.get('platform_accepted'):
            identifier = created.get('response', {}).get('panel_id')
            remember('panel', identifier, initial_panels)
            detail = operation('get_v2_panels_panel_id', parameters={'panel_id': identifier})
            data = detail.get('response', {})
            check('new panel is scoped specific unbound and carries the private ownership marker', detail.get('platform_accepted') and data.get('scope') == 'c2c' and data.get('target_type') == 'specific' and not data.get('user_openids') and data.get('panel', {}).get('remark') == marker)
            changed = operation('put_v2_panels_panel_id', parameters={'panel_id': identifier}, body={'panel': {'items': [{'type': 'command', 'name': 'DSH验收', 'desc': '已更新的合成验收'}], 'remark': marker + '_UPDATED'}})
            if changed.get('platform_accepted'):
                detail = operation('get_v2_panels_panel_id', parameters={'panel_id': identifier})
                check('only the newly created panel revision changed', detail.get('platform_accepted') and detail.get('response', {}).get('panel', {}).get('remark') == marker + '_UPDATED' and not detail.get('response', {}).get('user_openids'))
            cleanup('panel', initial_panels, 'delete_v2_panels_panel_id', 'panel_id')
        png = media_helpers.synthetic_png()
        file_name = marker.lower() + '.png'
        md5 = hashlib.md5(png).hexdigest()
        report['synthetic_image'] = {'bytes': len(png), 'sha256': hashlib.sha256(png).hexdigest()}
        prepared = operation('post_v2_media_images_prepare', body={'file_name': file_name, 'file_size': len(png), 'content_type': 'image/png', 'md5': md5, 'sha1': hashlib.sha1(png).hexdigest()})
        if prepared.get('platform_accepted') and prepared.get('object_ref'):
            stage = 'synthetic material presigned PUT'
            uploaded = call('POST', PREFIX + '/qq/api/presigned-put', {'object_ref': prepared['object_ref'], 'field': 'presigned_url', 'file_data': base64.b64encode(png).decode('ascii'), 'content_type': 'image/png'})
            check('synthetic PNG PUT succeeded without exposing the presigned URL', uploaded.get('uploaded') and uploaded.get('bytes') == len(png))
            journal['material_create_unresolved'] = True
            save()
            complete = operation('post_v2_media_images_complete', body={'upload_id': {'$qq_ref': prepared['object_ref'], 'field': 'upload_id'}, 'md5': md5})
            if complete.get('outcome') not in {'result_unknown', 'asynchronous_pending'} and not complete.get('platform_accepted'):
                journal.pop('material_create_unresolved', None)
                save()
            if complete.get('platform_accepted'):
                identifier = complete.get('response', {}).get('image_id')
                remember('material', identifier, initial_materials)
                listing = operation('get_v2_media_images')
                images = listing.get('response', {}).get('images', [])
                check('only the newly uploaded synthetic material is selected', listing.get('platform_accepted') and any(image.get('image_id') == identifier and image.get('file_name') == file_name for image in images))
                cleanup('material', initial_materials, 'delete_v2_media_images_image_id_delete', 'image_id')
        final_panels = operation('get_v2_panels', query={'scope': 'c2c', 'limit': '50'})
        final_materials = operation('get_v2_media_images')
        panel_ids = {item.get('panel_id') for item in final_panels.get('response', {}).get('records', [])}
        image_ids = {item.get('image_id') for item in final_materials.get('response', {}).get('images', [])}
        check('all prior panels and materials remain and owned probes are gone', final_panels.get('platform_accepted') and final_materials.get('platform_accepted') and initial_panels <= panel_ids and initial_materials <= image_ids and journal.get('panel', {}).get('id') not in panel_ids and journal.get('material', {}).get('id') not in image_ids and not journal.get('panel_create_unresolved') and not journal.get('material_create_unresolved'))
        report['status'] = 'failed_with_explicit_platform_results' if failed else 'passed_owned_resource_crud'
    except Exception as error:
        failed = True
        report['status'] = 'failed'
        report['failure'] = {'stage': stage, 'exception_type': type(error).__name__}
        if isinstance(error, ResourceApiFailure):
            report['failure'].update(local_http_status=error.http_status, local_error_code=error.error_code, upstream_http_status=error.upstream_http_status)
        print('FAIL ' + stage + ' (' + type(error).__name__ + ')', flush=True)
    finally:
        if harness:
            for kind, before, op, parameter in [('panel', initial_panels, 'delete_v2_panels_panel_id', 'panel_id'), ('material', initial_materials, 'delete_v2_media_images_image_id_delete', 'image_id')]:
                # Never repeat an uncertain delete; leave its ownership journal.
                if cleanup_needed(journal, kind):
                    try:
                        cleanup(kind, before, op, parameter)
                    except Exception:
                        report['cleanup_pending'] = True
            harness.close()
        if temporary:
            temporary.cleanup()
        key = None
        report['cleanup_pending'] = bool(journal.get('panel_create_unresolved') or journal.get('material_create_unresolved')) or any(item.get('created_by_this_invocation') and not item.get('deleted') for item in [journal.get('panel', {}), journal.get('material', {})])
        report['finished_at'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        save()
    return 1 if failed or report['cleanup_pending'] else 0


if __name__ == '__main__':
    raise SystemExit(main())
