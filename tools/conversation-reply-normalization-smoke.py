#!/usr/bin/env python3
"""Check reply normalization through signed synthetic QQ and a real Rust daemon.

Only loopback HTTP is used for the scripted model. The temporary account is
durably pacing-fenced before enable; production configuration and data are never
read. Scripted output checks host processing, not model expression or QQ delivery.
"""
import argparse
import copy
import hashlib
import http.server
import importlib.util
import json
import pathlib
import platform
import secrets
import sys
import tempfile
import threading

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    'reply_normalization_replay', pathlib.Path(__file__).with_name('conversation-replay-smoke.py'))
replay = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(replay)
batch, base = replay.batch, replay.base
CASES = ('group-normalization', 'five-sentences', 'forbidden-recipient',
         'forbidden-url', 'speaker-isolation')
REAL_FAILURE_TEXT = '你说的“这题”是指哪一题？我这边没看到具体题目内容。'


def sha(value):
    if not isinstance(value, bytes):
        value = json.dumps(value, ensure_ascii=False, sort_keys=True).encode('utf-8')
    return hashlib.sha256(value).hexdigest()


class Model(replay.Model):
    def __init__(self):
        super().__init__()
        self.http = []
        self.expected_key = None


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        model = self.server.model
        if self.headers.get('Authorization') != 'Bearer ' + str(model.expected_key):
            self.send_error(401)
            return
        size = int(self.headers.get('Content-Length', '0'))
        if not 0 < size <= 1_048_576:
            self.send_error(413)
            return
        raw_request = self.rfile.read(size)
        payload = json.loads(raw_request)
        question = model.question(payload)
        with model.condition:
            model.requests.append(payload)
            model.http.append({'question': question, 'body_sha256': sha(raw_request),
                               'body_bytes': size, 'messages_sha256': sha(payload['messages'])})
            model.condition.notify_all()
        content = model.raw_answers.get(question)
        if content is None:
            self.send_error(500)
            return
        body = json.dumps({'choices': [{'message': {'role': 'assistant', 'content': content}}],
                           'usage': {'prompt_tokens': 8, 'completion_tokens': 8}}).encode('utf-8')
        try:
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report', required=True)
    parser.add_argument('--openssl')
    parser.add_argument('--cases', help='comma separated case IDs; default: all five')
    args = parser.parse_args()
    selected = set(args.cases.split(',')) if args.cases else set(CASES)
    if not selected or not selected.issubset(CASES):
        parser.error('unknown case ID')
    if 'speaker-isolation' in selected and 'group-normalization' not in selected:
        parser.error('speaker-isolation requires group-normalization')
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    output = pathlib.Path(args.report).resolve()
    if output.exists():
        parser.error('report already exists; preserve previous evidence and use a new path')
    report = {
        'kind': 'reply normalization signed synthetic GROUP real-process acceptance',
        'status': 'running', 'started_at': base.utc_now(), 'architecture': platform.machine(),
        'binary_path': str(binary), 'binary_sha256': sha(binary.read_bytes()),
        'runner_sha256': sha(pathlib.Path(__file__).read_bytes()), 'source_commit': None,
        'selected_cases': [c for c in CASES if c in selected], 'cases': [],
        'boundaries': {'production_data_accessed': False, 'real_model_tested': False,
                       'real_qq_http_tested': False, 'real_weixin_http_tested': False,
                       'client_delivery_tested': False, 'model_expression_evaluated': False,
                       'signed_inbound_and_scripted_outputs_are_synthetic': True},
        'command': [sys.executable, *sys.argv],
    }
    model = Model()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    server.daemon_threads = True
    server.model = model
    threading.Thread(target=server.serve_forever, daemon=True).start()
    h, runs, exit_code, phase = None, {}, 0, 'temporary setup'

    def rows(sql, params=()):
        return batch.rows(h, sql, params)

    def check(case, name, condition, **evidence):
        case['assertions'].append({'name': name, 'passed': bool(condition), **evidence})
        print(('PASS ' if condition else 'FAIL ') + case['id'] + ' ' + name, flush=True)

    def group_run(case, signer, group, speaker, question, raw):
        model.raw_answers[question] = raw
        value = batch.event(speaker, question)
        value['t'] = 'GROUP_AT_MESSAGE_CREATE'
        value['d']['group_openid'] = group
        value['d']['author'] = {'member_openid': speaker}
        batch.webhook(h, signer, value)
        event_key = 'qq:' + batch.ACCOUNT + ':' + value['t'] + ':' + value['id']
        found = batch.wait_for(lambda: rows('SELECT run_id FROM inbox WHERE event_key=?', (event_key,)),
                               'signed_group_event_missing_inbox')
        run = found[0]['run_id']
        runs[case['id']] = run
        case.update(run_id=run, synthetic_question=question, scripted_raw_answer=raw)
        result = h.wait_run(run, via_api=False)
        case['terminal_status'] = result['status']
        request = model.for_question(question)
        prompt_rows = rows('SELECT messages_json,digest FROM run_prompt_snapshots WHERE run_id=?', (run,))
        frozen = json.loads(prompt_rows[0]['messages_json']) if len(prompt_rows) == 1 else None
        check(case, 'actual_HTTP_matches_immutable_Rust_prompt',
              frozen is not None and request['messages'] == frozen)
        check(case, 'complete_original_question_is_last_user_input',
              frozen is not None and frozen[-1] == {'role': 'user', 'content': question})
        messages = rows("SELECT content FROM messages WHERE run_id=? AND role='user'", (run,))
        check(case, 'canonical_original_user_message_is_preserved',
              len(messages) == 1 and messages[0]['content'] == question)
        check(case, 'terminal_run_releases_actual_model_lease',
              not rows('SELECT 1 FROM native_worker_leases WHERE run_id=?', (run,)))
        return run, result, frozen

    def rejected(case, run, result, expected_error=None):
        check(case, 'whole_invalid_reply_is_failed', result['status'] == 'failed')
        check(case, 'invalid_reply_has_no_plan_or_Outbox',
              not rows('SELECT 1 FROM run_reply_plans WHERE run_id=?', (run,))
              and not rows('SELECT 1 FROM outbox WHERE run_id=?', (run,)))
        if expected_error:
            check(case, 'bounded_host_failure_reason_is_preserved',
                  expected_error in str(result.get('error') or ''))

    try:
        with tempfile.TemporaryDirectory(prefix='dsh-reply-normalization-') as temporary:
            h = base.Harness(str(binary), str(web), temporary, model)
            try:
                h.start()
                h.bootstrap()
                model.expected_key = h.key
                h.configure(server.server_port)
                report['source_commit'] = h.commit
                secret = secrets.token_urlsafe(32)
                signer = batch.Signer(batch.openssl_path(args.openssl), temporary, secret)
                credential, _, _ = h.request('POST', batch.API + '/secrets',
                                              {'kind': 'qq_app_secret', 'value': secret})
                with h.db() as db:
                    db.execute('INSERT INTO reply_account_pacing(account_id,next_ms) VALUES(?,?)',
                               (batch.ACCOUNT, batch.PACING_FENCE_MS))
                config = copy.deepcopy(h.applied)
                config['assistant']['max_concurrent_runs'] = 1
                config['reply'] = {'min_interval_ms': 2200, 'max_interval_ms': 4400, 'max_items': 4}
                config['qq'] = {'enabled': True, 'app_id': batch.ACCOUNT,
                                'credential_ref': credential['id'], 'transport': 'webhook'}
                batch.apply_config(h, config)
                report['configuration'] = {'reply': h.applied['reply'], 'max_concurrent_runs': 1,
                                           'qq_pacing_fence_installed_before_enable': True}
                suffix = secrets.token_hex(5)
                group, a, b = 'normalization_group_' + suffix, 'speaker_A_' + suffix, 'speaker_B_' + suffix
                first = None
                for case_id in CASES:
                    if case_id not in selected:
                        continue
                    phase = case_id
                    case = {'id': case_id, 'status': 'running', 'assertions': []}
                    report['cases'].append(case)
                    try:
                        if case_id == 'group-normalization':
                            # A current-topic preference only becomes active
                            # after A has an actual topic. Admit that topic via
                            # the signed adapter instead of seeding state SQL.
                            setup = {'id': 'group-A-topic-setup', 'assertions': []}
                            case['topic_setup'] = setup
                            topic, topic_result, _ = group_run(
                                setup, signer, group, a, '分析这道合成题：7加8是多少？',
                                json.dumps([{'type': 'text', 'text': '请继续说明这道题。', 'delay_ms': 0}],
                                           ensure_ascii=False))
                            check(case, 'A_topic_is_admitted_and_completed_before_local_preference',
                                  topic_result['status'] == 'completed'
                                  and all(item['passed'] for item in setup['assertions']))
                            raw = json.dumps([{'type': 'text', 'text': REAL_FAILURE_TEXT, 'delay_ms': 0}],
                                             ensure_ascii=False)
                            first, result, frozen = group_run(case, signer, group, a, '这题详细点。', raw)
                            check(case, 'original_real_failure_fixture_now_completes', result['status'] == 'completed')
                            found = rows('SELECT plan_json FROM run_reply_plans WHERE run_id=?', (first,))
                            plan = json.loads(found[0]['plan_json']) if len(found) == 1 else None
                            expected = [{'type': 'text', 'text': '你说的“这题”是指哪一题？', 'delay_ms': 0},
                                        {'type': 'text', 'text': '我这边没看到具体题目内容。', 'delay_ms': 2200}]
                            check(case, 'one_text_is_normalized_to_exactly_two_complete_sentences',
                                  plan is not None and plan['normalized'] and plan['items'] == expected,
                                  observed_plan=plan)
                            outbox = rows('SELECT id,status FROM outbox WHERE run_id=?', (first,))
                            parts = rows('SELECT p.seq,p.payload_json,p.delay_ms,p.status FROM outbox_reply_parts p '
                                         'JOIN outbox o ON o.id=p.outbox_id WHERE o.run_id=? ORDER BY p.seq', (first,))
                            check(case, 'real_Outbox_has_only_two_queued_parts_with_configured_delay',
                                  len(outbox) == 1 and outbox[0]['status'] == 'queued'
                                  and [p['seq'] for p in parts] == [1, 2]
                                  and [p['delay_ms'] for p in parts] == [0, 2200]
                                  and all(p['status'] == 'queued' for p in parts)
                                  and [json.loads(p['payload_json']) for p in parts] == expected)
                            focus = replay.envelope(frozen or [], 'DSH_CONVERSATION_FOCUS_V1')
                            check(case, 'A_local_detail_preference_is_derived_from_its_own_signed_input',
                                  focus is not None and focus.get('response_preference') is not None
                                  and focus['response_preference']['detail'] == 'detailed'
                                  and focus['response_preference']['source_run_id'] == first
                                  and (focus.get('current_focus') or {}).get('source_run_id') == topic,
                                  preference=focus.get('response_preference') if focus else None)
                        elif case_id == 'five-sentences':
                            raw = json.dumps([{'type': 'text', 'text': '第一句。第二句。第三句。第四句。第五句。',
                                               'delay_ms': 0}], ensure_ascii=False)
                            run, result, _ = group_run(case, signer, group, 'five_' + suffix,
                                                       '请验证五句整体数量限制。', raw)
                            rejected(case, run, result, '数量限制')
                        elif case_id.startswith('forbidden-'):
                            field = case_id.removeprefix('forbidden-')
                            item = {'type': 'text', 'text': '合成权限越界样本。', 'delay_ms': 0,
                                    field: 'synthetic_external_recipient' if field == 'recipient'
                                    else 'https://example.invalid/synthetic-not-requested'}
                            run, result, _ = group_run(case, signer, group, field + '_' + suffix,
                                                       '请验证禁止字段' + field + '。',
                                                       json.dumps([item], ensure_ascii=False))
                            rejected(case, run, result)
                        else:
                            raw = json.dumps([{'type': 'text', 'text': '7加8等于15。', 'delay_ms': 0}],
                                             ensure_ascii=False)
                            run, result, frozen = group_run(case, signer, group, b, '7加8是多少？', raw)
                            check(case, 'B_independent_group_turn_completes', result['status'] == 'completed')
                            focus = replay.envelope(frozen or [], 'DSH_CONVERSATION_FOCUS_V1')
                            check(case, 'B_does_not_inherit_A_local_detail_preference',
                                  focus is not None and focus.get('response_preference') is None)
                            identities = rows('SELECT run_id,context_json FROM run_conversation_contexts '
                                              'WHERE run_id IN (?,?)', (first, run))
                            identities = {i['run_id']: json.loads(i['context_json']) for i in identities}
                            left, right = identities.get(first, {}), identities.get(run, {})
                            check(case, 'real_signed_same_group_has_distinct_verified_speakers',
                                  left.get('conversation_id') == right.get('conversation_id')
                                  and left.get('conversation_kind') == right.get('conversation_kind') == 'group'
                                  and left.get('speaker_identity') and right.get('speaker_identity')
                                  and left['speaker_identity'] != right['speaker_identity'])
                    except Exception as error:
                        case.update(status='failed', error_type=type(error).__name__)
                        if isinstance(error, AssertionError):
                            case['error_label'] = str(error)[:240]
                    if case['status'] == 'running':
                        case['status'] = ('failed' if any(not a['passed'] for a in case['assertions']) else 'passed')
                phase = 'outbound safety'
                safety = {'qq_api_call_count': len(rows('SELECT 1 FROM qq_api_calls')),
                          'sending_parts': len(rows("SELECT 1 FROM outbox_reply_parts WHERE status='sending'")),
                          'durable_pacing_fence_retained': rows('SELECT next_ms FROM reply_account_pacing WHERE account_id=?',
                                                              (batch.ACCOUNT,)) == [{'next_ms': batch.PACING_FENCE_MS}],
                          'weixin_enabled': bool(rows('SELECT 1 FROM weixin_account WHERE enabled=1'))}
                report['outbound_safety'] = safety
                good_safety = (not safety['qq_api_call_count'] and not safety['sending_parts']
                               and safety['durable_pacing_fence_retained'] and not safety['weixin_enabled'])
                report['status'] = ('passed' if good_safety and all(c['status'] == 'passed' for c in report['cases'])
                                    else 'failed')
                exit_code = 0 if report['status'] == 'passed' else 1
            finally:
                # Preserve bounded synthetic failure/audit evidence BEFORE the
                # daemon and temporary database are removed. Never copy logs,
                # secret rows, bearer headers, or a real source configuration.
                if h.database.exists():
                    diagnostics = []
                    for case_id, run in runs.items():
                        found = rows('SELECT status,error,response FROM runs WHERE id=?', (run,))
                        if found:
                            value = found[0]
                            for field in ('error', 'response'):
                                text = str(value[field] or '')
                                for secret_value in (h.key, h.password):
                                    text = text.replace(secret_value, '[redacted]')
                                value[field] = text[:2000]
                            diagnostics.append({'case_id': case_id, 'run_id': run, **value})
                    report['synthetic_run_diagnostics_before_cleanup'] = diagnostics
                h.close()
    except Exception as error:
        report.update(status='failed', failed_phase=phase, error_type=type(error).__name__)
        if isinstance(error, AssertionError):
            report['error_label'] = str(error)[:240]
        exit_code = 1
    finally:
        server.shutdown()
        server.server_close()
        report['actual_loopback_HTTP'] = model.http
        report['provider_request_count'] = model.count()
        report['finished_at'] = base.utc_now()
        report['summary'] = {'passed_cases': sum(c['status'] == 'passed' for c in report['cases']),
                             'failed_cases': sum(c['status'] == 'failed' for c in report['cases']),
                             'passed_assertions': sum(a['passed'] for c in report['cases'] for a in c['assertions']),
                             'failed_assertions': sum(not a['passed'] for c in report['cases'] for a in c['assertions'])}
        report['exit_code'] = exit_code
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
        print(json.dumps({'status': report['status'], **report['summary']}, ensure_ascii=False), flush=True)
    return exit_code


if __name__ == '__main__':
    raise SystemExit(main())
