#!/usr/bin/env python3
"""Verify QQ C2C typing/Markdown/streaming and retract only our fresh messages.

The fixed DSH接口验收 marker must have exactly native seq=1 confirmed sent.
Typing uses seq=2; synthetic Markdown uses seq=3; one stream uses seq=4 with
index=0/1 and the same returned stream ID, as required by the official contract.
Only Markdown/stream IDs returned by this invocation may be retracted. Unknown
writes immediately block further requests; they are never automatically retried.
"""
import argparse
import copy
import datetime
import hashlib
import importlib.util
import json
import os
import pathlib
import tempfile
import threading
import time
import types
import urllib.error
import urllib.request
import uuid

PREFIX = '/api/admin/v1'
MARKER = 'DSH接口验收：'


def load_module(name, file):
    spec = importlib.util.spec_from_file_location(name, pathlib.Path(__file__).with_name(file))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def stream_fragment(message_id, index, text, stream_id=None):
    if index not in (0, 1) or (index == 1 and not stream_id) or (index == 0 and stream_id):
        raise ValueError('stream must start once at zero then finish with its own returned ID')
    body = {'input_mode': 'replace', 'input_state': 1 if index == 0 else 10,
            'index': index, 'content_type': 'text', 'content_raw': text,
            'msg_id': message_id, 'msg_seq': 4}
    if stream_id:
        body['stream_msg_id'] = stream_id
    return body


def retractable(journal, kind, identifier, account_hash):
    item = journal.get('messages', {}).get(kind, {})
    return (kind in {'markdown', 'stream'} and bool(identifier) and
            journal.get('account_sha256') == account_hash and item.get('id') == identifier and
            item.get('created_by_this_invocation') is True and item.get('seq') in (3, 4) and
            not item.get('retract_intent_recorded'))


class UnknownResult(RuntimeError):
    pass


class ControlsApiFailure(RuntimeError):
    def __init__(self, status, code=None):
        self.http_status, self.error_code = status, code
        super().__init__('isolated controls API rejected request')


