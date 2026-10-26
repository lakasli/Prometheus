#!/usr/bin/env python3
"""Run the 40 versioned conversation fixtures against an isolated Rust daemon.

Host assertions use actual admission, model HTTP capture, frozen SQLite evidence
and restarts. The model is loopback and scripted: expression is NEVER marked as
passed from these replies. All platform receipts/faults are synthetic, under a
TemporaryDirectory; an account pacing fence is installed before QQ is enabled.
No production data, real QQ/Weixin HTTP or real model is accessed; the host clock
is never changed.
"""
import argparse
import copy
import datetime
import hashlib
import http.server
import importlib.util
import json
import pathlib
import secrets
import sys
import tempfile
import threading
import time
import uuid

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    'replay_coordination_harness', pathlib.Path(__file__).with_name('conversation-coordination-smoke.py'))
coord = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(coord)
batch, base = coord.batch, coord.base
API = batch.API
EXPECTED_IDS = ([f'C{i:02}' for i in range(1, 9)] + [f'D{i:02}' for i in range(1, 8)]
                + [f'R{i:02}' for i in range(1, 8)] + [f'S{i:02}' for i in range(1, 7)]
                + [f'E{i:02}' for i in range(1, 7)] + [f'F{i:02}' for i in range(1, 7)])


def digest(value):
    if not isinstance(value, bytes):
        value = json.dumps(value, ensure_ascii=False, sort_keys=True).encode('utf-8')
    return hashlib.sha256(value).hexdigest()


def flat(messages, role=None):
    return '\n'.join(m['content'] for m in messages if role is None or m['role'] == role)


def envelope(messages, marker):
    for message in messages:
        if marker + '\n' in message['content']:
            section = message['content'].partition(marker + '\n')[2]
            for line in section.splitlines():
                try:
                    return json.loads(line)
                except json.JSONDecodeError:
                    continue
    return None


class Model(coord.ModelState):
    def __init__(self):
        super().__init__()
        self.raw_answers = {}
        self.fail_questions = set()
        self.summary_text = 'SYNTHETIC_REPLAY_SUMMARY'


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        size = int(self.headers.get('Content-Length', '0'))
        if not 0 < size <= 1_048_576:
            self.send_error(413)
            return
        payload = json.loads(self.rfile.read(size))
        model = self.server.model
        question = model.question(payload)
        with model.condition:
            model.requests.append(payload)
            model.condition.notify_all()
        held = question == model.hold_question
        if held:
            model.hold_received.set()
            model.hold_release.wait(timeout=45)
        if question in model.fail_questions:
            self.send_error(503)
            return
        is_summary = '对话资料压缩器' in payload['messages'][0].get('content', '')
        content = model.summary_text if is_summary else model.raw_answers.get(question)
        if content is None:
            content = json.dumps([
                {'type': 'text', 'text': answer, 'delay_ms': 0 if i == 0 else 1500}
                for i, answer in enumerate(model.answers.get(question, ['SYNTHETIC_REPLAY_REPLY']))
            ], ensure_ascii=False)
        raw = json.dumps({'choices': [{'message': {'role': 'assistant', 'content': content}}],
                          'usage': {'prompt_tokens': 8, 'completion_tokens': 8}}).encode()
        try:
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass
        finally:
            if held:
                model.hold_responded.set()


