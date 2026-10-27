#!/usr/bin/env python3
"""Daily-life acceptance in a disposable daemon; no real channel is enabled.

Optional live configuration reads only the deployed model profile/key. Test
inputs, diaries, accounts and schedule are synthetic and remain isolated.
"""
import argparse
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

spec = importlib.util.spec_from_file_location('life_schedule', pathlib.Path(__file__).with_name('schedule-smoke.py'))
schedule = importlib.util.module_from_spec(spec)
spec.loader.exec_module(schedule)
base, diary, API = schedule.memory, schedule.diary, schedule.PREFIX


class Provider(base.Provider):
    def do_POST(self):
        payload = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        with self.server.mock_state.condition:
            self.server.mock_state.requests.append(payload)
        systems = '\n'.join(m['content'] for m in payload['messages'] if m['role'] == 'system')
        if 'DSH_SCHEDULE_NOTICE_V1' in systems:
            result = {'decision': 'execute', 'delay_minutes': 0}
        else:
            offer = next((json.loads(line)['available'] for line in systems.splitlines()
                          if line.startswith('{"actual_activities":') or line.startswith('{"available":')), None)
            if offer and offer['slots']:
                result = {'reply': [{'type': 'text', 'text': '我去抄首《春晓》。', 'delay_ms': 0}],
                          'self_activity': {'poem_id': 'chunxiao', 'slot_id': offer['slots'][0]['slot_id']}}
            else:
                result = [{'type': 'text', 'text': '今天想安安静静读一首诗。', 'delay_ms': 0}]
        raw = json.dumps({'choices': [{'message': {'content': json.dumps(result, ensure_ascii=False)},
                                     'finish_reason': 'stop'}]}, ensure_ascii=False).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)


