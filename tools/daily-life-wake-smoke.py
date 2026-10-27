#!/usr/bin/env python3
"""Real civil-minute wake and diary boundary in a disposable Linux daemon.

Uses a loopback provider, real host clock and published binary. No production
configuration, credentials, conversations or channel accounts are accessed.
"""
import argparse
import datetime
import importlib.util
import json
import pathlib
import platform
import tempfile
import threading
import time
import http.server

spec = importlib.util.spec_from_file_location('daily_life_acceptance', pathlib.Path(__file__).with_name('daily-life-smoke.py'))
life = importlib.util.module_from_spec(spec)
spec.loader.exec_module(life)
base, API = life.base, life.API


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--expected-commit', required=True)
    parser.add_argument('--report', required=True)
    args = parser.parse_args()
    report = {'kind': 'isolated real-clock sleeping wake acceptance', 'status': 'running',
              'started_at': base.utc_now(), 'architecture': platform.machine(), 'checks': [],
              'production_accessed': False, 'external_model': False, 'real_channel_tested': False,
              'synthetic_clock_or_event_injected': False}
    destination = pathlib.Path(args.report).resolve()

    def check(name, condition=True):
        report['last_check'] = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    mock = base.MockState()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), life.Provider)
    server.daemon_threads = True
    server.mock_state = mock
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-real-wake-') as temporary:
            h = base.Harness(str(pathlib.Path(args.binary).resolve()), str(pathlib.Path(args.web_dir).resolve()), temporary, mock)
            try:
                h.start()
                report['commit'] = h.commit
                check('temporary daemon matches the published binary', h.commit == args.expected_commit)
                h.bootstrap()
                h.configure(server.server_port)
                h.request('POST', API + '/task-triggers/clock/acknowledge', {'observed_now': int(time.time()), 'confirmed': True})
                h.request('PUT', API + '/diary/policies', {'source_owner_id': h.owner, 'enabled': True, 'allow_model': False,
                                                        'timezone': 'Asia/Shanghai'}, headers={'If-Match': '0'})
                conversation = h.conversation('synthetic sleep diary')
                before = h.submit(conversation, '请记下今天的合成编号：SLEEP_DIARY_BOUNDARY_694。')
                check('pre-sleep interaction completes', h.wait_run(before)['status'] == 'completed')
                due = ((int(time.time()) + 149) // 60) * 60
                local = datetime.datetime.fromtimestamp(due, datetime.timezone(datetime.timedelta(hours=8)))
                old = h.request('GET', API + '/rest/policy')[0]
                policy = dict(old['policy'], schedule_enabled=True, anticipation_enabled=True, reset_context_on_sleep=True,
                              sleep_local=(local - datetime.timedelta(hours=12)).strftime('%H:%M'), wake_local=local.strftime('%H:%M'))
                h.request('PUT', API + '/rest/policy', policy,
                          headers={'If-Match': f'"{old["revision"]}"', 'Idempotency-Key': 'real-wake-policy'})
                state = h.request('GET', API + '/rest')[0]['state']
                h.request('POST', API + '/rest/sleep', {'wake_at': due},
                          headers={'If-Match': f'"{state["generation"]}"', 'Idempotency-Key': 'real-wake-sleep'})
                life.wait(lambda: h.request('GET', API + '/rest')[0]['state']['phase'] == 'sleeping', 40)
                check('actual daemon enters sleeping before the future civil wake')

                def notice():
                    items = h.request('GET', API + '/schedule/decisions')[0]['items']
                    return next((item for item in items if item['kind'] == 'wake' and item['nominal_at'] == due
                                 and item['decision_status'] in ('completed', 'failed')), None)

                decision = life.wait(notice, 160)
                check('sleeping instance can receive the one-minute model decision',
                      decision['decision_status'] == 'completed' and decision['decision'] == 'execute')
                check('notice uses the real one-minute window', 0 < due - decision['notified_at'] <= 60)
                with mock.condition:
                    requests = list(mock.requests)
                envelope = next(json.loads(request['messages'][-1]['content']) for request in requests
                                if any('DSH_SCHEDULE_NOTICE_V1' in message['content'] for message in request['messages']))
                check('actual provider receives the sleeping state for this exact wake', envelope['phase'] == 'sleeping' and envelope['due_at'] == due)
                life.wait(lambda: h.request('GET', API + '/rest')[0]['state']['phase'] == 'awake', max(15, due - int(time.time()) + 15))
                check('fixed wake executes without another model request', int(time.time()) >= due)
                after = h.submit(conversation, '醒来后从日记查看今天的合成编号。')
                check('post-wake conversation completes', h.wait_run(after)['status'] == 'completed')
                with h.db() as db:
                    cut = db.execute('SELECT through_run_rowid FROM sleep_context_boundaries WHERE conversation_id=?', (conversation,)).fetchone()[0]
                    old_order = db.execute('SELECT rowid FROM runs WHERE id=?', (before,)).fetchone()[0]
                    messages = json.loads(db.execute('SELECT messages_json FROM run_prompt_snapshots WHERE run_id=?', (after,)).fetchone()[0])
                    captured = db.execute("SELECT count(*) FROM diary_events WHERE content LIKE '%SLEEP_DIARY_BOUNDARY_694%' AND deleted_at IS NULL").fetchone()[0]
                    retained = db.execute('SELECT count(*) FROM messages WHERE run_id=?', (before,)).fetchone()[0]
                    report['diary_checkpoint'] = {
                        'captured_events': captured,
                        'recallable_entries': db.execute("SELECT count(*) FROM diary_entries WHERE facts LIKE '%SLEEP_DIARY_BOUNDARY_694%' AND deleted_at IS NULL").fetchone()[0],
                        'model_jobs': db.execute('SELECT count(*) FROM diary_model_jobs').fetchone()[0],
                    }
                check('sleep boundary excludes previous raw assistant turns from the real next request', cut >= old_order and not any(m['role'] == 'assistant' for m in messages))
                check('original messages and captured diary evidence remain', retained > 0 and captured > 0)
                check('post-wake request recalls the recorded fact through diary evidence',
                      any('SLEEP_DIARY_BOUNDARY_694' in message['content'] and 'DSH_DIARY' in message['content'] for message in messages))
                report.update(status='passed', passed=len(report['checks']), wake_at=due,
                              notified_at=decision['notified_at'], notice_phase=envelope['phase'])
            finally:
                h.close()
    except Exception as error:
        report.update(status='failed', error_type=type(error).__name__)
        raise
    finally:
        report['finished_at'] = base.utc_now()
        destination.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        server.shutdown()
        server.server_close()


if __name__ == '__main__':
    main()