class Replay:
    def __init__(self, harness, model, signer, report):
        self.h, self.model, self.signer, self.report = harness, model, signer, report
        self.case = None
        self.suffix = secrets.token_hex(5)

    def rows(self, sql, arguments=()):
        return batch.rows(self.h, sql, arguments)

    def scalar(self, sql, arguments=()):
        values = self.rows(sql, arguments)
        return next(iter(values[0].values())) if values else None

    def setup(self, action, **metadata):
        self.case['setup_evidence'].append({'action': action, **metadata})

    def check(self, name, condition, criteria=(), **metadata):
        success = bool(condition)
        self.case['host']['assertions'].append({
            'name': name, 'status': 'passed' if success else 'failed',
            'fixture_criteria': list(criteria), **metadata})
        print(('PASS ' if success else 'FAIL ') + self.case['id'] + ' ' + name, flush=True)
        return success

    def note(self, name, reason, criteria=()):
        self.case['host']['not_evaluated'].append({
            'name': name, 'reason': reason, 'fixture_criteria': list(criteria)})

    def chat(self, label=None):
        chat = self.h.conversation('合成回放 ' + (label or self.case['id']))
        self.setup('authenticated_conversation_created', conversation_ref=chat)
        return chat

    def user(self):
        return 'replay_' + self.case['id'] + '_' + self.suffix

    def submit(self, question, chat=None, user=None, wait=True, group=None):
        before = self.model.count()
        if chat is not None:
            run = self.h.submit(chat, question)
        else:
            value = batch.event(user or self.user(), question)
            if group is not None:
                value['t'] = 'GROUP_AT_MESSAGE_CREATE'
                value['d']['group_openid'] = group
                value['d']['author'] = {'member_openid': user or self.user()}
            batch.webhook(self.h, self.signer, value)
            if group is None:
                run = batch.received_runs(self.h, [value])[0]
            else:
                key = 'qq:' + batch.ACCOUNT + ':' + value['t'] + ':' + value['id']
                run = batch.wait_for(lambda: self.scalar('SELECT run_id FROM inbox WHERE event_key=?', (key,)),
                                     'signed_group_event_did_not_reach_Inbox')
        self.case['run_refs'].append(run)
        self.setup('real_daemon_input_admitted', run_ref=run,
                   transport='authenticated_web' if chat else 'signed_synthetic_qq',
                   input_digest=digest(question), request_count_before=before)
        if wait:
            result = self.h.wait_run(run, via_api=False)
            if result['status'] != 'completed':
                error = str(result.get('error') or '')
                for secret in (self.h.key, self.h.password):
                    error = error.replace(secret, '[redacted]')
                self.setup('run_failed_with_controlled_host_error', run_ref=run,
                           terminal_status=result['status'], controlled_error=error[:500],
                           provider_request_count_after=self.model.count())
                raise AssertionError('synthetic_run_not_completed:' + result['status'])
        return run

    def messages(self, run):
        result = self.rows('SELECT messages_json FROM run_prompt_snapshots WHERE run_id=?', (run,))
        if len(result) != 1:
            raise AssertionError('missing_frozen_prompt')
        messages = json.loads(result[0]['messages_json'])
        observed = [request for request in self.model.requests if request['messages'] == messages]
        self.check('frozen_prompt_matches_actual_loopback_HTTP', bool(observed),
                   run_ref=run, messages_digest=digest(messages))
        return messages

    def budget(self, run):
        return json.loads(self.scalar('SELECT snapshot_json FROM context_budget_snapshots WHERE run_id=?', (run,)))

    def focus(self, run):
        return envelope(self.messages(run), 'DSH_CONVERSATION_FOCUS_V1')

    def publication(self, run):
        return self.rows('SELECT * FROM run_publication_decisions WHERE run_id=?', (run,))[0]

    def held(self, question, chat=None, user=None):
        self.model.hold_question = question
        self.model.hold_release.clear()
        self.model.hold_received.clear()
        self.model.hold_responded.clear()
        run = self.submit(question, chat=chat, user=user, wait=False)
        if not self.model.hold_received.wait(timeout=12):
            raise TimeoutError('model_HTTP_barrier_not_reached')
        self.setup('actual_loopback_HTTP_held', run_ref=run)
        return run

    def release(self, *runs):
        self.model.hold_release.set()
        for run in runs:
            self.h.wait_run(run, via_api=False)

    def outbox(self, run):
        return self.rows('SELECT * FROM outbox WHERE run_id=?', (run,))[0]

    def source_parts(self, count=2, user=None):
        question = 'SYNTHETIC_' + self.case['id'] + '_SOURCE_' + self.suffix
        texts = ['SYNTHETIC_' + self.case['id'] + '_PART_' + str(i) for i in range(1, count + 1)]
        self.model.answers[question] = texts
        source = self.submit(question, user=user)
        outbox = self.outbox(source)
        self.check('setup_created_actual_ordered_reply_parts', self.scalar(
            'SELECT count(*) FROM outbox_reply_parts WHERE outbox_id=?', (outbox['id'],)) == count)
        return source, outbox, texts

    def receipt(self, outbox, seq=1, status='sent', error=None, payload=None):
        with self.h.db() as db:
            db.execute('UPDATE outbox_reply_parts SET status=?,platform_message_id=?,finished_ms=?,error=? '
                       'WHERE outbox_id=? AND seq=?',
                       (status, 'synthetic-receipt-' + self.case['id'] if status == 'sent' else None,
                        int(time.time() * 1000), error, outbox['id'], seq))
            if payload is not None:
                db.execute('UPDATE outbox_reply_parts SET payload_json=? WHERE outbox_id=? AND seq=?',
                           (json.dumps(payload), outbox['id'], seq))
            if status in ('unknown', 'sending'):
                db.execute('UPDATE outbox SET status=? WHERE id=?', (status, outbox['id']))
        self.setup('synthetic_platform_evidence_in_temporary_db', outbox_ref=outbox['id'],
                   seq=seq, state=status, actual_platform_HTTP=False)

    def no_actions(self, before, criteria=()):
        after = self.action_counts()
        self.check('conversation_did_not_create_schedule_permission_or_memory', after == before,
                   criteria, before=before, after=after)

    def action_counts(self):
        result = {}
        for table in ['memory_records', 'schedule_activities', 'schedule_autonomy_grants',
                      'schedule_admin_approvals', 'conversation_commitments', 'sleep_cycles',
                      'relationship_people', 'relationship_assertions', 'relationship_contact_permissions']:
            exists = self.scalar("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name=?", (table,))
            if exists:
                result[table] = self.scalar('SELECT count(*) FROM ' + table)
        return result

    def memory(self, chat, content):
        value, _, _ = self.h.request('POST', API + '/memory', {
            'content': content, 'visibility': 'private', 'source_conversation_id': chat}, expected=201)
        self.setup('authenticated_explicit_private_memory_created', memory_ref=value['id'])
        return value

    def policy(self, change=None):
        view, _, _ = self.h.request('GET', API + '/context-policy')
        if change is None:
            return view['policy']
        candidate = copy.deepcopy(view['policy'])
        candidate.update(change)
        result, _, _ = self.h.request('PUT', API + '/context-policy', candidate,
                                    headers={'If-Match': '"' + str(view['policy']['revision']) + '"'})
        self.setup('authenticated_budget_policy_changed', revision=result['policy']['revision'])
        return result['policy']

    def restore_policy(self, policy):
        current = self.policy()
        replacement = copy.deepcopy(policy)
        replacement['revision'] = current['revision']
        self.h.request('PUT', API + '/context-policy', replacement,
                       headers={'If-Match': '"' + str(current['revision']) + '"'})

    def diary_policy(self, enabled):
        page, _, _ = self.h.request('GET', API + '/diary/policies')
        current = next((p for p in page['items'] if p['source_owner_id'] == self.h.owner), None)
        self.h.request('PUT', API + '/diary/policies', {
            'source_owner_id': self.h.owner, 'enabled': enabled, 'allow_model': enabled,
            'timezone': 'Asia/Shanghai'}, headers={'If-Match': str(current['revision'] if current else 0)})
        self.setup('authenticated_diary_source_policy_changed', enabled=enabled, allow_model=enabled)

    def proposal(self, chat, source, intent, confirmed=False):
        path = API + '/conversations/' + chat + '/commitments'
        query = '?run_id=' + source
        original = self.scalar('SELECT input FROM runs WHERE id=?', (source,))
        value, _, _ = self.h.request('POST', path + query,
            {'intent': intent, 'kind': 'reading', 'time_text': '明天' if '明天' in original else None,
             'sources': []},
            headers={'Idempotency-Key': str(uuid.uuid4())})
        if confirmed:
            value, _, _ = self.h.request('POST', path + '/' + value['id'] + '/confirm' + query,
                {'confirmed': True}, headers={'Idempotency-Key': str(uuid.uuid4()),
                                             'If-Match': '"' + str(value['revision']) + '"'})
        self.setup('explicit_management_commitment_' + ('confirmed' if confirmed else 'proposed'),
                   commitment_ref=value['id'], source_run_ref=source)
        return value, path + '/' + value['id'] + query


