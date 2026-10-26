#!/usr/bin/env python3
"""Isolated real-daemon commitment review, ownership and schedule evidence.

All data, users, model HTTP and activities are synthetic and disposable.
This neither accesses production configuration nor tests real QQ/Weixin,
contact permission, active sending or semantic fulfillment of a promise.
"""
import argparse
import hashlib
import http.server
import importlib.util
import json
import pathlib
import platform
import sys
import tempfile
import threading
import time
import uuid

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    'commitment_smoke_base', pathlib.Path(__file__).with_name('memory-smoke.py'))
base = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(base)
API = base.PREFIX


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report', required=True)
    args = parser.parse_args()
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    report = {'kind': 'isolated conversation commitment process acceptance', 'status': 'running',
              'started_at': base.utc_now(), 'commit': None, 'checks': [],
              'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'architecture': platform.machine(), 'production_data_accessed': False,
              'real_external_model_tested': False, 'real_qq_tested': False,
              'real_weixin_tested': False, 'recipient_permission_tested': False,
              'promise_fulfillment_tested': False, 'model_is_loopback_mock': True}
    mock = base.MockState()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), base.Provider)
    server.daemon_threads = True
    server.mock_state = mock
    threading.Thread(target=server.serve_forever, daemon=True).start()
    stage = 'isolated setup'

    def check(name, condition=True):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    def write_headers(key, revision=None):
        value = {'Idempotency-Key': key}
        if revision is not None:
            value['If-Match'] = '"' + str(revision) + '"'
        return value

    try:
        with tempfile.TemporaryDirectory(prefix='dsh-commitment-smoke-') as temporary:
            h = base.Harness(str(binary), str(web), temporary, mock)
            try:
                h.start()
                h.bootstrap()
                h.configure(server.server_port)
                report['commit'] = h.commit
                h.request('GET', API + '/commitments/scopes', authenticated=False, expected=401)
                chat = h.conversation('合成会话约定验收')
                run, _ = h.model_turn(chat, '请你阅读明天的资料')
                root = API + '/conversations/' + chat + '/commitments'
                query = '?run_id=' + run
                scope_page, _, _ = h.request('GET', API + '/commitments/scopes?limit=20')
                check('scope picker discovers an existing frozen source without raw message content',
                      any(item['conversation_id'] == chat and item['run_id'] == run
                          for item in scope_page['items'])
                      and '请你阅读明天的资料' not in json.dumps(scope_page, ensure_ascii=False))
                proposal = {'intent': '阅读已授权的合成资料', 'kind': 'reading', 'time_text': '明天'}
                headers = write_headers('create-reading')
                h.request('POST', root + query, proposal, headers=headers, with_csrf=False, expected=403)
                h.request('POST', root + query, proposal,
                          headers=dict(headers, Origin='https://invalid.example'), expected=403)
                h.request('POST', root + query, dict(proposal, owner_id='forged'), headers=headers, expected=422)
                candidate, _, _ = h.request('POST', root + query, proposal, headers=headers)
                repeated, _, _ = h.request('POST', root + query, proposal, headers=headers)
                check('candidate creation is authenticated, idempotent and cannot accept a client owner',
                      candidate['id'] == repeated['id'] and candidate['confirmation_state'] == 'proposed'
                      and candidate['subject_kind'] == 'assistant'
                      and candidate['counterparty_kind'] == 'speaker'
                      and candidate['execution']['state'] == 'unscheduled'
                      and candidate['time']['precision'] == 'unresolved')
                item_path = root + '/' + candidate['id']
                h.request('POST', item_path + '/confirm' + query, {'confirmed': True},
                          headers=write_headers('confirm-reading'), expected=428)
                h.request('POST', item_path + '/confirm' + query, {'confirmed': True},
                          headers=write_headers('confirm-reading', 99), expected=409)
                confirmed, _, _ = h.request('POST', item_path + '/confirm' + query, {'confirmed': True},
                                            headers=write_headers('confirm-reading', 1))
                check('human confirmation remains distinct from creating a real schedule or recipient permission',
                      confirmed['confirmation_state'] == 'confirmed'
                      and confirmed['execution']['state'] == 'unscheduled'
                      and confirmed['recipient_permission'] is False)
                h.request('POST', API + '/task-triggers/clock/acknowledge',
                          {'observed_now': int(time.time()), 'confirmed': True})
                resource, _, _ = h.request('POST', API + '/schedule/resources',
                    {'title': '合成授权资料', 'content': '只用于隔离验收的合成正文。', 'type': 'text'})
                now = int(time.time())
                activity, _, _ = h.request('POST', API + '/schedule/activities',
                    {'title': '合成未来阅读活动', 'type': 'reading', 'purpose': '验证持久调度证据',
                     'start_at': now + 3600, 'end_at': now + 3900, 'time_zone': 'Asia/Shanghai',
                     'resource_id': resource['id'], 'repeat': None, 'target_ref': None,
                     'movable': False, 'grant_id': None}, headers=write_headers(str(uuid.uuid4())))
                activities, _, _ = h.request('GET', API + '/conversations/' + chat
                                              + '/commitment-activities' + query)
                check('activity selector lists only actual activities owned by the verified source',
                      any(item['activity_id'] == activity['id'] and item['binding_supported']
                          for item in activities['items']))
                binding = {'activity_id': activity['id'], 'activity_revision': activity['revision']}
                scheduled, _, _ = h.request('POST', item_path + '/activity' + query,
                                            binding, headers=write_headers('bind-reading', 2))
                check('scheduled status follows a persisted authorized activity without claiming fulfillment or delivery',
                      scheduled['execution']['state'] == 'scheduled'
                      and scheduled['execution']['activity_id'] == activity['id']
                      and scheduled['execution']['fulfillment_verified'] is False
                      and scheduled['execution']['recipient_permission'] is False)
                h.request('POST', item_path + '/cancel' + query,
                          {'cancel_schedule': True, 'expected_activity_revision': 99},
                          headers=write_headers('stale-cancel', 3), expected=409)
                old_activity, _, _ = h.request('GET', API + '/schedule/activities/' + activity['id'])
                check('stale schedule cancellation cannot change the commitment or the actual activity',
                      old_activity['state'] == 'enabled'
                      and h.request('GET', item_path + query)[0]['confirmation_state'] == 'confirmed')
                cancelled, _, _ = h.request('POST', item_path + '/cancel' + query,
                    {'cancel_schedule': True, 'expected_activity_revision': activity['revision']},
                    headers=write_headers('cancel-real-activity', 3))
                actual, _, _ = h.request('GET', API + '/schedule/activities/' + activity['id'])
                check('cancellation is reported only after the real persistent schedule cancellation succeeds',
                      cancelled['confirmation_state'] == 'cancelled'
                      and cancelled['execution']['state'] == 'cancelled' and actual['state'] == 'cancelled')
                own_run, _ = h.model_turn(chat, '我答应你明天阅读资料')
                own_query = '?run_id=' + own_run
                personal, _, _ = h.request('POST', root + own_query, proposal,
                                           headers=write_headers('personal-reading'))
                check('a personal reading promise is represented with the speaker as subject',
                      personal['subject_kind'] == 'speaker' and personal['counterparty_kind'] == 'assistant'
                      and personal['execution']['state'] == 'unscheduled')
                personal_path = root + '/' + personal['id']
                h.request('POST', personal_path + '/participants' + own_query,
                    {'subject_kind': 'speaker', 'counterparty_kind': 'assistant', 'recipient_id': 'forged'},
                    headers=write_headers('no-recipient-id', 1), expected=422)
                reviewed, _, _ = h.request('POST', personal_path + '/participants' + own_query,
                    {'subject_kind': 'speaker', 'counterparty_kind': 'assistant'},
                    headers=write_headers('review-participants', 1))
                h.request('POST', personal_path + '/confirm' + own_query, {'confirmed': True},
                          headers=write_headers('confirm-personal', reviewed['revision']))
                h.request('POST', personal_path + '/activity' + own_query, binding,
                          headers=write_headers('not-assistant-task', reviewed['revision'] + 1), expected=422)
                check('participant review accepts only controlled roles and cannot turn a user promise into an assistant task',
                      h.request('GET', personal_path + own_query)[0]['execution']['state'] == 'unscheduled')
                h.stop()
                h.start()
                h.login()
                retained, _, _ = h.request('GET', item_path + query)
                check('restart preserves confirmation cancellation and source ownership without creating an extra activity',
                      retained['confirmation_state'] == 'cancelled'
                      and retained['source_run_id'] == run
                      and len(h.request('GET', API + '/schedule/activities')[0]['items']) == 1)
                with h.db() as db:
                    sends = db.execute('SELECT count(*) FROM outbox').fetchone()[0]
                    api_calls = db.execute('SELECT count(*) FROM qq_api_calls').fetchone()[0]
                check('isolated management never creates a channel send or QQ API request', sends == 0 and api_calls == 0)
                report['status'] = 'passed'
            finally:
                h.close()
    except Exception as error:
        report.update(status='failed', failed_phase=stage, error_type=type(error).__name__)
        print('FAIL isolated commitments; inspect report metadata', flush=True)
    finally:
        server.shutdown()
        server.server_close()
        report.update(finished_at=base.utc_now(), passed=len(report['checks']),
                      mock_model_requests=mock.count())
        output = pathlib.Path(args.report).resolve()
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    return 0 if report['status'] == 'passed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