def is_known_local_rejection(error):
    return isinstance(error, ControlsApiFailure) and 400 <= error.http_status < 500


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--source-data-dir', required=True)
    parser.add_argument('--source-run-id', required=True)
    parser.add_argument('--report', required=True)
    args = parser.parse_args()
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    source = pathlib.Path(args.source_data_dir).resolve(strict=True)
    source_run = str(uuid.UUID(args.source_run_id))
    report_path = pathlib.Path(args.report).resolve()
    journal_path = report_path.with_suffix(report_path.suffix + '.ownership.json')
    if report_path.exists() or journal_path.exists():
        print('BLOCKED prior interface test report or ownership journal exists', flush=True)
        return 2
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report = {'kind': 'explicit QQ C2C synthetic controls acceptance',
              'started_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
              'commit': None, 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'source_run_sha256': hashlib.sha256(source_run.encode()).hexdigest(),
              'passive_sequences': {'native': 1, 'typing': 2, 'markdown': 3, 'stream': 4},
              'native_reply_retracted': False, 'existing_messages_modified': False,
              'production_database_modified': False, 'automatic_retries': False,
              'client_verified': False, 'operations': [], 'checks': [], 'status': 'running'}
    journal = {'version': 1, 'account_sha256': None, 'source_run_id': source_run, 'messages': {}}
    fd = os.open(journal_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'w', encoding='utf-8') as output:
        json.dump(journal, output)
    resource_helpers = load_module('qq_controls_private_journal', 'qq-resource-live.py')
    media_helpers = load_module('qq_controls_source_guard', 'qq-media-live.py')
    helpers = load_module('qq_controls_daemon_helpers', 'memory-smoke.py')
    key, harness, temporary = None, None, None
    failed, blocked = False, False
    stage = 'verify explicit controls test request'

    def save():
        resource_helpers.write_private(report_path, report)
        resource_helpers.write_private(journal_path, journal)

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
            raw, status = response.read(), response.code
        value = json.loads(raw)
        if status != expected:
            error = value.get('error', {}) if isinstance(value, dict) else {}
            raise ControlsApiFailure(status, error.get('code'))
        return value

    def operation(kind, identifier, parameters, body=None):
        nonlocal failed, blocked, stage
        if blocked:
            raise UnknownResult('later operations blocked')
        latest_qq, latest_target = media_helpers.source_context(source, source_run, required_marker=MARKER)
        check('explicit source remains unchanged before ' + kind, latest_qq == qq and latest_target == target)
        stage = kind
        entry = {'kind': kind, 'operation': identifier, 'intent_recorded': True, 'outcome': 'request_pending'}
        report['operations'].append(entry)
        save()
        try:
            result = call('POST', PREFIX + '/qq/api/operations/' + identifier,
                {'parameters': parameters, 'body': body, 'acknowledge_effects': True})
        except Exception as error:
            known_local_rejection = is_known_local_rejection(error)
            entry['outcome'] = 'local_request_rejected' if known_local_rejection else 'result_unknown'
            if isinstance(error, ControlsApiFailure):
                entry['local_http_status'], entry['local_error_code'] = error.http_status, error.error_code
            blocked, failed = not known_local_rejection, True
            save()
            raise
        entry.update({key: result.get(key) for key in ['outcome', 'http_status', 'business_code']})
        entry['platform_accepted'] = bool(result.get('platform_accepted'))
        print('QQ ' + kind + ': ' + str(entry['outcome']) + ' HTTP=' + str(entry.get('http_status')) + ' code=' + str(entry.get('business_code')), flush=True)
        if entry['outcome'] in {'result_unknown', 'asynchronous_pending'}:
            blocked, failed = True, True
            save()
            raise UnknownResult('control operation outcome unknown')
        if not entry['platform_accepted']:
            failed = True
        save()
        time.sleep(3)
        return result

    def remember(kind, result, seq):
        nonlocal blocked, failed
        identifier = result.get('response', {}).get('id')
        if not isinstance(identifier, str) or not identifier or identifier == target['message_id']:
            blocked, failed = True, True
            report['operations'][-1]['outcome'] = 'result_unknown_missing_new_message_id'
            save()
            raise UnknownResult('new message ownership not established')
        journal['messages'][kind] = {'id': identifier, 'seq': seq, 'created_by_this_invocation': True, 'created_at': time.time(), 'retracted': False}
        save()
        return identifier

    def retract(kind, identifier):
        item = journal['messages'][kind]
        check('retraction targets only this invocation fresh ' + kind, retractable(journal, kind, identifier, account_hash) and identifier != target['message_id'] and time.time() - item['created_at'] < 90)
        item['retract_intent_recorded'] = True
        save()
        result = operation(kind + '_retract', 'delete_v2_users_user_openid_messages_message_id', {'user_openid': target['target'], 'message_id': identifier})
        item['retracted'] = bool(result.get('platform_accepted'))
        item['retract_outcome'] = result['outcome']
        save()

    try:
        qq, target = media_helpers.source_context(source, source_run, required_marker=MARKER)
        check('fixed interface marker owner account window and exactly native seq1 verified', True)
        account_hash = hashlib.sha256(qq['app_id'].encode()).hexdigest()
        journal['account_sha256'] = account_hash
        credential = str(uuid.UUID(qq['credential_ref']))
        store = (source / 'secrets').resolve(strict=True)
        secret = (store / credential).resolve(strict=True)
        if not secret.is_relative_to(store):
            raise RuntimeError('QQ credential escaped private store')
        key = secret.read_text()
        temporary = tempfile.TemporaryDirectory(prefix='dsh-qq-controls-live-')
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
        check('isolated webhook-only test account configuration validates', call('POST', route + '/validate', {})['valid'])
        harness.request('POST', route + '/apply', {}, expected=202, headers={'If-Match': '"' + str(current['revision']) + '"'})
        parameters = {'user_openid': target['target']}
        operation('typing_seq2', 'post_v2_users_user_openid_messages', parameters,
            {'msg_type': 6, 'input_notify': {'input_type': 1, 'input_second': 15}, 'msg_id': target['message_id'], 'msg_seq': 2})
        markdown = operation('markdown_seq3', 'post_v2_users_user_openid_messages', parameters,
            {'msg_type': 2, 'markdown': {'content': '**合成接口验收**：这是一条 Markdown 测试消息。'}, 'msg_id': target['message_id'], 'msg_seq': 3})
        if markdown.get('platform_accepted'):
            identifier = remember('markdown', markdown, 3)
            print('QQ synthetic Markdown visible window: 8 seconds before owned-message retraction', flush=True)
            time.sleep(8)
            retract('markdown', identifier)
        first_text = '流式合成验收第一段'
        final_text = first_text + '，第二段已完成。'
        first = operation('stream_seq4_index0', 'post_v2_users_user_openid_stream_messages', parameters,
            stream_fragment(target['message_id'], 0, first_text))
        if first.get('platform_accepted'):
            stream_id = remember('stream', first, 4)
            check('stream replacement preserves the exact sent prefix', final_text.startswith(first_text))
            final = operation('stream_seq4_index1_end', 'post_v2_users_user_openid_stream_messages', parameters,
                stream_fragment(target['message_id'], 1, final_text, stream_id))
            if final.get('platform_accepted'):
                check('stream finish uses the same newly returned stream ID', final.get('response', {}).get('id') == stream_id)
                journal['messages']['stream']['generation_finished'] = True
                save()
                print('QQ synthetic stream visible window: 8 seconds before owned-message retraction', flush=True)
                time.sleep(8)
                retract('stream', stream_id)
            else:
                # A definite failed finish still leaves our known first fragment
                # visible; it is safe to retract that exact fresh message once.
                retract('stream', stream_id)
        report['status'] = 'failed_with_explicit_platform_results' if failed else 'platform_accepted_client_verification_pending'
    except Exception as error:
        failed = True
        report['status'] = 'blocked_by_unknown' if blocked else 'failed'
        report['failure'] = {'stage': stage, 'exception_type': type(error).__name__}
        if isinstance(error, ControlsApiFailure):
            report['failure'].update(local_http_status=error.http_status, local_error_code=error.error_code)
        print('FAIL ' + stage + ' (' + type(error).__name__ + ')', flush=True)
    finally:
        # No network calls in finally: after an uncertain request, a cleanup
        # attempt would violate the explicit stop boundary. Keep the journal.
        report['unknown_blocked_remaining_operations'] = blocked
        report['owned_message_cleanup_pending'] = any(not item.get('retracted') for item in journal['messages'].values())
        if harness:
            harness.close()
        if temporary:
            temporary.cleanup()
        key = None
        report['finished_at'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        save()
    return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