def corrections(r, fixture):
    case = fixture['id']
    turns = [turn['text'] for turn in fixture['turns']]
    user = r.user()
    source_question = turns[0] if case in ('C01', 'C02', 'C03') else 'SYNTHETIC_' + case + '_ORIGINAL_QUESTION'
    stale = 'SYNTHETIC_STALE_' + case
    r.model.answers[source_question] = [stale]
    if case == 'C04':
        old = r.submit(source_question, user=user)
        original = coord.snapshot(r.h, old)
        outbox = r.outbox(old)
        later = r.submit(turns[0], user=user)
        messages = r.messages(later)
        correction = r.rows('SELECT * FROM conversation_corrections WHERE run_id=?', (later,))
        r.check('explicit_withdrawal_persistently_links_and_suppresses_the_queued_candidate',
                r.publication(old)['decision'] == 'suppressed' and len(correction) == 1
                and correction[0]['predecessor_run_id'] == old
                and correction[0]['predecessor_suppressed'] == 1, ('must:0',))
        r.check('queued_unclaimed_parts_cancel_without_erasing_question_generation_or_original_snapshot',
                r.scalar('SELECT status FROM outbox WHERE id=?', (outbox['id'],)) == 'cancelled'
                and r.scalar('SELECT input FROM runs WHERE id=?', (old,)) == source_question
                and stale in r.scalar('SELECT response FROM runs WHERE id=?', (old,))
                and coord.snapshot(r.h, old) == original
                and source_question in flat(messages) and stale not in flat(messages),
                ('must:1', 'must_not:0'))
        return
    old = r.held(source_question, user=user)
    original = coord.snapshot(r.h, old)
    if case == 'C07':
        with r.h.db() as db:
            db.execute('UPDATE run_publication_decisions SET effects_started=1 WHERE run_id=?', (old,))
        r.setup('synthetic_persistent_effect_started_fence', run_ref=old,
                actual_external_effect=False)
    correction = turns[1] if case in ('C01', 'C02', 'C03') else (
        '等等，改成明天' if case == 'C08' else turns[0])
    later = r.submit(correction, user=user, wait=False)
    expected_suppression = case in ('C01', 'C02', 'C04', 'C08')
    publication = r.publication(old)
    r.check('inflight_input_has_distinct_successor_and_physical_lease',
            later != old and not r.model.hold_responded.is_set()
            and r.scalar('SELECT count(*) FROM native_worker_leases WHERE run_id=?', (old,)) == 1
            and r.scalar('SELECT status FROM runs WHERE id=?', (later,)) == 'queued'
            and not r.rows('SELECT 1 FROM run_prompt_snapshots WHERE run_id=?', (later,)),
            ('must:0',))
    r.check('frozen_original_prompt_is_unchanged', coord.snapshot(r.h, old) == original,
            ('must_not:0',))
    r.check('publication_matches_explicit_control_and_side_effect_fence',
            publication['decision'] == ('suppressed' if expected_suppression else 'active'),
            ('must:0', 'must_not:0'), actual_decision=publication['decision'])
    if case == 'C07':
        row = r.rows('SELECT reason FROM conversation_corrections WHERE run_id=?', (later,))
        r.check('started_effect_is_preserved_and_not_automatically_replayed',
                bool(row) and row[0]['reason'] == 'effects_started'
                and r.publication(old)['effects_started'] == 1, ('must:0', 'must_not:1'))
    if case == 'C08':
        r.h.stop()
        r.model.hold_release.set()
        r.h.start()
        r.h.login()
        restored = r.publication(old)
        r.check('restart_preserves_exact_suppression_revision_without_outbox',
                restored == publication and coord.snapshot(r.h, old) == original
                and not r.rows('SELECT 1 FROM outbox WHERE run_id=?', (old,)),
                ('must:0', 'must_not:0'))
        status = r.scalar('SELECT status FROM runs WHERE id=?', (old,))
        r.check('restart_does_not_invent_completion_for_interrupted_HTTP',
                status in ('interrupted', 'outcome_unknown', 'failed'), ('must:1',), actual_status=status)
        r.h.wait_run(later, via_api=False)
        follow = r.submit(turns[0], user=user)
        messages = r.messages(follow)
        r.check('restart_successor_keeps_correction_context_without_stale_assistant',
                source_question in flat(messages) and stale not in flat(messages, 'assistant')
                and correction in flat(messages), ('must:0', 'must_not:0'))
        return
    r.release(old, later)
    messages = r.messages(later)
    if expected_suppression:
        r.check('suppressed_generation_completed_for_audit_without_outbox',
                r.scalar('SELECT status FROM runs WHERE id=?', (old,)) == 'completed'
                and stale in r.scalar('SELECT response FROM runs WHERE id=?', (old,))
                and not r.rows('SELECT 1 FROM outbox WHERE run_id=?', (old,)), ('must:1',))
        r.check('successor_retains_original_input_and_correction_without_unsent_answer',
                source_question in flat(messages) and correction in flat(messages)
                and stale not in flat(messages), ('must:1', 'must_not:0'))
    else:
        r.check('ordinary_quoted_question_or_started_effect_preserves_predecessor_audit',
                r.scalar('SELECT status FROM runs WHERE id=?', (old,)) == 'completed'
                and r.publication(old)['decision'] == 'active'
                and r.scalar("SELECT count(*) FROM outbox WHERE run_id=? AND status='queued'", (old,)) == 1,
                ('must:0', 'must_not:0'))
    if case == 'C01':
        temporal = envelope(messages, 'DSH_CONVERSATION_TIME_V1')
        r.check('relative_date_is_grounded_in_actual_host_snapshot', bool(temporal), ('must:0',))
        if temporal and temporal.get('now_local'):
            snapshot_time = datetime.datetime.fromisoformat(temporal['now_local'])
            r.case['effective_time_evidence'] = {
                'source': 'actual_frozen_host_time_envelope', 'now_local': temporal['now_local'],
                'original_expected_corrected_date': '2026-10-27',
                'shifted_expected_corrected_date': (snapshot_time.date() + datetime.timedelta(days=1)).isoformat()}
        r.note('model_resolves_corrected_date', 'loopback replies cannot prove date reasoning', ('must:0',))
    elif case == 'C02':
        r.note('model_computes_23_times_3_as_69', 'loopback replies cannot prove arithmetic reasoning', ('must:0',))


