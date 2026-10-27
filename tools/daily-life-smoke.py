#!/usr/bin/env python3
"""Daily-life acceptance in a disposable daemon; no real channel is enabled.

Optional live configuration reads only the deployed model profile/key. Test
inputs, diaries, accounts and schedule are synthetic and remain isolated.
"""
import argparse
import base64
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
        users=[m['content'] for m in payload['messages'] if m['role']=='user' and isinstance(m.get('content'),str)]
        latest=users[-1] if users else ''
        replay = None
        if 'DSH_SELF_PLAN_COMMIT_V2' not in systems and 'DSH_CONVERSATION_IMAGE_DESCRIPTION_V1' not in systems:
            if latest.startswith('REPLAY_PROMISE_CASE'):
                replay = '我趁这会儿去把《鹿柴》抄一抄，抄完也歇。'
            elif latest.startswith('REPLAY_QUOTE_CASE'):
                replay = '你说过“我趁这会儿先去整理想法”，我记得。'
            elif latest.startswith('REPLAY_CONDITIONAL_CASE'):
                replay = '如果我现在有空，也许会去整理一些东西。'
        if getattr(self.server,'relay',None) and replay is None:
            try:
                status, raw = self.server.relay.forward(payload)
            except Exception:
                status, raw = 503, b'{"error":"bounded_relay_failed"}'
            self.send_response(status)
            self.send_header('Content-Type','application/json')
            self.send_header('Content-Length',str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
            return
        if replay is not None:
            result=[{'type':'text','text':replay,'delay_ms':0}]
        elif 'DSH_SCHEDULE_NOTICE_V1' in systems:
            result={'decision':'execute','delay_minutes':0}
        elif 'DSH_SELF_PLAN_COMMIT_V2' in systems:
            given=json.loads(latest)
            negative=any(w in given['reply'] for w in ('你说过','如果','也许'))
            result={'self_activity':None} if negative else {'self_activity':{'slot_id':given['slots'][0]['slot_id'],'title':'整理想法','content':'SYNTHETIC_WRITTEN_RESULT'}} if given['slots'] else {'self_activity':None,'unfulfilled':True}
        elif 'DSH_CONVERSATION_IMAGE_DESCRIPTION_V1' in systems:
            result='图片里有一个蓝色杯子，写着 SYNTHETIC_CUP42。'
        else:
            offer=next((json.loads(line)['available'] for line in systems.splitlines() if line.startswith('{"actual_activities":') or line.startswith('{"available":')),None)
            if offer and offer['slots'] and ('安排' in latest or 'REPLAY_PROMISE' in latest):
                result=[{'type':'text','text':'我趁这会儿先把想法整理成一段记录。','delay_ms':0}]
            elif 'DSH_VISION_DESCRIPTION_SOURCE_V1' in systems or '图片来源边界' in systems:
                result=[{'type':'text','text':'是一只蓝色的杯子，杯身上写着 CUP42。','delay_ms':0}]
            else:
                result=[{'type':'text','text':'这句我记下了。','delay_ms':0}]
        raw = json.dumps({'choices': [{'message': {'content': result if isinstance(result,str) else json.dumps(result, ensure_ascii=False)},
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
    p.add_argument('--max-real-requests',type=int,default=12)
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
    server.relay = None
    if args.live_config_from:
        relay_spec=importlib.util.spec_from_file_location('bounded_life_relay',pathlib.Path(__file__).with_name('bounded-model-relay.py'))
        relay_module=importlib.util.module_from_spec(relay_spec)
        relay_spec.loader.exec_module(relay_module)
        if not 1<=args.max_real_requests<=12: raise ValueError('real_request_budget_must_be_1_to_12')
        server.relay=relay_module.Relay(args.live_config_from,args.max_real_requests)
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
                h.configure(server.server_port)
                if server.relay:
                    current=h.request('GET',API+'/config')[0]
                    candidate=json.loads(json.dumps(current['config']))
                    candidate['model']['model']=server.relay.model
                    draft=h.request('POST',API+'/config/drafts',{'base_revision':current['revision'],'config':candidate})[0]
                    h.request('POST',API+'/config/drafts/'+draft['id']+'/apply',{},expected=202,headers={'If-Match':str(current['revision'])})
                    wait(lambda:h.request('GET',API+'/config')[0]['config']['model']['model']==server.relay.model)
                h.request('POST', API + '/task-triggers/clock/acknowledge', {'observed_now': int(time.time()), 'confirmed': True})
                grant_policy={'title':'日常自主安排','allowed_types':['writing'],'resource_ids':[],'time_zone':'Asia/Shanghai','valid_from':int(time.time())-1,'valid_until':int(time.time())+86400,'horizon_days':1,'window_start':'00:00','window_end':'23:59','max_daily_activities':8,'max_daily_minutes':80,'max_daily_changes':24}
                h.request('POST',API+'/schedule/autonomy-grants',grant_policy,expected=403,with_csrf=False)
                personal=h.request('POST',API+'/schedule/autonomy-grants',grant_policy)[0]
                h.request('PATCH',API+'/schedule/autonomy-grants/'+personal['id'],{'expected_revision':1,'policy':grant_policy})
                h.request('PATCH',API+'/schedule/autonomy-grants/'+personal['id'],{'expected_revision':1,'policy':grant_policy},expected=409)
                check('standard work grants enforce CSRF and revisions without a hobby-specific switch',h.request('GET',API+'/schedule/personal-policy')[0]['revision']==0)
                route = API + '/assistant/personality'
                personality = json.loads((pathlib.Path(__file__).resolve().parents[1] / 'docs/examples/liu-jiahui-personality-v3.json').read_text(encoding='utf-8'))
                current = h.request('GET', route)[0]
                h.request('PUT', route, {'personality': personality}, headers={'If-Match': current['etag']})
                h.request('PUT', API + '/diary/policies', {'source_owner_id':h.owner,'enabled':True,'allow_model':False,'timezone':'Asia/Shanghai'}, headers={'If-Match':'0'})
                conversation = h.conversation('synthetic own activities')
                run = h.submit(conversation, '你决定接下来十分钟做什么，请安排一件能完成的事，记到日程并留下成果。')
                def terminal():
                    result = h.request('GET', API + '/runs/' + run)[0]
                    return result if result['status'] in base.TERMINAL else None
                result = wait(terminal)
                report['initial_run_status']=result['status']
                report['initial_run_error']=result.get('error')
                report['synthetic_plan_answer'] = result.get('response', '')
                with h.db() as db:
                    generated=db.execute('SELECT raw_generation FROM rest_deferred_presentations WHERE run_id=?',(run,)).fetchone()
                    report['synthetic_initial_generation']=generated[0][:16000] if generated else None
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
                check('fixed system instructions contain no prescribed hobby or poem catalogue',
                      all(word not in systems for word in ('抄诗','poem_id','catalog')))
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
                check('scheduled general writing leaves a real private artifact and content hash',
                      saved['method'] == 'saved_text_artifact' and saved['sha256'] == hashlib.sha256(saved['text'].encode()).hexdigest())
                events = h.request('GET', API + '/diary/events')[0]['items']
                check('completed activity is recorded in the source-private diary without publishing its content',
                      any(e['origin_type'] == 'schedule_activity' and e['visibility']=='private' for e in events))
                replay=h.submit(h.conversation('known reply replay'),'REPLAY_PROMISE_CASE: reproduce the known missing-structure response for the semantic check.')
                check('known natural QQ statement completes through semantic checking',h.wait_run(replay)['status']=='completed')
                with h.db() as db:
                    replay_activity=db.execute('SELECT activity_id FROM daily_life_activities WHERE source_run_id=?',(replay,)).fetchone()
                check('known reply replay really registers an activity without a sentence whitelist',bool(replay_activity))
                report['known_reply_native_injected']=True
                for marker in ('REPLAY_QUOTE_CASE','REPLAY_CONDITIONAL_CASE'):
                    checked=h.submit(h.conversation(marker),marker)
                    check(marker+' remains ordinary dialogue',h.wait_run(checked)['status']=='completed')
                    with h.db() as db:
                        count=db.execute('SELECT count(*) FROM daily_life_activities WHERE source_run_id=?',(checked,)).fetchone()[0]
                    check(marker+' does not create an activity',count==0)
                formats=['jpg'] if args.live_config_from else ['png','jpg','webp','gif','bmp']
                for extension in formats:
                    picture=(pathlib.Path(__file__).parent/'fixtures/common-images'/('cup.'+extension)).read_bytes()
                    identifier=h.conversation('synthetic '+extension+' picture')
                    image_run=h.request('POST',API+'/conversations/'+identifier+'/image-messages',{'content':'图片里的杯子是什么颜色？自然答一句。','client_message_id':'image-'+extension,'image_base64':base64.b64encode(picture).decode()},expected=202)[0]['run_id']
                    answer=h.wait_run(image_run)
                    check(extension+' passes actual decode, vision and ordinary native reply',answer['status']=='completed' and any(word in answer.get('response','').lower() for word in ('蓝','blue')))
                    check(extension+' reply does not expose processing or format parameters',not any(word in answer.get('response','') for word in ('VisionProvider','PNG','H.264','视觉描述','DeepSeek','MiB')))
                broken=h.conversation('synthetic corrupt picture')
                h.request('POST',API+'/conversations/'+broken+'/image-messages',{'content':'看看图片。','client_message_id':'bad-image','image_base64':base64.b64encode(b'\xff\xd8\xffbroken').decode()},expected=400)
                check('corrupt image is rejected before creating a false visual answer')
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
        if server.relay:
            report['real_request_limit']=server.relay.limit
            report['real_requests']=server.relay.records
            report['peak_real_requests']=server.relay.peak_active
        report['finished_at'] = base.utc_now()
        output.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        server.shutdown()
        server.server_close()


if __name__ == '__main__':
    main()
