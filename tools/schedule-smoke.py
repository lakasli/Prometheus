#!/usr/bin/env python3
"""Isolated schedule API/process and optional real-model acceptance.

Only synthetic text and temporary accounts are used. --live-config-from reads
the authorized deployment's current provider configuration and key reference;
it copies no conversations, diaries, contacts, resources or channel settings.
Reports contain check metadata and hashes, never credentials or model content.
"""
import argparse
import hashlib
import http.server
import importlib.util
import json
import pathlib
import platform
import tempfile
import threading
import time
import uuid

spec = importlib.util.spec_from_file_location(
    'schedule_diary_helpers', pathlib.Path(__file__).with_name('diary-smoke.py'))
diary = importlib.util.module_from_spec(spec)
spec.loader.exec_module(diary)
memory = diary.memory
PREFIX = memory.PREFIX
ROOT = PREFIX + '/schedule'


class Provider(memory.Provider):
    def do_POST(self):
        size = int(self.headers.get('Content-Length', '0'))
        if not 0 < size <= 1024 * 1024:
            self.send_error(413)
            return
        payload = json.loads(self.rfile.read(size))
        with self.server.mock_state.condition:
            self.server.mock_state.requests.append(payload)
        system = payload['messages'][0]['content']
        envelope = json.loads(payload['messages'][-1]['content'])
        if '安排一个阅读活动' in system:
            selection = {'slot_id': envelope['candidate_slots'][0]['slot_id'],
                         'resource_id': envelope['authorized_resources'][0]['resource_id']}
            content = json.dumps({'selection': selection,
                                  'reason': '使用已有授权及空闲时间'}, ensure_ascii=False)
        else:
            content = '本片段是合成验收资料，说明春日读诗与按来源记录的重要性；覆盖范围只限本次给定字符区间。'
        raw = json.dumps({'choices': [{'message': {'content': content}}]}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(raw)))
        self.end_headers()
        try:
            self.wfile.write(raw)
        except (BrokenPipeError, ConnectionResetError):
            pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report', required=True)
    parser.add_argument('--live-config-from')
    args = parser.parse_args()
    binary = str(pathlib.Path(args.binary).resolve(strict=True))
    web = str(pathlib.Path(args.web_dir).resolve(strict=True))
    report = {'kind': 'isolated-schedule-process-api', 'status': 'running',
              'started_at': memory.utc_now(), 'platform': platform.system(),
              'architecture': platform.machine(),
              'binary_sha256': hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest(),
              'external_model_requested': bool(args.live_config_from),
              'external_reading_passed': False, 'external_planning_passed': False,
              'production_data_modified': False, 'real_channel_tested': False,
              'checks': []}
    mock = memory.MockState()
    provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    provider.daemon_threads = True
    provider.mock_state = mock
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    failed = False

    def check(name, condition):
        if not condition:
            report['failed_check'] = name
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    try:
        with tempfile.TemporaryDirectory(prefix='dsh-schedule-acceptance-') as temporary:
            h = memory.Harness(binary, web, temporary, mock)
            try:
                h.start()
                report['commit'] = h.commit
                now = int(time.time())
                query = '?start=' + str(now - 86400) + '&end=' + str(now + 7 * 86400)
                for path in ['/calendar' + query, '/activities', '/resources',
                             '/autonomy-grants', '/plans', '/capabilities']:
                    h.request('GET', ROOT + path, expected=401)
                check('all schedule reads require authentication', True)
                h.bootstrap()
                h.request('POST', ROOT + '/resources', {'title': 'synthetic'},
                          expected=403, with_csrf=False)
                h.request('POST', ROOT + '/resources', {'title': 'synthetic'},
                          expected=403, headers={'Origin': 'https://invalid.example'})
                check('schedule writes require CSRF and exact origin', True)
                if args.live_config_from:
                    diary.configure_live(h, args.live_config_from)
                else:
                    h.configure(provider.server_port)
                h.request('POST', PREFIX + '/task-triggers/clock/acknowledge',
                          {'observed_now': int(time.time()), 'confirmed': True})
                text = ('合成验收资料：春天适合读诗，记录观点必须保留原始来源。' * 200)[:5000]
                resource, _, _ = h.request('POST', ROOT + '/resources', {
                    'title': '合成私人书架资料', 'type': 'text', 'content': text})
                rid = resource['id']
                check('authorized text is private, immutable and content addressed',
                      resource['visibility'] == 'private' and resource['characters'] == len(text)
                      and resource['content_hash'] == hashlib.sha256(text.encode()).hexdigest())
                now = int(time.time())
                activity = {'title': 'Synthetic private reading marker', 'type': 'reading',
                            'purpose': '按当前文本片段记录简短笔记，不编造其他来源。',
                            'start_at': now + 2, 'end_at': now + 242,
                            'time_zone': 'Asia/Shanghai', 'resource_id': rid,
                            'target_ref': None, 'repeat': None, 'movable': True,
                            'grant_id': None}
                key = str(uuid.uuid4())
                created, _, _ = h.request('POST', ROOT + '/activities', activity,
                                         headers={'Idempotency-Key': key})
                same, _, _ = h.request('POST', ROOT + '/activities', activity,
                                      headers={'Idempotency-Key': key})
                h.request('POST', ROOT + '/activities', dict(activity, title='changed'),
                          headers={'Idempotency-Key': key}, expected=409)
                check('activity creation is idempotent and mismatched replay conflicts',
                      created['id'] == same['id'])
                h.request('PATCH', ROOT + '/activities/' + created['id'],
                          {'expected_revision': 2, 'state': 'paused'}, expected=409)
                check('stale activity revisions cannot override the schedule', True)

                # A second synthetic admin is inserted only in the temporary
                # fixture; no production accounts or data are touched.
                original_username = h.username
                with h.db() as db:
                    db.execute("INSERT INTO admins(id,username,password_hash,created_at) SELECT 'schedule-other-owner',?,password_hash,? FROM admins WHERE id=?",
                               ('schedule-other-test', now, h.owner))
                h.username = 'schedule-other-test'
                h.login()
                h.request('GET', ROOT + '/activities/' + created['id'], expected=404)
                other_calendar, raw, _ = h.request('GET', ROOT + '/calendar' + query)
                check('another owner cannot see private title, activity or resource',
                      not other_calendar['items'] and b'Synthetic private reading marker' not in raw
                      and not h.request('GET', ROOT + '/resources')[0]['items'])
                h.username = original_username
                h.login()
                deadline = time.monotonic() + 180
                finished = None
                while time.monotonic() < deadline:
                    current, _, _ = h.request('GET', ROOT + '/activities/' + created['id'])
                    if current['occurrences'] and current['occurrences'][0]['status'] == 'completed':
                        finished = current['occurrences'][0]
                        break
                    time.sleep(.3)
                check('model notes advance exact real text coverage across multiple chunks',
                      finished is not None and finished['processed_chars'] == len(text)
                      and finished['resource_hash'] == resource['content_hash']
                      and len(finished['result']['fragments']) >= 2
                      and all(item['note'] and item['provider'] for item in finished['result']['fragments']))
                report['external_reading_passed'] = bool(args.live_config_from)
                h.stop()
                h.start()
                h.login()
                h.request('POST', PREFIX + '/task-triggers/clock/acknowledge',
                          {'observed_now': int(time.time()), 'confirmed': True})
                after, _, _ = h.request('GET', ROOT + '/activities/' + created['id'])
                check('completed reading checkpoint and notes survive daemon restart',
                      after['occurrences'][0]['processed_chars'] == len(text)
                      and after['occurrences'][0]['status'] == 'completed')

                now = int(time.time())
                planning_resource, _, _ = h.request('POST', ROOT + '/resources', {
                    'title': '合成未读规划资料', 'type': 'text',
                    'content': '这是尚未阅读的合成资料，用来验证模型只能在授权书架中安排下一次阅读。'})
                planning_rid = planning_resource['id']
                policy = {'title': 'Synthetic bounded reading authorization',
                          'allowed_types': ['reading'], 'resource_ids': [planning_rid],
                          'time_zone': 'Asia/Shanghai', 'valid_from': now - 60,
                          'valid_until': now + 7 * 86400, 'horizon_days': 7,
                          'window_start': '00:00', 'window_end': '23:59',
                          'max_daily_activities': 8, 'max_daily_minutes': 120,
                          'max_daily_changes': 20, 'max_daily_model_calls': 16,
                          'max_daily_tokens': 120000, 'max_output_tokens': 1200}
                grant, _, _ = h.request('POST', ROOT + '/autonomy-grants', policy)
                plan, _, _ = h.request('POST', ROOT + '/plans', {
                    'goal': '阅读授权合成资料，留出已有忙闲时间。', 'grant_id': grant['id'],
                    'resource_ids': [planning_rid], 'duration_minutes': 10,
                    'state': 'enabled', 'planning_mode': 'model'})
                current_config, _, _ = h.request('GET', PREFIX + '/config')
                deadline = time.monotonic() + max(180, current_config['config']['assistant']['timeout_seconds'] + 30)
                planned = None
                while time.monotonic() < deadline:
                    candidates, _, _ = h.request('GET', ROOT + '/activities')
                    planned = next((v for v in candidates['items'] if v['creator'] == 'autonomous'
                                    and v['state'] == 'enabled'), None)
                    if planned:
                        break
                    plan_status, _, _ = h.request('GET', ROOT + '/plans')
                    observed = next((v for v in plan_status['items'] if v['id'] == plan['id']), {})
                    # Preserve only safe metadata. The definition contains the
                    # private goal and must never enter the report.
                    report['planning_diagnostics'] = {k: observed.get(k) for k in (
                        'state', 'revision', 'block_reason', 'last_planned_at', 'planning_diagnostics')}
                    attempt = observed.get('planning_diagnostics', {}).get('last_attempt') or {}
                    if attempt.get('status') in {'failed', 'rejected', 'blocked', 'cancelled', 'interrupted', 'declined'}:
                        break
                    time.sleep(.3)
                plan_status, _, _ = h.request('GET', ROOT + '/plans')
                observed = next((v for v in plan_status['items'] if v['id'] == plan['id']), {})
                report['planning_diagnostics'] = {k: observed.get(k) for k in (
                    'state', 'revision', 'block_reason', 'last_planned_at', 'planning_diagnostics')}
                check('model autonomously schedules only within an existing bounded grant',
                      planned is not None and planned['definition']['resource_id'] == planning_rid
                      and planned['definition']['grant_id'] == grant['id']
                      and planned['definition']['type'] == 'reading')
                report['external_planning_passed'] = bool(args.live_config_from)
                h.request('PATCH', ROOT + '/plans/' + plan['id'],
                          {'expected_revision': 1, 'state': 'paused'})
                h.request('POST', ROOT + '/activities/' + planned['id'] + '/pause',
                          {'expected_revision': planned['revision']})
                h.request('POST', ROOT + '/autonomy-grants/' + grant['id'] + '/revoke',
                          {'expected_revision': 1})
                changed_policy = dict(policy, title='revoked scope marker')
                h.request('PATCH', ROOT + '/autonomy-grants/' + grant['id'],
                          {'expected_revision': 1, 'policy': changed_policy}, expected=409)
                check('autonomy revocation is versioned and rejects stale policy writes', True)
                future = dict(activity, type='video', resource_id=None,
                              start_at=int(time.time()) + 600, end_at=int(time.time()) + 1200)
                preview, _, _ = h.request('POST', ROOT + '/activities/preview', future)
                check('video and active messaging remain blocked without verified capabilities',
                      preview['capability_block'] == 'video_provider_unavailable'
                      and h.request('GET', ROOT + '/capabilities')[0]['proactive_chat'] is False)
                pending_video, _, _ = h.request('POST', ROOT + '/proposals',
                    dict(future, grant_id=grant['id']), headers={'Idempotency-Key': str(uuid.uuid4())})
                check('out of scope proposal remains pending without self approval',
                      pending_video['state'] == 'pending_approval')
                approve_path = ROOT + '/activities/' + pending_video['id'] + '/approve'
                h.request('POST', approve_path, {'confirmed': True, 'scope': 'series'}, expected=400)
                h.request('POST', approve_path, {'confirmed': False, 'scope': 'series'},
                          headers={'If-Match': '1'}, expected=422)
                h.request('POST', approve_path, {'confirmed': True, 'scope': 'series'},
                          headers={'If-Match': '2'}, expected=409)
                approved, _, _ = h.request('POST', approve_path, {'confirmed': True, 'scope': 'series'},
                                          headers={'If-Match': '1'})
                check('explicit exact revision series approval preserves AI origin and cannot enable video',
                      approved['state'] == 'enabled' and approved['creator'] == 'autonomous'
                      and approved['definition']['grant_id'] is None
                      and approved['admin_approval']['approved_revision'] == 2
                      and h.request('POST', ROOT + '/activities/' + approved['id'] + '/preview')[0]['capability_block']
                      == 'video_provider_unavailable')
                h.request('POST', ROOT + '/resources/' + rid + '/revoke',
                          {'expected_revision': 1})
                forgotten, _, _ = h.request('GET', ROOT + '/activities/' + created['id'])
                check('source revocation clears derived reading notes and blocks future reads',
                      forgotten['occurrences'][0]['result'] is None
                      and forgotten['occurrences'][0]['block_reason'] == 'resource_revoked')
                report['status'] = 'passed'
            finally:
                h.close()
    except Exception as error:
        failed = True
        report['status'] = 'failed'
        report['error_type'] = type(error).__name__
        report.setdefault('failed_check', 'unexpected_failure_after_' + str(len(report['checks'])) + '_checks')
        trace = error.__traceback__
        while trace:
            if trace.tb_frame.f_code.co_filename == __file__:
                report['failure_line'] = trace.tb_lineno
            trace = trace.tb_next
        print('FAIL isolated schedule acceptance; see sanitized report', flush=True)
    finally:
        provider.shutdown()
        provider.server_close()
        report['finished_at'] = memory.utc_now()
        report['passed'] = len(report['checks'])
        report['synthetic_provider_requests'] = len(mock.requests)
        pathlib.Path(args.report).write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
    return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