def deliveries(r, fixture):
    case, question = fixture['id'], fixture['turns'][0]['text']
    if case == 'D04':
        chat = r.chat()
        source_question = 'SYNTHETIC_WEIXIN_OVERSIZE_' + r.suffix
        long_marker = 'SYNTHETIC_LONG_WEIXIN_CANDIDATE'
        # Two individually valid text items, together beyond iLink's 4096 byte
        # text limit. Avoid violating the independent per-item reply contract.
        r.model.answers[source_question] = [long_marker + '甲' * 800, long_marker + '乙' * 800]
        source = r.submit(source_question, chat=chat)
        context, outbox, at = str(uuid.uuid4()), str(uuid.uuid4()), int(time.time())
        with r.h.db() as db:
            db.execute('INSERT INTO weixin_contexts(id,account_id,peer,generation,token,created_at,expires_at) '
                       'VALUES(?,?,?,?,NULL,?,?)', (context, 'synthetic', 'peer', 'generation', at, at + 300))
            db.execute("INSERT INTO weixin_outbox(id,run_id,account_id,peer,context_ref,credential_ref,generation,"
                       "client_id,content,status,deadline,error_code,server_message_id,created_at,updated_at) "
                       "VALUES(?,?,?,?,?,?,?, ?,NULL,'accepted',?,'original_response_too_large',?,?,?)",
                       (outbox, source, 'synthetic', 'peer', context, 'not-a-secret', 'generation',
                        str(uuid.uuid4()), at + 300, 'synthetic-weixin-receipt', at, at))
        r.setup('synthetic_weixin_oversize_fallback_acceptance_on_authenticated_local_source',
                actual_weixin_HTTP=False, weixin_adapter_admission_tested=False)
        following = r.submit(question, chat=chat)
        messages = r.messages(following)
        r.check('history_uses_only_actual_weixin_fallback_body',
                '回复超过当前微信文本限制，请缩小问题范围后重试。' in flat(messages, 'assistant')
                and long_marker not in flat(messages), ('must:0', 'must_not:0'))
        return
    source, outbox, parts = r.source_parts(2)
    if case != 'D03':
        if case == 'D05':
            r.receipt(outbox, payload={'type': 'image', 'asset_id': str(uuid.uuid4()), 'text': '候选图说明'},
                      error='媒体发送失败后已改发文字；原因：synthetic_upload_failure')
        elif case == 'D06':
            r.receipt(outbox, payload={'type': 'voice', 'asset_id': str(uuid.uuid4()), 'text': parts[0]})
        else:
            r.receipt(outbox)
    if case in ('D02', 'D07'):
        r.receipt(outbox, seq=2, status='unknown', error='synthetic_result_unknown')
    if case == 'D03':
        with r.h.db() as db:
            db.execute('UPDATE outbox SET deadline=? WHERE id=?', (int(time.time()) - 2, outbox['id']))
        r.setup('real_historical_deadline_elapsed', effective_deadline=int(time.time()) - 2)
        batch.wait_for(lambda: r.scalar('SELECT status FROM outbox WHERE id=?', (outbox['id'],)) == 'expired',
                       'sender_did_not_expire_parts')
    if case == 'D07':
        following = r.held(question, user=r.user())
        before = coord.snapshot(r.h, following)
        r.receipt(outbox, seq=2)
        r.check('late_receipt_does_not_rewrite_inflight_prompt',
                coord.snapshot(r.h, following) == before, ('must:0', 'must_not:0'))
        r.release(following)
        after = r.submit('SYNTHETIC_D07_AFTER_LATE_RECEIPT', user=r.user())
        newer = r.messages(after)
        r.check('new_snapshot_observes_late_receipt_while_frozen_snapshot_retains_old_evidence',
                parts[1] not in flat(json.loads(before['messages_json'])) and parts[1] in flat(newer, 'assistant'),
                ('must:0',))
        old_sources = r.scalar("SELECT snapshot_json FROM delivery_context_snapshots WHERE kind='run' AND target_id=?", (following,))
        if old_sources:
            source_evidence = next((item for item in json.loads(old_sources)['sources'] if item['run_id'] == source), None)
            current_revision = r.scalar('SELECT revision FROM delivery_context_revisions WHERE run_id=?', (source,))
            r.check('late_receipt_changes_source_revision_and_makes_old_derived_evidence_detectably_stale',
                    source_evidence is not None and source_evidence['delivery_revision'] != current_revision,
                    ('must:1',))
        else:
            r.note('derived_late_receipt_revision', 'prompt delivery evidence is not available under expected kind')
        r.note('summary_and_diary_regeneration_after_late_receipt',
               'no automatic summary/diary job is created by this scenario; source fencing only', ('must:1',))
        return
    following = r.submit(question)
    messages = r.messages(following)
    evidence = envelope(messages, 'DSH_DELIVERY_EVIDENCE_V1')
    if case in ('D01', 'D02'):
        r.check('only_confirmed_text_is_projected_as_assistant_history',
                parts[0] in flat(messages, 'assistant') and parts[1] not in flat(messages),
                ('must:0', 'must_not:0'))
        state = r.scalar('SELECT status FROM outbox_reply_parts WHERE outbox_id=? AND seq=2', (outbox['id'],))
        r.check('unattempted_or_unknown_state_is_preserved_without_replay',
                state == ('unknown' if case == 'D02' else 'queued'), ('must:1', 'must_not:0'), state=state)
    elif case == 'D03':
        r.check('expired_candidate_never_becomes_a_delivered_assistant_message',
                not any(part in flat(messages) for part in parts)
                and r.scalar('SELECT input FROM runs WHERE id=?', (source,)) in flat(messages),
                ('must:0', 'must_not:0'))
        r.check('all_expired_parts_stay_expired_without_new_credentials',
                all(row['status'] == 'expired' for row in r.rows(
                    'SELECT status FROM outbox_reply_parts WHERE outbox_id=?', (outbox['id'],))), ('must_not:1',))
    elif case == 'D05':
        r.check('failed_image_is_not_projected_as_successful_image_or_candidate_caption',
                '图片发送暂不可用。' in flat(messages, 'assistant')
                and '候选图说明' not in flat(messages, 'assistant'), ('must:0', 'must:1', 'must_not:0'))
    elif case == 'D06':
        r.check('accepted_voice_without_transcription_does_not_become_verified_spoken_text',
                parts[0] not in flat(messages, 'assistant')
                and bool(evidence), ('must:0', 'must_not:0'))


def references(r, fixture):
    case, question = fixture['id'], fixture['turns'][-1]['text']
    if case == 'R06':
        group = 'synthetic_group_' + r.suffix
        first = r.submit(fixture['turns'][0]['text'], user='speaker_A_' + r.suffix, group=group)
        last = r.submit(question, user='speaker_B_' + r.suffix, group=group)
        context = r.focus(last)
        r.check('group_B_does_not_inherit_group_A_local_detail_preference',
                context is not None and context.get('response_preference') is None, ('must:0', 'must_not:0'))
        bindings = r.rows('SELECT binding_json FROM run_conversation_bindings WHERE run_id IN (?,?)', (first, last))
        r.check('group_scope_is_saved_from_real_signed_inbound_evidence', len(bindings) == 2, ('must:0',))
        return
    chat = r.chat()
    if case == 'R01':
        source = r.submit('给阳台的薄荷换盆，先确认花盆和土壤。', chat=chat)
        run = r.submit(question, chat=chat)
        context = r.focus(run)
        r.check('unique_focus_continuation_resolves_to_real_original_topic',
                context is not None and context['continuation']
                and context['current_focus'] is not None
                and context['current_focus']['source_run_id'] == source,
                ('must:0',), actual_intent=context.get('input_intent') if context else None)
    elif case == 'R02':
        a = r.submit('周末行程去哪里，需要比较交通。', chat=chat)
        b = r.submit('电脑采购方案需要比较内存和存储。', chat=chat)
        run = r.submit(question, chat=chat)
        context = r.focus(run)
        r.check('ambiguous_deictic_input_does_not_silently_choose_a_topic',
                context is not None and context['status'] == 'ambiguous'
                and context['requires_clarification'] and context['current_focus'] is None
                and {item['source_run_id'] for item in context['alternatives']} >= {a, b},
                ('must:0', 'must_not:0'), actual_status=context.get('status') if context else None)
    elif case == 'R03':
        at = int(time.time())
        historical_at = at - 24 * 3600
        # Occupy the actual sole model slot, so the source's synthetic historical
        # timestamp is installed BEFORE its identity/context is first frozen.
        barrier_chat = r.chat('R03历史来源准备屏障')
        barrier = r.held('SYNTHETIC_R03_PRE_ADMISSION_BARRIER_' + r.suffix, chat=barrier_chat)
        source = r.submit('木桌尺寸是长120厘米、宽60厘米。', chat=chat, wait=False)
        with r.h.db() as db:
            db.execute('UPDATE runs SET created_at=? WHERE id=?', (historical_at, source))
            db.execute('UPDATE messages SET created_at=? WHERE run_id=?', (historical_at, source))
        r.release(barrier, source)
        with r.h.db() as db:
            db.execute('UPDATE conversation_state_entities SET expires_at=? WHERE id=?',
                       (int(time.time()) - 1, 'focus:' + source))
        r.setup('temporary_focus_TTL_expired_with_real_historical_timestamps_without_host_clock_change',
                source_run_ref=source, historical_user_time=historical_at, source_text_and_owner_unchanged=True)
        run = r.submit(question, chat=chat)
        context, messages = r.focus(run), r.messages(run)
        r.check('expired_focus_does_not_fabricate_cross_day_state_but_readable_history_survives',
                context is not None and (context.get('current_focus') or {}).get('source_run_id') != source
                and '长120厘米、宽60厘米' in flat(messages), ('must:0', 'must_not:0'))
        r.note('actual_midnight_clock_transition',
               'process uses real host time; TTL elapsed is tested, cross-day at APIs belongs to unit tests')
    elif case in ('R04', 'R05'):
        marker = 'B_ONLY_482'
        other = r.submit('同名人物合成爱好 ' + marker, user='speaker_B_' + case + '_' + r.suffix)
        other_chat = r.scalar('SELECT conversation_id FROM runs WHERE id=?', (other,))
        other_memory = r.memory(other_chat, '同名人物只属于B的合成爱好 ' + marker)
        if case == 'R05':
            people = []
            for source in (chat, other_chat):
                person, _, _ = r.h.request('POST', API + '/relationships/people', {
                    'display_name': '合成同名人物', 'visibility': 'private',
                    'source_conversation_id': source, 'publish': False,
                    'reason': '仅隔离回放的合成同名资料'}, expected=201)
                people.append(person)
            r.setup('authenticated_distinct_private_people_with_equal_display_names',
                    person_refs=[p['id'] for p in people])
            r.check('equal_display_name_does_not_merge_person_ID_or_source_owner',
                    people[0]['id'] != people[1]['id']
                    and people[0]['source_owner_id'] != people[1]['source_owner_id'], ('must_not:0',))
        if case == 'R04':
            r.submit('正在整理工作计划。', chat=chat)
        run = r.submit(question, chat=chat)
        messages = r.messages(run)
        r.check('private_other_owner_material_never_enters_A_actual_model_HTTP',
                marker not in flat(messages), ('must:0', 'must_not:0'), private_source_run_ref=other)
        if case == 'R04':
            context = r.focus(run)
            r.check('current_topic_switch_does_not_resolve_to_previous_work_topic',
                    context is not None and (context.get('current_focus') or {}).get('source_run_id') == run,
                    ('must_not:0',))
        r.h.request('DELETE', API + '/memory/' + other_memory['id'],
                    headers={'If-Match': '"' + str(other_memory['revision']) + '"'})
    elif case == 'R07':
        marker = 'FORGOTTEN_DERIVED_BODY_' + r.suffix
        source = r.submit(marker, chat=chat)
        record = r.memory(chat, marker)
        r.submit('等等，改成后天', chat=chat)
        commitment, _ = r.proposal(chat, source, marker)
        r.h.request('DELETE', API + '/memory/' + record['id'],
                    headers={'If-Match': '"' + str(record['revision']) + '"'})
        r.setup('authenticated_explicit_memory_forget_invalidates_conversation_derived_sources',
                memory_ref=record['id'], original_chat_physical_deletion_claimed=False)
        run = r.submit(question, chat=chat)
        messages = r.messages(run)
        r.check('forgotten_content_cannot_return_via_focus_correction_commitment_or_history',
                marker not in flat(messages), ('must:0', 'must_not:0'))
        r.check('invalidated_derived_body_is_cleared_in_storage',
                not r.rows('SELECT 1 FROM conversation_state_entities WHERE content_json LIKE ?', ('%' + marker + '%',))
                and not r.rows('SELECT 1 FROM conversation_commitments WHERE intent LIKE ?', ('%' + marker + '%',)),
                ('must:0',))