def wait(predicate, timeout=150):
    until = time.monotonic() + timeout
    while time.monotonic() < until:
        result = predicate()
        if result:
            return result
        time.sleep(.2)
    raise TimeoutError('bounded isolated acceptance timed out')


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', required=True)
    p.add_argument('--web-dir', required=True)
    p.add_argument('--report', required=True)
    p.add_argument('--live-config-from')
    args = p.parse_args()
    report = {'kind': 'isolated daily-life acceptance', 'status': 'running',
              'started_at': base.utc_now(), 'architecture': platform.machine(),
              'external_model': bool(args.live_config_from), 'production_modified': False,
              'real_qq_tested': False, 'real_weixin_tested': False, 'checks': []}
    output = pathlib.Path(args.report).resolve()
    output.parent.mkdir(parents=True, exist_ok=True)

    def check(name, condition=True):
        report['last_check'] = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    mock = base.MockState()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    server.daemon_threads = True
    server.mock_state = mock
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-daily-life-') as temporary:
            h = base.Harness(str(pathlib.Path(args.binary).resolve()), str(pathlib.Path(args.web_dir).resolve()), temporary, mock)
            try:
                h.start()
                report['commit'] = h.commit
                for path in ['/schedule/personal-policy', '/schedule/decisions', '/rest/policy']:
                    h.request('GET', API + path, expected=401)
                check('new schedule reads require authentication')
                h.bootstrap()
                if args.live_config_from:
                    diary.configure_live(h, args.live_config_from)
                else:
                    h.configure(server.server_port)
                h.request('POST', API + '/task-triggers/clock/acknowledge', {'observed_now': int(time.time()), 'confirmed': True})
                h.request('PUT', API + '/schedule/personal-policy', {'enabled': True, 'daily_limit': 4},
                          expected=403, with_csrf=False, headers={'If-Match': '0'})
                personal = h.request('PUT', API + '/schedule/personal-policy', {'enabled': True, 'daily_limit': 4}, headers={'If-Match': '0'})[0]
                h.request('PUT', API + '/schedule/personal-policy', {'enabled': True, 'daily_limit': 4}, expected=409, headers={'If-Match': '0'})
                check('personal schedule grants require CSRF and reject stale versions', personal['enabled'])
                route = API + '/assistant/personality'
                personality = json.loads((pathlib.Path(__file__).resolve().parents[1] / 'docs/examples/liu-jiahui-personality-v2.json').read_text(encoding='utf-8'))
                current = h.request('GET', route)[0]
                h.request('PUT', route, {'personality': personality}, headers={'If-Match': current['etag']})
                conversation = h.conversation('synthetic own activities')
                run = h.submit(conversation, '你可以自己安排接下来空闲十分钟，抄一首目录里的古诗，并把小安排记到日程。')
                def terminal():
                    result = h.request('GET', API + '/runs/' + run)[0]
                    return result if result['status'] in base.TERMINAL else None
                result = wait(terminal)
                check('ordinary native conversation completes with the schedule protocol', result['status'] == 'completed')
                activities = h.request('GET', API + '/schedule/activities')[0]['items']
                # All inputs here are synthetic; retain enough evidence to
                # diagnose a protocol/model failure after the fixture closes.
                report['synthetic_plan_answer'] = result.get('response', '')
                with h.db() as db:
                    offered = db.execute('SELECT offer_json FROM daily_life_offers WHERE run_id=?', (run,)).fetchone()
                    report['offered_slot_count'] = len(json.loads(offered[0])['slots']) if offered else 0
                    report['activity_count'] = len(activities)
                    report['spoken_recovery_count'] = db.execute(
                        "SELECT count(*) FROM audit_events WHERE action='schedule.personal.reply_recovered' AND resource=?", (run,)).fetchone()[0]
                check('spoken own plan is persisted as an enabled autonomous calendar activity',
                      len(activities) == 1 and activities[0]['creator'] == 'autonomous' and activities[0]['state'] == 'enabled')
                aid = activities[0]['id']
                with h.db() as db:
                    messages = json.loads(db.execute('SELECT messages_json FROM run_prompt_snapshots WHERE run_id=?', (run,)).fetchone()[0])
                systems = '\n'.join(m['content'] for m in messages if m['role'] == 'system')
                check('actual provider system prompt uses the human perspective without mechanical identity words',
                      '你是刘嘉卉' in systems and all(word not in systems for word in ('AI助手', '智能体', '人工智能')))
                if args.live_config_from:
                    family = h.submit(h.conversation('synthetic family'), '在咱们约定的家庭背景里，咱爸叫林远山，咱妈叫周晴。自然接一句就好。')
                    family_result = wait(lambda: (v if (v := h.request('GET', API + '/runs/' + family)[0])['status'] in base.TERMINAL else None))
                    answer = family_result.get('response', '')
                    check('real model accepts explicitly supplied family background without a technical disclaimer',
                          family_result['status'] == 'completed' and not any(w in answer for w in ('AI', '智能体', '不是人类', '不能当成真的', '不能把')))
                    report['synthetic_family_answer'] = answer
                # Fixture-only time compression: no production clock or record is changed.
                due = int(time.time()) + 3
                with h.db() as db:
                    row = db.execute('SELECT definition_json FROM schedule_activities WHERE id=?', (aid,)).fetchone()
                    definition = json.loads(row[0])
                    definition.update(start_at=due, end_at=due + 600)
                    db.execute('UPDATE schedule_activities SET definition_json=? WHERE id=?', (json.dumps(definition, ensure_ascii=False), aid))
                detail = wait(lambda: (v if (v := h.request('GET', API + '/schedule/activities/' + aid)[0])['occurrences'] and v['occurrences'][0]['status'] == 'completed' else None), 65)
                saved = detail['occurrences'][0]['result']
                check('scheduled electronic poetry copy leaves actual complete text and a content hash',
                      saved['method'] == 'electronic_poetry_copy' and saved['sha256'] == hashlib.sha256(saved['text'].encode()).hexdigest())
                events = h.request('GET', API + '/diary/events')[0]['items']
                check('completed poetry is also recorded in the diary with execution provenance',
                      any(e['origin_type'] == 'schedule_activity' for e in events))
                policy = h.request('GET', API + '/rest/policy')[0]
                body = dict(policy['policy'], schedule_enabled=True, sleep_local='23:00', wake_local='07:00',
                            anticipation_enabled=True, reset_context_on_sleep=True)
                policy = h.request('PUT', API + '/rest/policy', body, headers={'If-Match': f'"{policy["revision"]}"', 'Idempotency-Key': 'synthetic-routine'})[0]
                rev = policy['revision']
                due = int(time.time()) + 55
                # Seed one isolated notice at its one-minute boundary. The
                # actual scheduler, provider, Python script and fenced commit run.
                with h.db() as db:
                    db.execute("INSERT INTO daily_life_events(id,kind,policy_revision,nominal_at,due_at) VALUES('synthetic-notice','sleep',?,?,?)", (rev, due, due))
                def decision():
                    values = h.request('GET', API + '/schedule/decisions')[0]['items']
                    return next((v for v in values if v['id'] == 'synthetic-notice' and v['decision_status'] in ('completed', 'failed')), None)
                notice = wait(decision, 65)
                check('one-minute notice invokes the configured model and fixed Python decision script',
                      notice['decision_status'] == 'completed' and notice['decision'] in ('execute', 'cancel', 'postpone'))
                report['model_schedule_choice'] = notice['decision']
                if not args.live_config_from:
                    # Test the actual cutoff with an administrative sleep; the
                    # normal scheduled sleep path is independently unit tested.
                    state = h.request('GET', API + '/rest')[0]['state']
                    h.request('POST', API + '/rest/sleep', {'wake_at': int(time.time()) + 15},
                              headers={'If-Match': f'"{state["generation"]}"', 'Idempotency-Key': 'synthetic-context-cut'})
                    with h.db() as db:
                        cut = db.execute('SELECT count(*) FROM sleep_context_boundaries').fetchone()[0]
                        originals = db.execute('SELECT count(*) FROM messages WHERE run_id=?', (run,)).fetchone()[0]
                    check('accepted sleep cuts conversation context while retaining original log messages', cut > 0 and originals > 0)
                report['status'] = 'passed'
                report['passed'] = len(report['checks'])
                print(json.dumps({'status': 'passed', 'checks': report['passed'], 'commit': h.commit}), flush=True)
            finally:
                h.close()
    except Exception as error:
        report['status'] = 'failed'
        report['error_type'] = type(error).__name__
        raise
    finally:
        report['finished_at'] = base.utc_now()
        output.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        server.shutdown()
        server.server_close()


if __name__ == '__main__':
    main()
