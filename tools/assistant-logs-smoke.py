#!/usr/bin/env python3
"""Single assistant and runtime logs acceptance using only synthetic local data.

Uses a real daemon, disposable database, loopback mock model and generated
credentials. No production data or external model/channel requests are used.
Reports contain metadata/check names only.
"""
import argparse
import copy
import datetime
import hashlib
import http.server
import importlib.util
import json
import pathlib
import platform
import tempfile
import threading
import urllib.parse


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, pathlib.Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


base = module('assistant_log_harness', 'memory-smoke.py')
candidate = module('assistant_log_owners', 'memory-candidate-smoke.py')
API = base.PREFIX
PERSONALITY = API + '/assistant/personality'
LOGS = API + '/runtime-logs'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report')
    args = parser.parse_args()
    binary = str(pathlib.Path(args.binary).resolve(strict=True))
    web = str(pathlib.Path(args.web_dir).resolve(strict=True))
    checks = []

    def check(name, condition=True):
        if not condition:
            raise AssertionError(name)
        checks.append(name)
        print('PASS ' + name, flush=True)

    mock = base.MockState()
    provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), base.Provider)
    provider.daemon_threads = True
    provider.mock_state = mock
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-assistant-logs-') as temporary:
            h = base.Harness(binary, web, temporary, mock)
            try:
                h.start()
                for path in (PERSONALITY, LOGS):
                    h.request('GET', path, expected=401, authenticated=False)
                check('personality and logs require authentication')
                h.bootstrap()
                h.configure(provider.server_port)
                initial, _, _ = h.request('GET', PERSONALITY)
                value = copy.deepcopy(initial['personality'])
                value.update(name='Synthetic assistant', personality='PERSONALITY_ONE_SENTINEL',
                             description='Synthetic single assistant acceptance',
                             system_prompt='Be concise and precise.')
                h.request('PUT', PERSONALITY, {'personality': value}, expected=403,
                          with_csrf=False, headers={'If-Match': initial['etag']})
                check('personality writes require CSRF')
                saved, _, _ = h.request('PUT', PERSONALITY, {'personality': value},
                                        headers={'If-Match': initial['etag']})
                h.request('PUT', PERSONALITY, {'personality': value}, expected=409,
                          headers={'If-Match': initial['etag']})
                check('personality saves with optimistic concurrency', saved['personality'] == value)
                invalid = dict(value, name='')
                h.request('PUT', PERSONALITY, {'personality': invalid}, expected=422,
                          headers={'If-Match': saved['etag']})
                check('personality validates required fields')
                for path, body in ((API + '/character-cards', {}),
                                   (API + '/character-cards/default', {'card_id':'old', 'revision':1})):
                    h.request('PUT' if path.endswith('/default') else 'POST', path, body, expected=410)
                check('legacy role creation and switching are disabled')
                h.request('POST', API + '/conversations', {
                    'title':'rejected selection', 'character':{'card_id':'old','revision':1}}, expected=410)
                check('explicit conversation role selection is rejected')
                conversation = h.conversation('single assistant acceptance')
                first, payload = h.model_turn(conversation, 'first-single-assistant-probe')
                check('configured personality reaches actual HTTP model request',
                      'PERSONALITY_ONE_SENTINEL' in json.dumps(payload))
                value['personality'] = 'PERSONALITY_TWO_SENTINEL'
                revised, _, _ = h.request('PUT', PERSONALITY, {'personality': value},
                                          headers={'If-Match': saved['etag']})
                second, payload = h.model_turn(conversation, 'second-single-assistant-probe')
                system = '\n'.join(str(item['content']) for item in payload['messages'] if item['role'] == 'system')
                check('existing conversations use current single personality on next run',
                      'PERSONALITY_TWO_SENTINEL' in system and 'PERSONALITY_ONE_SENTINEL' not in system)
                with h.db() as db:
                    old_snapshot = db.execute('SELECT character_snapshot_json FROM runs WHERE id=?',(first,)).fetchone()[0]
                check('completed run retains original personality snapshot',
                      'PERSONALITY_ONE_SENTINEL' in old_snapshot)
                logs, raw, headers = h.request('GET', LOGS + '?run_id=' + second)
                kinds = {item['kind'] for item in logs['items']}
                check('logs cover real input output and run lifecycle', {'input','output','run'} <= kinds)
                check('runtime logs forbid browser caching', 'no-store' in headers.get('Cache-Control', ''))
                check('runtime logs never expose configured API key', h.key.encode() not in raw)
                check('unimplemented ASR and subagent remain explicit',
                      logs['capabilities']['asr'] is False and logs['capabilities']['subagent'] is False)
                filtered, _, _ = h.request('GET', LOGS + '?kind=output&run_id=' + second)
                check('kind and run filters are honored', bool(filtered['items']) and all(
                    item['kind']=='output' and item['run_id']==second for item in filtered['items']))
                page, _, _ = h.request('GET', LOGS + '?limit=2')
                next_page, _, _ = h.request('GET', LOGS + '?limit=2&cursor=' + page['next_cursor'])
                check('cursor pagination is stable and does not overlap',
                      len(page['items'])==2 and bool(next_page['items']) and
                      not ({item['id'] for item in page['items']} & {item['id'] for item in next_page['items']}))
                with candidate.other_admin(h):
                    foreign, _, _ = h.request('GET', LOGS + '?run_id=' + second)
                    check('another administrator cannot read private Web run logs', not foreign['items'])
                history, _, _ = h.request('GET', PERSONALITY + '/history')
                previous = next(item for item in history['items'] if item['revision'] == saved['revision'])
                revised, _, _ = h.request('PUT', PERSONALITY, {'personality': previous['personality']},
                                          headers={'If-Match': revised['etag']})
                check('restoring a historical personality creates a new current revision',
                      revised['revision'] > previous['revision'] and
                      revised['personality']['personality'] == 'PERSONALITY_ONE_SENTINEL')
                before_ids = {item['id'] for item in logs['items']}
                h.stop()
                h.start()
                h.login()
                restored, _, _ = h.request('GET', PERSONALITY)
                persisted, _, _ = h.request('GET', LOGS + '?run_id=' + second)
                check('personality and logs survive daemon restart',
                      restored['etag']==revised['etag'] and before_ids <= {item['id'] for item in persisted['items']})
                caps, _, _ = h.request('GET', API + '/capabilities')
                check('capabilities describe a single assistant and no role switching',
                      caps['character_cards'] is False and caps['qq_persona_switch'] is False)
                report = {
                    'kind':'single assistant and logs isolated black-box acceptance',
                    'commit':h.commit, 'binary_sha256':hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest(),
                    'tested_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),
                    'platform':platform.machine(), 'passed':len(checks), 'checks':checks,
                    'external_model_tested':False, 'real_qq_tested':False,
                    'asr_tested':False, 'subagent_tested':False,
                }
                if args.report:
                    pathlib.Path(args.report).write_text(json.dumps(report, ensure_ascii=False, indent=2),encoding='utf-8')
                print(json.dumps({'passed':len(checks), 'commit':h.commit}))
            finally:
                h.close()
    finally:
        provider.shutdown()
        provider.server_close()


if __name__ == '__main__':
    main()