def budgets(r, fixture):
    case = fixture['id']
    chat = r.chat()
    if case in ('S01', 'S02'):
        question = fixture['turns'][0]['text']
        run = r.submit(question, chat=chat)
        messages, budget = r.messages(run), r.budget(run)
        r.check('complete_current_question_reaches_model_without_host_truncation',
                messages[-1] == {'role': 'user', 'content': question}
                and next(b for b in budget['blocks'] if b['id'] == 'current_input')['selected'], ('must:0',))
        r.note('answer_correctness_and_expression', 'requires actual model output and human review', ('must:0', 'must_not:0'))
    elif case == 'S03':
        original = r.policy()
        try:
            r.submit('完整原始条件：箱子里有17枚零件。', chat=chat)
            r.submit('刚才那个数字是23，不是17。', chat=chat)
            r.policy({'max_input_bytes': 1, 'estimated_input_limit': 1})
            before = r.model.count()
            run = r.submit(fixture['turns'][0]['text'], chat=chat, wait=False)
            state = r.h.wait_run(run, via_api=False)
            budget = r.budget(run)
            attempted_current = any(r.model.question(payload) == fixture['turns'][0]['text']
                                    for payload in r.model.requests[before:])
            r.check('mandatory_overflow_is_durable_rejection_with_zero_provider_requests',
                    state['status'] == 'failed' and not budget['accepted']
                    and not attempted_current and not r.rows(
                        'SELECT 1 FROM run_prompt_snapshots WHERE run_id=?', (run,)), ('must:0', 'must:1', 'must_not:0'),
                    rejection_reason=budget['rejection_reason'])
        finally:
            r.restore_policy(original)
    elif case == 'S04':
        original = r.policy()
        note = None
        try:
            r.diary_policy(True)
            note, _, _ = r.h.request('POST', API + '/diary/events', {
                'content': '最后改过的参数 OPTIONAL_S04_DIARY_MARKER ' + '合成可选日记资料' * 130,
                'source_conversation_id': chat, 'happened_at': int(time.time()),
                'visibility': 'private'}, expected=201)
            today = datetime.datetime.now(datetime.timezone(datetime.timedelta(hours=8))).date().isoformat()
            entry, _, _ = r.h.request('POST', API + '/diary/consolidate', {
                'period': 'day', 'day': today, 'timezone': 'Asia/Shanghai',
                'visibility': 'private', 'source_owner_id': r.h.owner})
            r.setup('actual_authorized_diary_entry_consolidated_from_explicit_synthetic_sources',
                    diary_ref=entry['id'], event_ref=note['id'], generator=entry['generator'])
            r.model.summary_text = 'SYNTHETIC_OPTIONAL_S04_SUMMARY ' + '重复的合成摘要资料' * 150
            for index in range(10):
                r.submit('合成历史资料第' + str(index) + '项：最后改过的参数尚未确认。', chat=chat)
            batch.wait_for(lambda: r.scalar('SELECT count(*) FROM conversation_summaries '
                                           'WHERE conversation_id=? AND content IS NOT NULL AND invalidated_at IS NULL', (chat,)),
                           'real_summary_worker_did_not_persist_fixture')
            r.setup('actual_summary_worker_completed_loopback_generation_with_source_fencing', conversation_ref=chat)
            r.submit('原问题参数是200，要求使用完整条件。', chat=chat)
            r.submit('刚才那个数字是300，不是200。', chat=chat)
            probe = r.submit('SYNTHETIC_S04_BASELINE_PROBE', chat=chat)
            budget = r.budget(probe)
            mandatory = sum(block['serialized_bytes'] for block in budget['blocks'] if block['required'])
            # The policy can shrink only the local budget. The margin includes
            # actual request framing; the optional record is still much larger.
            limit = min(95_000, mandatory + 1500)
            r.policy({'max_input_bytes': limit, 'estimated_input_limit': limit})
            run = r.submit(fixture['turns'][0]['text'], chat=chat)
            chosen, messages = r.budget(run), r.messages(run)
            dropped = [b for b in chosen['blocks'] if not b['selected']]
            r.check('optional_blocks_are_dropped_whole_with_explicit_decisions',
                    chosen['accepted'] and bool(dropped)
                    and any(b['reason'] in ('input_bytes_exceeded', 'estimated_input_budget_exceeded') for b in dropped)
                    and any(b['kind'] in ('diary', 'summary') and b['reason'] in (
                        'input_bytes_exceeded', 'estimated_input_budget_exceeded') for b in dropped),
                    ('must:0', 'must_not:0'))
            r.check('budget_really_considered_both_persisted_diary_and_summary_sources',
                    {'diary', 'summary'}.issubset({block['kind'] for block in chosen['blocks']}), ('must:0',))
            r.check('current_input_and_available_correction_remain_complete_required_blocks',
                    messages[-1]['content'] == fixture['turns'][0]['text']
                    and all(b['selected'] for b in chosen['blocks'] if b['required']), ('must:1',))
        finally:
            r.restore_policy(original)
            r.model.summary_text = 'SYNTHETIC_REPLAY_SUMMARY'
            if note:
                r.h.request('DELETE', API + '/diary/events/' + note['id'],
                            headers={'If-Match': '"' + str(note['revision']) + '"'})
            r.diary_policy(False)
    elif case == 'S05':
        before = r.action_counts()
        first = r.submit(fixture['turns'][0]['text'], chat=chat)
        second = r.submit(fixture['turns'][1]['text'], chat=chat)
        context = r.focus(second)
        r.check('local_detail_request_is_not_a_saved_global_preference',
                context is not None and context.get('response_preference') is None,
                ('must_not:0',))
        r.no_actions(before, ('must_not:0',))
        r.note('proof_detail_then_short_arithmetic', 'requires actual model output and human review', ('must:0',))
    elif case == 'S06':
        run = r.submit(fixture['turns'][0]['text'], chat=chat)
        budget = r.budget(run)
        r.check('snapshot_explicitly_labels_unknown_window_and_non_exact_versioned_estimate',
                budget['policy']['provider_context_window'] is None
                and budget['policy']['window_basis'] == 'unknown'
                and budget['token_counts_exact'] is False
                and budget['estimator_version'] == 'utf8_serialized_bytes_v1', ('must:0', 'must_not:0'))
        r.check('scripted_provider_usage_is_separate_from_host_estimated_input',
                budget['estimated_input_tokens'] != 8 and budget['actual_response_tokens'] == 1024,
                ('must:0',), synthetic_provider_usage={'prompt_tokens': 8, 'completion_tokens': 8})


