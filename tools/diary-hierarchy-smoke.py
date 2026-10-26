#!/usr/bin/env python3
"""Isolated 310-event monthly hierarchy acceptance, with mock or live provider.

All source material/accounts/databases are synthetic. --live-config-from reads
only the authorized deployment's current model configuration and key; production
diaries and channels are neither copied nor changed. Reports omit all prose,
provider outputs and credentials.
"""
import argparse
import calendar
import datetime
import hashlib
import http.server
import importlib.util
import json
import pathlib
import platform
import tempfile
import threading
import time

spec = importlib.util.spec_from_file_location(
    'diary_smoke', pathlib.Path(__file__).with_name('diary-smoke.py'))
diary = importlib.util.module_from_spec(spec)
spec.loader.exec_module(diary)
memory = diary.memory
PREFIX = memory.PREFIX


def month_31():
    today = datetime.datetime.now(datetime.timezone(datetime.timedelta(hours=8))).date()
    year, month = today.year, today.month
    while True:
        month -= 1
        if month == 0:
            month = 12
            year -= 1
        if calendar.monthrange(year, month)[1] == 31:
            return year, month


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report')
    parser.add_argument('--live-config-from')
    args = parser.parse_args()
    binary = str(pathlib.Path(args.binary).resolve(strict=True))
    web_dir = str(pathlib.Path(args.web_dir).resolve(strict=True))
    report = {'kind': 'isolated-310-event-month-hierarchy', 'status': 'running',
              'started_at': memory.utc_now(), 'platform': platform.system(),
              'architecture': platform.machine(), 'production_data_modified': False,
              'external_model_requested': bool(args.live_config_from),
              'external_model_passed': False, 'real_channel_tested': False,
              'binary_sha256': hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest(),
              'checks': []}
    mock = memory.MockState()
    provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), diary.Provider)
    provider.daemon_threads = True
    provider.mock_state = mock
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    stage = 'setup'

    def check(name, condition):
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    failed = False
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-diary-hierarchy-') as temporary:
            h = memory.Harness(binary, web_dir, temporary, mock)
            try:
                h.start()
                h.bootstrap()
                report['commit'] = h.commit
                if args.live_config_from:
                    diary.configure_live(h, args.live_config_from)
                else:
                    h.configure(provider.server_port)
                conversation = h.conversation('Synthetic monthly diary hierarchy')
                policy = {'source_owner_id': h.owner, 'enabled': True,
                          'allow_model': False, 'timezone': 'Asia/Shanghai'}
                h.request('PUT', PREFIX + '/diary/policies', policy, headers={'If-Match': '0'})
                year, month = month_31()
                days, origins = {}, []
                total_parent_bytes = 0
                stage = 'populate-31-sealed-daily-parents'
                for day in range(1, 32):
                    date = f'{year:04}-{month:02}-{day:02}'
                    happened = int(datetime.datetime(year, month, day, 12,
                        tzinfo=datetime.timezone(datetime.timedelta(hours=8))).timestamp())
                    for index in range(10):
                        # Short distinct events; their complete daily bodies fit
                        # 12 KB, while 31 whole parents require multiple requests.
                        content = f'合成第{day}天第{index}件经历：' + (
                            '今天读了一小段描写山川的文字，记下了作者视角和时间顺序的问题。' * 3)
                        event, _, _ = h.request('POST', PREFIX + '/diary/events', {
                            'content': content, 'source_conversation_id': conversation,
                            'happened_at': happened, 'visibility': 'private'}, expected=201)
                        origins.append(event['id'])
                    entry, _, _ = h.request('POST', PREFIX + '/diary/consolidate', {
                        'period': 'day', 'day': date, 'timezone': 'Asia/Shanghai',
                        'visibility': 'private', 'source_owner_id': h.owner})
                    if entry['source_count'] != 10 or '摘录达到正文预算' in entry['facts']:
                        raise AssertionError('daily parent was not complete')
                    days[day] = entry
                    total_parent_bytes += len((entry['facts'] + entry['feelings']).encode())
                check('31 sealed daily parents cover all 310 events without text truncation', len(origins) == 310)
                check('all complete daily parent text exceeds one 48 KB request', total_parent_bytes > 48000)
                with h.db() as db:
                    queued = db.execute('SELECT count(*) FROM diary_model_jobs').fetchone()[0]
                    # All historical parents were explicitly sealed through the
                    # real API. Advance only this temporary fixture's automatic
                    # cursor so the live-provider test pays for the requested
                    # month reduction, not unrelated historical weekly jobs.
                    yesterday = (datetime.datetime.now(datetime.timezone(datetime.timedelta(hours=8))).date()
                                 - datetime.timedelta(days=1)).isoformat()
                    db.execute('UPDATE diary_scope_checkpoints SET last_date=? WHERE source_owner_id=?',
                               (yesterday, h.owner))
                check('model-disabled daily construction creates no 31-job model backlog', queued == 0)
                stage = 'bounded-month-model-reduction'
                policy['allow_model'] = True
                h.request('PUT', PREFIX + '/diary/policies', policy, headers={'If-Match': '1'})
                month_entry, _, _ = h.request('POST', PREFIX + '/diary/consolidate', {
                    'period': 'month', 'day': f'{year:04}-{month:02}-01',
                    'timezone': 'Asia/Shanghai', 'visibility': 'private',
                    'source_owner_id': h.owner, 'retry_failed': True})
                h.request('POST', PREFIX + '/task-triggers/clock/acknowledge',
                          {'observed_now': int(time.time()), 'confirmed': True})
                deadline = time.monotonic() + 300
                generated = None
                while time.monotonic() < deadline:
                    candidate, _, _ = h.request('GET', PREFIX + '/diary/entries/' + month_entry['id'])
                    if candidate['generator'].startswith('model:'):
                        generated = candidate
                        break
                    with h.db() as db:
                        job = db.execute('SELECT status,error FROM diary_model_jobs WHERE entry_id=?',
                                         (month_entry['id'],)).fetchone()
                    if job and job['status'] in ('blocked', 'failed'):
                        raise AssertionError('month model job failed')
                    time.sleep(.15)
                check('month really completes model aggregation of all 310 sources',
                      generated is not None and generated['source_count'] == 310)
                with h.db() as db:
                    parts = db.execute('SELECT count(*) FROM diary_model_fragments WHERE entry_id=?',
                                       (month_entry['id'],)).fetchone()[0]
                    requests = [dict(row) for row in db.execute(
                        'SELECT input_bytes,outcome FROM diary_model_leases ORDER BY started_at,id')]
                report['model_requests'] = len(requests)
                report['fragment_count'] = parts
                report['maximum_request_input_bytes'] = max(row['input_bytes'] or 0 for row in requests)
                report['raw_source_count'] = generated['source_count']
                report['parent_count'] = 31
                report['total_parent_text_bytes'] = total_parent_bytes
                check('at least two persisted fragments precede final merge within eight requests',
                      parts >= 2 and 3 <= len(requests) <= 8)
                check('every serialized provider request is within 48000 bytes and completed',
                      all(row['input_bytes'] and row['input_bytes'] <= 48000 and row['outcome'] == 'completed'
                          for row in requests))
                if not args.live_config_from:
                    check('mock HTTP server observed exactly the persisted request count', mock.count() == len(requests))
                report['external_model_passed'] = bool(args.live_config_from)
                stage = 'paged-source-coverage'
                seen = set()
                offset = 0
                while True:
                    page, _, _ = h.request('GET', PREFIX + '/diary/entries/' + month_entry['id']
                                          + f'/sources?limit=100&offset={offset}')
                    if page['total'] != 310 or len(page['items']) > 100:
                        raise AssertionError('source page misreported coverage')
                    seen.update(event['id'] for event in page['items'])
                    if not page['has_more']:
                        break
                    offset += page['limit']
                check('bounded source pages preserve the complete exact 310-origin chain', seen == set(origins))
                # Pause further model work, then exercise the overlapping week
                # as a deterministic parent catalogue without spending a call.
                policy['allow_model'] = False
                h.request('PUT', PREFIX + '/diary/policies', policy, headers={'If-Match': '2'})
                week, _, _ = h.request('POST', PREFIX + '/diary/consolidate', {
                    'period': 'week', 'day': f'{year:04}-{month:02}-20',
                    'timezone': 'Asia/Shanghai', 'visibility': 'private',
                    'source_owner_id': h.owner})
                stage = 'forget-and-restart-fences'
                forgotten = origins[190]
                h.request('DELETE', PREFIX + '/diary/events/' + forgotten, headers={'If-Match': '1'})
                for entry_id in (days[20]['id'], week['id'], month_entry['id']):
                    h.request('GET', PREFIX + '/diary/entries/' + entry_id, expected=404)
                with h.db() as db:
                    remaining = db.execute('SELECT count(*) FROM diary_model_fragments WHERE entry_id=?',
                                           (month_entry['id'],)).fetchone()[0]
                check('forget invalidates day week month and clears all derived fragment prose', remaining == 0)
                h.stop()
                h.start()
                h.login()
                h.request('GET', PREFIX + '/diary/events/' + forgotten, expected=404)
                h.request('GET', PREFIX + '/diary/entries/' + month_entry['id'], expected=404)
                with h.db() as db:
                    remaining = db.execute('SELECT count(*) FROM diary_model_fragments WHERE entry_id=?',
                                           (month_entry['id'],)).fetchone()[0]
                check('restart never resurrects forgotten source or stale model fragments', remaining == 0)
                report['status'] = 'passed'
            finally:
                h.close()
    except Exception as error:
        failed = True
        report.update(status='failed', failure_stage=stage, error_type=type(error).__name__)
        print('FAIL ' + stage + ' (' + type(error).__name__ + ')', flush=True)
    finally:
        provider.shutdown()
        report['finished_at'] = memory.utc_now()
        if args.report:
            pathlib.Path(args.report).write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
    return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