def emotional(r, fixture):
    case = fixture['id']
    chat = r.chat()
    if case == 'E03':
        source = r.submit('买书预算是200。', chat=chat)
        correction = r.submit(fixture['turns'][0]['text'], chat=chat)
        following = r.submit(fixture['turns'][1]['text'], chat=chat)
        messages, context = r.messages(following), r.focus(following)
        r.check('literal_correction_remains_linked_to_original_source_for_later_turn',
                context is not None and any(item['target_run_id'] == source
                    and item['correction_run_id'] == correction and '300' in item['correction_text']
                    for item in context['corrections']), ('must:0',))
        r.check('original_and_corrected_numbers_are_auditable_without_mutating_original_input',
                r.scalar('SELECT input FROM runs WHERE id=?', (source,)) == '买书预算是200。'
                and fixture['turns'][0]['text'] in flat(messages), ('must:0',))
        r.note('model_continues_using_300_instead_of_200', 'requires actual model output and human review', ('must_not:0',))
        return
    before = r.action_counts()
    run = r.submit(fixture['turns'][0]['text'], chat=chat)
    messages = r.messages(run)
    if case == 'E06':
        value, _ = r.proposal(chat, run, '明天阅读装书架资料', confirmed=True)
        later = r.submit('SYNTHETIC_E06_CONFIRMATION_STATUS_PROBE', chat=chat)
        context = envelope(r.messages(later), 'DSH_CONVERSATION_COMMITMENTS_V1')
        chosen = next((item for item in (context or {}).get('items', []) if item['id'] == value['id']), None)
        confirmation = value.get('confirmation_evidence') or {}
        r.check('human_confirmation_is_distinct_from_unscheduled_execution_and_recipient_permission',
                value['confirmation_state'] == 'confirmed' and value['execution']['state'] == 'unscheduled'
                and value['recipient_permission'] is False
                and confirmation.get('basis') == 'authenticated_management_review'
                and confirmation.get('source_assent_verified') is False,
                ('must:0', 'must:1', 'must_not:0'), source_speaker_assent_tested=False)
        r.check('actual_prompt_contains_only_proven_commitment_state',
                chosen is not None and chosen['revision'] == value['revision']
                and chosen['confirmation_state'] == 'confirmed'
                and chosen['execution']['state'] == 'unscheduled'
                and chosen['recipient_permission'] is False
                and (chosen.get('confirmation_evidence') or {}).get('basis') == 'authenticated_management_review'
                and (chosen.get('confirmation_evidence') or {}).get('source_assent_verified') is False,
                ('must:0',), commitment_ref=value['id'])
        after = r.action_counts()
        r.check('management_review_creates_no_schedule_contact_or_autonomy_grant',
                all(after[key] == count for key, count in before.items() if key != 'conversation_commitments'),
                ('must:1', 'must_not:0'))
    else:
        r.no_actions(before, ('must_not:0',))
        r.check('current_emotional_or_technical_input_is_preserved_as_user_data',
                messages[-1]['content'] == fixture['turns'][0]['text'], ('must:0',))
        r.note('natural_empathy_disagreement_or_suggestion_wording',
               'requires actual model output and human review; canned loopback output is not semantic evidence',
               tuple('must:' + str(i) for i in range(len(fixture['must']))))


def closures(r, fixture):
    case = fixture['id']
    chat = r.chat()
    before = r.action_counts()
    if case == 'F04':
        r.submit('上周选择木材的合成来源记录。', chat=chat)
        # A temporary SQL fault models unavailable retrieval, without relabeling
        # it as an empty successful search. No source data or clock is changed.
        with r.h.db() as db:
            db.execute('ALTER TABLE memory_records RENAME TO replay_unavailable_memory_records')
        r.setup('temporary_retrieval_storage_unavailable_fault', actual_production_fault=False)
        try:
            count = r.model.count()
            run = r.submit(fixture['turns'][0]['text'], chat=chat, wait=False)
            value = r.h.wait_run(run, via_api=False)
            attempted_current = any(r.model.question(payload) == fixture['turns'][0]['text']
                                    for payload in r.model.requests[count:])
            r.check('retrieval_failure_stops_provider_instead_of_claiming_successful_no_match',
                    value['status'] == 'failed' and not attempted_current, ('must:0', 'must:1', 'must_not:0'),
                    actual_status=value['status'])
        finally:
            with r.h.db() as db:
                db.execute('ALTER TABLE replay_unavailable_memory_records RENAME TO memory_records')
        return
    if case == 'F06':
        source = r.submit('请你阅读合成资料，尚未执行。', chat=chat)
        value, _ = r.proposal(chat, source, '阅读合成资料', confirmed=True)
        r.h.request('POST', API + '/task-triggers/clock/acknowledge',
                    {'observed_now': int(time.time()), 'confirmed': True})
        resource, _, _ = r.h.request('POST', API + '/schedule/resources',
            {'title': '回放合成资料', 'content': '仅用于隔离回放。', 'type': 'text'})
        at = int(time.time())
        activity, _, _ = r.h.request('POST', API + '/schedule/activities', {
            'title': '回放未来阅读', 'type': 'reading', 'purpose': '计划不等于完成',
            'start_at': at + 3600, 'end_at': at + 3900, 'time_zone': 'Asia/Shanghai',
            'resource_id': resource['id'], 'repeat': None, 'target_ref': None,
            'movable': False, 'grant_id': None}, headers={'Idempotency-Key': str(uuid.uuid4())})
        root = API + '/conversations/' + chat + '/commitments/' + value['id']
        bound, _, _ = r.h.request('POST', root + '/activity?run_id=' + source,
            {'activity_id': activity['id'], 'activity_revision': activity['revision']},
            headers={'Idempotency-Key': str(uuid.uuid4()), 'If-Match': '"' + str(value['revision']) + '"'})
        r.setup('authorized_real_future_schedule_activity_bound_without_execution', activity_ref=activity['id'])
        run = r.submit(fixture['turns'][0]['text'], chat=chat)
        context = envelope(r.messages(run), 'DSH_CONVERSATION_COMMITMENTS_V1')
        chosen = next((item for item in (context or {}).get('items', []) if item['id'] == value['id']), None)
        execution = (chosen or {}).get('execution') or {}
        r.check('planned_activity_is_scheduled_without_execution_or_delivery_evidence',
                bound['execution']['state'] == 'scheduled'
                and bound['execution']['fulfillment_verified'] is False
                and bound['recipient_permission'] is False
                and chosen is not None and chosen['revision'] == bound['revision']
                and chosen['confirmation_state'] == 'confirmed'
                and execution.get('state') == 'scheduled'
                and execution.get('activity_id') == activity['id']
                and execution.get('current_activity_revision') == activity['revision']
                and execution.get('fulfillment_verified') is False
                and execution.get('completed_at') is None and execution.get('result_digest') is None
                and chosen['recipient_permission'] is False,
                ('must:0', 'must:1', 'must_not:0'), commitment_ref=value['id'], activity_ref=activity['id'])
        finished = r.scalar("SELECT count(*) FROM schedule_occurrences WHERE activity_id=? AND "
                            "(status IN ('running','partial','completed','unknown') OR started_at IS NOT NULL "
                            "OR completed_at IS NOT NULL OR result_json IS NOT NULL)", (activity['id'],))
        r.check('actual_persistent_activity_has_no_started_completed_or_result_occurrence',
                finished == 0 and r.scalar('SELECT state FROM schedule_activities WHERE id=?',
                                          (activity['id'],)) == 'enabled',
                ('must:0', 'must_not:0'))
        return
    runs = [r.submit(turn['text'], chat=None if case == 'F02' else chat)
            for turn in fixture['turns']]
    context, messages = r.focus(runs[-1]), r.messages(runs[-1])
    if case in ('F01', 'F02'):
        r.check('closure_is_expression_state_without_global_control_authority',
                context is not None and context['closed'], ('must:0', 'must_not:0'),
                actual_intent=context.get('input_intent') if context else None)
        r.no_actions(before, ('must_not:0',))
        if case == 'F02':
            owner = r.scalar('SELECT owner_id FROM runs WHERE id=?', (runs[-1],))
            r.check('signed_channel_speaker_has_no_admin_identity_or_global_sleep_grant',
                    owner.startswith('qq:') and not r.rows('SELECT 1 FROM admins WHERE id=?', (owner,)), ('must_not:0',))
    elif case == 'F03':
        recall = envelope(messages, 'DSH_RECALL_DECISION_V1')
        r.check('unknown_pet_has_no_visible_memory_match_or_invented_memory_record',
                recall is not None and recall['status'] == 'not_found'
                and not r.rows('SELECT 1 FROM memory_run_uses WHERE run_id=?', (runs[-1],)), ('must:0', 'must_not:0'))
        r.no_actions(before, ('must_not:0',))
    elif case == 'F05':
        prior = r.focus(runs[0])
        r.check('the_actual_prior_closure_input_was_observed_as_closed',
                prior is not None and prior['closed'], ('must:0',),
                actual_intent=prior.get('input_intent') if prior else None)
        r.check('substantive_new_question_reopens_focus_after_prior_closure',
                context is not None and not context['closed']
                and (context['current_focus'] or {}).get('source_run_id') == runs[-1]
                and messages[-1]['content'] == fixture['turns'][-1]['text'], ('must:0', 'must_not:0'))
        r.no_actions(before, ('must_not:0',))


DRIVERS = {'C': corrections, 'D': deliveries, 'R': references,
           'S': budgets, 'E': emotional, 'F': closures}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report', required=True)
    parser.add_argument('--fixtures', default=str(pathlib.Path(__file__).with_name('fixtures') / 'conversation-regressions-v1.json'))
    parser.add_argument('--openssl')
    parser.add_argument('--expected-commit', help='optional exact deployed commit to verify against health metadata')
    parser.add_argument('--cases', help='comma separated case IDs; omitted means all 40')
    args = parser.parse_args()
    if args.expected_commit and (len(args.expected_commit) != 40
                                 or any(c not in '0123456789abcdefABCDEF' for c in args.expected_commit)):
        parser.error('--expected-commit must be a full 40 character hexadecimal commit SHA')
    binary, web = pathlib.Path(args.binary).resolve(strict=True), pathlib.Path(args.web_dir).resolve(strict=True)
    fixture_path = pathlib.Path(args.fixtures).resolve(strict=True)
    fixture_bytes = fixture_path.read_bytes()
    fixtures = json.loads(fixture_bytes)
    cases = fixtures['cases']
    if fixtures['schema_version'] != 1 or [case['id'] for case in cases] != EXPECTED_IDS:
        raise ValueError('unsupported_or_reordered_versioned_fixture_manifest')
    selected = set(args.cases.split(',')) if args.cases else set(EXPECTED_IDS)
    if not selected or not selected.issubset(EXPECTED_IDS):
        raise ValueError('unknown_case_ID')
    original = fixtures['trusted_time']
    original_time = datetime.datetime.fromisoformat(original)
    effective = datetime.datetime.now(datetime.timezone(datetime.timedelta(hours=8)))
    report = {
        'schema_version': 1, 'kind': 'conversation fixture host replay', 'status': 'running',
        'started_at': base.utc_now(), 'fixture_sha256': digest(fixture_bytes),
        'runner_sha256': digest(pathlib.Path(__file__).read_bytes()),
        'fixture_schema_version': fixtures['schema_version'], 'binary_sha256': digest(binary.read_bytes()),
        'source_commit': None, 'expected_commit': args.expected_commit,
        'commit_verification': 'health_metadata_only_unless_expected_commit_is_supplied',
        'selected_case_ids': [c['id'] for c in cases if c['id'] in selected],
        'clock': {'mode': 'real_host_clock_with_relative_historical_fixture_times',
                  'original_anchor': original, 'effective_anchor': effective.isoformat(),
                  'shift_seconds': (effective - original_time).total_seconds(), 'time_zone': 'Asia/Shanghai',
                  'host_clock_modified': False, 'actual_midnight_transition_tested': False},
        'boundaries': {'production_data_accessed': False, 'real_model_tested': False,
                       'real_qq_HTTP_tested': False, 'real_weixin_HTTP_tested': False,
                       'client_delivery_tested': False, 'receipts_are_synthetic': True,
                       'loopback_output_is_semantic_evidence': False,
                       'model_self_grading_used': False},
        'cases': [],
    }
    model = Model()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    server.daemon_threads = True
    server.model = model
    threading.Thread(target=server.serve_forever, daemon=True).start()
    exit_code = 0
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-conversation-replay-') as temporary:
            h = base.Harness(str(binary), str(web), temporary, model)
            try:
                h.start()
                h.bootstrap()
                h.configure(server.server_port)
                report['source_commit'] = h.commit
                if args.expected_commit:
                    if h.commit != args.expected_commit:
                        raise AssertionError('health_commit_does_not_match_exact_expected_commit')
                    report['commit_verification'] = 'exact_expected_commit_matches_health_metadata'
                secret = secrets.token_urlsafe(32)
                signer = batch.Signer(batch.openssl_path(args.openssl), temporary, secret)
                credential, _, _ = h.request('POST', API + '/secrets', {'kind': 'qq_app_secret', 'value': secret})
                with h.db() as db:
                    db.execute('INSERT INTO reply_account_pacing(account_id,next_ms) VALUES(?,?)',
                               (batch.ACCOUNT, batch.PACING_FENCE_MS))
                config = copy.deepcopy(h.applied)
                # R03 prepares historical source timestamps before first
                # context freeze while an unrelated actual HTTP owns this
                # sole slot. Make this explicit instead of relying on the
                # production default (which permits up to two model leases).
                config['assistant']['max_concurrent_runs'] = 1
                config['qq'] = {'enabled': True, 'app_id': batch.ACCOUNT,
                                'credential_ref': credential['id'], 'transport': 'webhook'}
                batch.apply_config(h, config)
                replay = Replay(h, model, signer, report)
                report['configuration'] = {'model_id': h.applied['model']['model'],
                                            'provider': 'isolated_loopback_scripted',
                                            'initial_context_policy': replay.policy(),
                                            'seed_requested': False}
                report['setup'] = {'qq_pacing_fence_installed_before_enable': True,
                                   'model_concurrency_limit': 1,
                                   'real_channel_account_used': False}
                for fixture in cases:
                    if fixture['id'] not in selected:
                        continue
                    result = {'id': fixture['id'], 'category': fixture['category'],
                              'fixture_digest': digest(fixture), 'declared_setup': fixture['setup'],
                              'setup_evidence': [], 'run_refs': [],
                              'host': {'status': 'running', 'assertions': [], 'not_evaluated': []},
                              'expression': {'status': 'not_evaluated',
                                             'reason': 'scripted loopback output; requires real-model replay and human review'},
                              'fixture_criteria': [
                                  {'ref': kind + ':' + str(i), 'text': text,
                                   'expression_status': 'not_evaluated', 'host_assertions': []}
                                  for kind in ('must', 'must_not')
                                  for i, text in enumerate(fixture[kind])],
                              'started_at': base.utc_now()}
                    report['cases'].append(result)
                    replay.case = result
                    before = model.count()
                    try:
                        DRIVERS[fixture['id'][0]](replay, fixture)
                    except Exception as error:
                        result['host']['status'] = 'failed'
                        # Error types and controlled assertion labels only; no
                        # arbitrary response body, header or credential output.
                        result['host']['error_type'] = type(error).__name__
                        if isinstance(error, AssertionError):
                            result['host']['error_label'] = str(error)[:250]
                        print('FAIL ' + fixture['id'] + ' setup/execution ' + type(error).__name__, file=sys.stderr)
                    finally:
                        model.hold_release.set()
                    if result['host']['status'] == 'running':
                        assertions = result['host']['assertions']
                        result['host']['status'] = ('failed' if any(c['status'] == 'failed' for c in assertions)
                                                    else 'passed' if assertions else 'not_evaluated')
                    for criterion in result['fixture_criteria']:
                        criterion['host_assertions'] = [a['name'] for a in result['host']['assertions']
                                                       if criterion['ref'] in a['fixture_criteria']]
                        criterion['host_status'] = ('checked_host_facts_only' if criterion['host_assertions']
                                                     else 'not_evaluated')
                    result['provider_request_count'] = model.count() - before
                    result['finished_at'] = base.utc_now()
                platform_calls = replay.scalar('SELECT count(*) FROM qq_api_calls')
                fence = replay.scalar('SELECT next_ms FROM reply_account_pacing WHERE account_id=?', (batch.ACCOUNT,))
                report['outbound_safety'] = {'qq_api_call_count': platform_calls,
                                             'durable_fence_retained': fence == batch.PACING_FENCE_MS,
                                             'weixin_account_enabled': bool(replay.scalar('SELECT count(*) FROM weixin_account WHERE enabled=1'))}
                report['personality_metadata'] = replay.rows(
                    'SELECT DISTINCT character_digest,compiler_version FROM runs ORDER BY character_digest,compiler_version')
                if platform_calls or fence != batch.PACING_FENCE_MS or report['outbound_safety']['weixin_account_enabled']:
                    report['status'], exit_code = 'failed_outbound_safety', 1
                else:
                    failures = sum(c['host']['status'] == 'failed' for c in report['cases'])
                    report['status'], exit_code = ('host_failed', 1) if failures else ('host_replay_complete_expression_pending', 0)
            finally:
                h.close()
    except Exception as error:
        report.update(status='setup_failed', error_type=type(error).__name__)
        print('FAIL replay setup ' + type(error).__name__, file=sys.stderr)
        exit_code = 1
    finally:
        model.hold_release.set()
        server.shutdown()
        server.server_close()
        report['finished_at'] = base.utc_now()
        report['actual_model_request_parameters'] = [json.loads(item) for item in sorted({
            json.dumps({key: payload[key] for key in ('model', 'max_tokens', 'stream', 'temperature', 'seed') if key in payload},
                       sort_keys=True) for payload in model.requests})]
        report['summary'] = {'expected_fixture_count': 40, 'executed_case_count': len(report['cases']),
                             'host_passed': sum(c['host']['status'] == 'passed' for c in report['cases']),
                             'host_failed': sum(c['host']['status'] == 'failed' for c in report['cases']),
                             'host_not_evaluated': sum(c['host']['status'] == 'not_evaluated' for c in report['cases']),
                             'expression_passed': 0, 'expression_not_evaluated': len(report['cases']),
                             'host_assertions_passed': sum(a['status'] == 'passed' for c in report['cases'] for a in c['host']['assertions']),
                             'host_assertions_failed': sum(a['status'] == 'failed' for c in report['cases'] for a in c['host']['assertions']),
                             'host_checks_not_evaluated': sum(len(c['host']['not_evaluated']) for c in report['cases']),
                             'fixture_criteria_without_host_check': sum(not item['host_assertions'] for c in report['cases'] for item in c['fixture_criteria']),
                             'host_case_pass_means_only_recorded_assertions_passed': True,
                             'all_40_semantically_passed': False, 'provider_requests': model.count()}
        path = pathlib.Path(args.report)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    return exit_code


if __name__ == '__main__':
    raise SystemExit(main())
