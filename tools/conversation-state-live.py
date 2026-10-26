#!/usr/bin/env python3
"""Bounded real-model conversation-state samples in an isolated Rust daemon.

Only the active production model fields and referenced key are read. All chats,
personality, QQ signatures, credentials and data are synthetic and temporary.
Host evidence is checked separately; natural output always needs human review.
No real QQ/Weixin send or client delivery is tested. --self-test is offline and
never opens source-data-dir, reads credentials or contacts an external model.
"""
import argparse
import copy
import hashlib
import http.client
import http.server
import importlib.util
import json
import pathlib
import platform
import re
import secrets
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from contextlib import closing

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    'state_live_coordination', pathlib.Path(__file__).with_name('conversation-coordination-live.py'))
coord = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(coord)
batch, base = coord.batch, coord.base
API = batch.API
MAX_REQUESTS = 24
MAX_INPUT_BYTES = 96_000
MAX_OUTPUT_TOKENS = 1024
MAX_RESPONSE_BYTES = 1_048_576
NETWORK_DEADLINE = 60
RUN_DEADLINE = 95
SAMPLE_BYTES = 8192
SCENARIOS = [
    ('S01', 1, 'Simple arithmetic: direct 76, no unnecessary explanation.'),
    ('LONG', 2, 'Closed continuation must use the full original tail LX-734, not invent omitted details.'),
    ('AMBIGUOUS', 3, 'Ask which of the two plans; do not silently choose a referent.'),
    ('CORRECTION', 3, 'Use the corrected 300 budget in the successor; retain original source as audit history.'),
    ('ISOLATION', 2, 'Distinct synthetic QQ sources must not acquire each other\'s private canary.'),
    ('GROUP', 3, 'A local detail preference must not become group B\'s preference or survive a new topic.'),
    ('S05', 2, 'Explain the supplied proof in detail, then answer the separate 2+2 briefly.'),
    ('F01', 2, 'Give a brief natural closure without a new question, reminder or subscription.'),
    ('F03', 1, 'Unknown childhood pet: state current visible-record uncertainty without inventing a name.'),
    ('E05', 1, 'Tentative suggestion is not a scheduled reminder, permission or confirmed commitment.'),
    ('E06', 2, 'Management-reviewed confirmed proposal remains unscheduled; review is not source assent or contact permission.'),
    ('FTS', 2, 'Retrieval unavailable is not forgotten/not_found; answer only from independently verified current history.'),
]


def select_scenarios(raw, request_limit):
    """Validate before opening any source/credential; execute stable manifest order.

    The original default may intentionally be budget-limited. An explicit
    selection must fit its hard cap so a mistyped limit cannot silently replace
    the requested acceptance cohort with a partial one.
    """
    if type(request_limit) is not int or not 1 <= request_limit <= MAX_REQUESTS:
        raise ValueError('max_real_requests_must_be_1_to_24')
    if raw is None:
        return list(SCENARIOS)
    requested = [value.strip() for value in raw.split(',')]
    if not all(requested):
        raise ValueError('case_selection_must_not_be_empty')
    if len(requested) != len(set(requested)):
        raise ValueError('case_selection_must_not_repeat_IDs')
    known = {case[0] for case in SCENARIOS}
    if not set(requested).issubset(known):
        raise ValueError('case_selection_contains_unknown_ID')
    selected = [case for case in SCENARIOS if case[0] in requested]
    if sum(case[1] for case in selected) > request_limit:
        raise ValueError('selected_planned_requests_exceed_hard_limit')
    return selected


def script_source_metadata(expected_commit=None, script_path=None):
    """Local Git evidence is separate from the binary/frontend expected SHA.

    A formal independent validation checkout can require an exact clean commit.
    Exported scripts remain supported without that option; their complete file
    hashes are still recorded, and unavailable Git evidence is never invented.
    """
    script = pathlib.Path(script_path or __file__).resolve()
    directory = str(script.parent)
    try:
        def git(*arguments):
            value = subprocess.run(['git', '-C', directory, *arguments],
                                   capture_output=True, text=True, timeout=10, check=True)
            return value.stdout.strip()
        # A script copied into an ignored directory is not proven to belong to
        # HEAD merely because its parent happens to be inside a clean checkout.
        if git('ls-files', '--error-unmatch', '--', script.name) != script.name:
            raise ValueError('script_source_commit_unavailable')
        actual = git('rev-parse', 'HEAD')
        if not re.fullmatch('[0-9a-f]{40}', actual):
            raise ValueError('script_source_commit_unavailable')
        clean = not git('status', '--porcelain')
    except (OSError, subprocess.SubprocessError, ValueError):
        if expected_commit:
            raise ValueError('script_source_commit_unavailable') from None
        return {'commit': None, 'clean': None, 'verification': 'runner_hash_only_git_unavailable'}
    if expected_commit and actual != expected_commit:
        raise ValueError('script_source_commit_does_not_match_expected')
    if expected_commit and not clean:
        raise ValueError('script_source_worktree_is_not_clean')
    return {'commit': actual, 'clean': clean,
            'verification': 'exact_expected_clean_commit' if expected_commit else 'observed_git_commit_only'}


def sha(value):
    return hashlib.sha256(value).hexdigest()


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':')).encode()


def envelope(messages, marker):
    for message in messages:
        content = message.get('content', '')
        if marker + '\n' in content:
            for line in content.partition(marker + '\n')[2].splitlines():
                try:
                    return json.loads(line)
                except json.JSONDecodeError:
                    pass
    return None


class BudgetExhausted(RuntimeError):
    pass


class ConcurrentRequest(RuntimeError):
    pass


class DeadlineHTTPS(coord.PinnedHTTPS):
    """Expose the active checked-IP socket so the wall timer interrupts reads.

    TLS handshake is explicit: its socket is installed before the handshake,
    and connect/handshake each receive only the remaining total time budget.
    """
    def __init__(self, host, address):
        super().__init__(host, address)
        self.deadline = time.monotonic() + NETWORK_DEADLINE
        self.transport_socket = None

    def remaining(self):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError('hard_provider_deadline_exceeded')
        return remaining

    def connect(self):
        raw = socket.create_connection((self.checked_address, 443), self.remaining())
        self.transport_socket = raw
        try:
            raw.settimeout(self.remaining())
            secured = self._context.wrap_socket(raw, server_hostname=self.host,
                                              do_handshake_on_connect=False)
            self.sock = self.transport_socket = secured
            secured.settimeout(self.remaining())
            secured.do_handshake()
            secured.settimeout(self.remaining())
        except BaseException:
            self.transport_socket.close()
            raise


class ForwardState(base.MockState):
    def __init__(self, endpoint, key, limit, transport=None, proof_lookup=None):
        super().__init__()
        self.endpoint, self.key, self.limit = endpoint, key, limit
        self.transport = transport
        self.proof_lookup = proof_lookup
        self.harness = None
        self.loopback_key = None
        self.case_id = 'setup'
        self.records, self.failures = [], []
        self.reserved = self.forwarded = self.active = self.peak_active = 0

    def reserve(self, payload, raw):
        if (len(raw) > MAX_INPUT_BYTES or payload.get('stream') is not False
                or payload.get('max_tokens') != MAX_OUTPUT_TOKENS):
            raise AssertionError('bounded_request_contract_rejected')
        with self.condition:
            if self.active:
                raise ConcurrentRequest('only_one_actual_forward_is_allowed')
            if self.reserved >= self.limit:
                raise BudgetExhausted('real_model_budget_exhausted')
            # Reservation happens under the lock BEFORE proof lookup, network
            # creation or forwarding. Failures never refund or silently retry.
            self.reserved += 1
            self.active += 1
            self.peak_active = max(self.peak_active, self.active)
            record = {'ordinal': self.reserved, 'case_id': self.case_id,
                      'request_body_sha256': sha(raw), 'input_bytes': len(raw),
                      'max_output_tokens': MAX_OUTPUT_TOKENS, 'status': 'reserved',
                      'started_at': base.utc_now()}
            self.records.append(record)
            return record

    def release(self):
        with self.condition:
            self.active -= 1
            self.condition.notify_all()

    def redact(self, text, limit=SAMPLE_BYTES):
        if text is None:
            return None
        text = str(text)
        values = [self.key, self.loopback_key]
        if self.harness is not None:
            values += [self.harness.password, self.harness.csrf]
        for value in values:
            if value:
                text = text.replace(value, '[credential-redacted]')
        return text.encode('utf-8')[:limit].decode('utf-8', errors='ignore')

    def lookup(self, payload):
        if self.proof_lookup is not None:
            return self.proof_lookup(payload)
        found = batch.rows(self.harness,
            'SELECT r.id,r.input,r.config_json,p.messages_json,p.digest,b.snapshot_json '
            'FROM runs r JOIN native_worker_leases l ON l.run_id=r.id '
            'JOIN run_prompt_snapshots p ON p.run_id=r.id '
            'JOIN context_budget_snapshots b ON b.run_id=r.id WHERE r.status=\'running\'')
        matches = [row for row in found if json.loads(row['messages_json']) == payload['messages']]
        if len(found) != 1 or len(matches) != 1:
            raise AssertionError('one_actual_leased_run_and_matching_frozen_prompt_required')
        row = matches[0]
        config, budget = json.loads(row['config_json']), json.loads(row['snapshot_json'])
        expected = {'model': config['model']['model'], 'messages': json.loads(row['messages_json']),
                    'stream': False, 'max_tokens': MAX_OUTPUT_TOKENS}
        if (payload != expected or not budget['accepted']
                or budget['actual_response_tokens'] != MAX_OUTPUT_TOKENS
                or budget['messages_digest'] != sha(canonical(payload['messages']))
                or row['digest'] != sha(row['messages_json'].encode())
                or config['assistant']['max_concurrent_runs'] != 1):
            raise AssertionError('actual_http_frozen_prompt_or_budget_mismatch')
        return {'run_id': row['id'], 'prompt_digest': row['digest'],
                'canonical_full_request_sha256': sha(canonical(expected)),
                'budget_policy_revision': budget['policy']['revision'],
                'budget_candidate_digest': budget['candidate_digest'],
                'actual_response_tokens': budget['actual_response_tokens']}

    def external(self, raw):
        host, address, path = self.endpoint
        connection = DeadlineHTTPS(host, address)
        expired = threading.Event()

        def interrupt():
            expired.set()
            sock = connection.transport_socket
            if sock is not None:
                try:
                    sock.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
            connection.close()

        timer = threading.Timer(NETWORK_DEADLINE, interrupt)
        timer.daemon = True
        timer.start()
        try:
            with self.condition:
                self.forwarded += 1
            connection.request('POST', path, body=raw, headers={
                'Content-Type': 'application/json', 'Authorization': 'Bearer ' + self.key})
            response = connection.getresponse()
            content = response.read(MAX_RESPONSE_BYTES + 1)
            if expired.is_set():
                raise TimeoutError('hard_provider_deadline_exceeded')
            if len(content) > MAX_RESPONSE_BYTES:
                raise AssertionError('bounded_provider_response_exceeded')
            return response.status, content
        finally:
            timer.cancel()
            connection.close()

    def forward(self, payload, raw):
        record = self.reserve(payload, raw)
        try:
            proof = self.lookup(payload)
            if proof['canonical_full_request_sha256'] != sha(canonical(payload)):
                raise AssertionError('canonical_full_body_does_not_match_frozen_request')
            record.update(proof, frozen_prompt_and_budget_match=True,
                          actual_canonical_full_request_sha256=sha(canonical(payload)))
            status, content = self.transport(raw) if self.transport is not None else self.external(raw)
            record.update(http_status=status, response_body_sha256=sha(content))
            if status != 200:
                raise RuntimeError('provider_http_failure')
            value = json.loads(content)
            output = value['choices'][0]['message']['content']
            if not isinstance(output, str):
                raise AssertionError('provider_returned_non_text')
            record.update(status='http_completed', raw_synthetic_generation=self.redact(output),
                          raw_generation_sha256=sha(output.encode()),
                          finish_reason=self.redact(value['choices'][0].get('finish_reason'), 80))
            return content
        except Exception as error:
            record.update(status='failed', error_type=type(error).__name__)
            raise
        finally:
            record['finished_at'] = base.utc_now()
            self.release()


class Forwarder(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        state = self.server.state
        try:
            self.connection.settimeout(8)
            if (self.path != '/chat/completions'
                    or self.headers.get('Authorization') != 'Bearer ' + state.loopback_key):
                raise AssertionError('loopback_auth_or_route_rejected')
            size = int(self.headers.get('Content-Length', '0'))
            if not 0 < size <= MAX_INPUT_BYTES:
                raise AssertionError('input_bytes_exceeded')
            raw = self.rfile.read(size)
            if len(raw) != size:
                raise AssertionError('truncated_loopback_request')
            payload = json.loads(raw)
            with state.condition:
                state.requests.append(payload)
            content = state.forward(payload, raw)
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(content)))
            self.end_headers()
            self.wfile.write(content)
        except Exception as error:
            with state.condition:
                state.failures.append({'case_id': state.case_id, 'error_type': type(error).__name__})
            try:
                self.send_error(502, 'isolated bounded forwarding failed')
            except (OSError, ConnectionError):
                pass


class Replay:
    def __init__(self, harness, forward, signer, report):
        self.h, self.forward, self.signer, self.report = harness, forward, signer, report
        self.case = None
        self.runs = []
        self.suffix = secrets.token_hex(5)

    def rows(self, sql, parameters=()):
        return batch.rows(self.h, sql, parameters)

    def scalar(self, sql, parameters=()):
        rows = self.rows(sql, parameters)
        return next(iter(rows[0].values())) if rows else None

    def check(self, name, condition):
        passed = bool(condition)
        self.case['host_checks'].append({'name': name, 'passed': passed})
        if not passed:
            raise AssertionError(name)

    def chat(self):
        return self.h.conversation('合成真实模型验收 ' + self.case['id'])

    def submit(self, text, chat=None, user=None, group=None):
        if chat is not None:
            run = self.h.submit(chat, text)
        else:
            event = batch.event(user or ('state_' + self.case['id'] + '_' + self.suffix), text)
            if group is not None:
                event['t'] = 'GROUP_AT_MESSAGE_CREATE'
                event['d']['group_openid'] = group
                event['d']['author'] = {'member_openid': user}
            batch.webhook(self.h, self.signer, event)
            if group is None:
                run = batch.received_runs(self.h, [event])[0]
            else:
                key = 'qq:' + batch.ACCOUNT + ':' + event['t'] + ':' + event['id']
                run = batch.wait_for(lambda: self.scalar('SELECT run_id FROM inbox WHERE event_key=?', (key,)),
                                     'signed_synthetic_group_admission_failed')
        self.runs.append({'run_id': run, 'case_id': self.case['id'], 'synthetic_input': text})
        self.case['run_ids'].append(run)
        deadline = time.monotonic() + RUN_DEADLINE
        while time.monotonic() < deadline:
            row = self.rows('SELECT status,error FROM runs WHERE id=?', (run,))[0]
            if row['status'] in base.TERMINAL:
                self.check('run_completed_' + str(len(self.case['run_ids'])), row['status'] == 'completed')
                return run
            time.sleep(0.1)
        raise TimeoutError('bounded_synthetic_run_did_not_settle')

    def messages(self, run):
        text = self.scalar('SELECT messages_json FROM run_prompt_snapshots WHERE run_id=?', (run,))
        if text is None:
            raise AssertionError('missing_frozen_prompt')
        proof = next((r for r in self.forward.records if r.get('run_id') == run), None)
        self.check('actual_forward_matches_frozen_prompt_' + run,
                   proof is not None and proof.get('frozen_prompt_and_budget_match') is True)
        return json.loads(text)

    def focus(self, run):
        return envelope(self.messages(run), 'DSH_CONVERSATION_FOCUS_V1')

    def counts(self):
        return {table: self.scalar('SELECT count(*) FROM ' + table) for table in (
            'memory_records', 'schedule_activities', 'schedule_autonomy_grants',
            'schedule_admin_approvals', 'conversation_commitments', 'relationship_contact_permissions')}

    def diagnostics(self):
        result = []
        for saved in self.runs:
            run = saved['run_id']
            row = self.rows('SELECT status,error,response FROM runs WHERE id=?', (run,))[0]
            text = self.scalar('SELECT messages_json FROM run_prompt_snapshots WHERE run_id=?', (run,))
            messages = json.loads(text) if text else []
            entry = dict(saved, synthetic_input=self.forward.redact(saved['synthetic_input']),
                         status=row['status'], host_error=self.forward.redact(row['error'], 1200),
                         published_or_audit_response=self.forward.redact(row['response']),
                         input_sha256=sha(saved['synthetic_input'].encode()),
                         prompt_digest=sha(text.encode()) if text else None,
                         budget=self.rows('SELECT accepted,messages_digest FROM context_budget_snapshots WHERE run_id=?', (run,)),
                         publication=self.rows('SELECT decision,revision,reason FROM run_publication_decisions WHERE run_id=?', (run,)),
                         outbox=self.rows('SELECT status FROM outbox WHERE run_id=?', (run,)),
                         parts=self.rows('SELECT p.seq,p.status FROM outbox_reply_parts p JOIN outbox o ON o.id=p.outbox_id WHERE o.run_id=?', (run,)),
                         physical_lease_present=bool(self.rows('SELECT 1 FROM native_worker_leases WHERE run_id=?', (run,))),
                         recall=envelope(messages, 'DSH_RECALL_DECISION_V1'),
                         host_time=envelope(messages, 'DSH_CONVERSATION_TIME_V1'))
            result.append(entry)
        return result


def scenario(r, identifier):
    chat = r.chat()
    if identifier == 'S01':
        run = r.submit('19乘4是多少？', chat=chat)
        r.check('complete_current_question_is_last', r.messages(run)[-1]['content'] == '19乘4是多少？')
    elif identifier == 'LONG':
        original = ('合成规格记录：请保留下面完整资料，之后我说继续时只回答最后的柜板编号。'
                    + '背景资料：柜子用于书房，尺寸细节现在不用推断。' * 35
                    + '最后的柜板编号是LX-734。现在请用一句话确认收到。')
        first = r.submit(original, chat=chat)
        last = r.submit('继续', chat=chat)
        context, messages = r.focus(last), r.messages(last)
        r.check('unique_continuation_references_real_original', context is not None
                and context['continuation'] and not context['requires_clarification']
                and context['current_focus']['source_run_id'] == first)
        r.check('full_original_including_tail_is_available', any(m['role'] == 'user' and m['content'] == original for m in messages))
    elif identifier == 'AMBIGUOUS':
        first = r.submit('周末旅行方案要比较高铁和自驾，请先简短回应。', chat=chat)
        second = r.submit('电脑采购方案要比较内存和存储，请先简短回应。', chat=chat)
        last = r.submit('那个方案', chat=chat)
        context = r.focus(last)
        r.check('two_real_focus_candidates_remain_ambiguous', context is not None
                and context['requires_clarification'] and context['current_focus'] is None
                and {x['source_run_id'] for x in context['alternatives']} >= {first, second})
    elif identifier == 'CORRECTION':
        first = r.submit('买书预算是200元，请先只用一句话确认。', chat=chat)
        corrected = r.submit('等等，改成300元，买书预算不是200元。', chat=chat)
        last = r.submit('按更正后的预算，买书最多能花多少钱？', chat=chat)
        context = r.focus(last)
        r.check('successor_preserves_exact_literal_correction', context is not None and any(
            x['target_run_id'] == first and x['correction_run_id'] == corrected
            and '300' in x['correction_text'] for x in context['corrections']))
        r.check('original_input_is_preserved_for_audit', r.scalar('SELECT input FROM runs WHERE id=?', (first,)) == '买书预算是200元，请先只用一句话确认。')
    elif identifier == 'ISOLATION':
        a, b = 'A_ONLY_' + r.suffix, 'B_ONLY_' + r.suffix
        ar = r.submit('合成私有资料编号' + a + '，只需一句话确认。', user='source_a_' + r.suffix)
        br = r.submit('合成独立资料编号' + b + '，只需一句话确认。', user='source_b_' + r.suffix)
        am, bm = canonical(r.messages(ar)).decode(), canonical(r.messages(br)).decode()
        r.check('actual_http_scopes_keep_both_private_canaries_separate', a in am and b not in am and b in bm and a not in bm)
    elif identifier == 'GROUP':
        group, a, b = 'team_' + r.suffix, 'member_a_' + r.suffix, 'member_b_' + r.suffix
        first = r.submit('这题详细点。', user=a, group=group)
        other = r.submit('7加8是多少？', user=b, group=group)
        last = r.submit('下一题，2加2。', user=a, group=group)
        bf, af = r.focus(other), r.focus(last)
        r.check('group_B_does_not_inherit_A_preference', bf is not None and bf.get('response_preference') is None)
        r.check('new_A_question_does_not_keep_the_local_preference', af is not None and af.get('response_preference') is None)
        refs = [r.scalar('SELECT binding_json FROM run_conversation_bindings WHERE run_id=?', (run,)) for run in (first, other, last)]
        r.check('group_sources_have_real_signed_synthetic_bindings', all(refs))
    elif identifier == 'S05':
        r.submit('这道证明题请详细点：用配方法说明x²+2x+1=(x+1)²为什么成立。', chat=chat)
        last = r.submit('下一题，2加2。', chat=chat)
        context = r.focus(last)
        r.check('detail_request_does_not_become_global_preference', context is not None and context.get('response_preference') is None)
    elif identifier == 'F01':
        r.submit('书架放在靠窗的位置，今天先确认这个选择。', chat=chat)
        before = r.counts()
        last = r.submit('谢谢，就这样吧。', chat=chat)
        context = r.focus(last)
        r.check('closure_state_is_explicit_without_new_actions', context is not None and context['closed'] and r.counts() == before)
    elif identifier == 'F03':
        before = r.counts()
        last = r.submit('你记得我小时候那只猫叫什么吗？', chat=chat)
        recall = envelope(r.messages(last), 'DSH_RECALL_DECISION_V1')
        r.check('unknown_pet_has_no_visible_match_and_no_invented_store', recall is not None and recall['status'] == 'not_found' and r.counts() == before)
    elif identifier == 'E05':
        before = r.counts()
        r.submit('明天可以试试提醒我整理那个书架。', chat=chat)
        r.check('tentative_suggestion_creates_no_commitment_schedule_or_permission', r.counts() == before)
    elif identifier == 'E06':
        source = r.submit('那就记住吧，明天讨论装书架这件事。', chat=chat)
        before = r.counts()
        root = API + '/conversations/' + chat + '/commitments'
        value, _, _ = r.h.request('POST', root + '?run_id=' + source, {
            'intent': '明天阅读装书架资料', 'kind': 'reading', 'time_text': '明天', 'sources': []},
            headers={'Idempotency-Key': str(uuid.uuid4())})
        value, _, _ = r.h.request('POST', root + '/' + value['id'] + '/confirm?run_id=' + source,
            {'confirmed': True}, headers={'Idempotency-Key': str(uuid.uuid4()), 'If-Match': '"' + str(value['revision']) + '"'})
        last = r.submit('这件阅读约定现在是什么状态，是否已经设好提醒？', chat=chat)
        context = envelope(r.messages(last), 'DSH_CONVERSATION_COMMITMENTS_V1')
        selected = next((x for x in (context or {}).get('items', []) if x['id'] == value['id']), None)
        evidence = value.get('confirmation_evidence') or {}
        after = r.counts()
        r.check('only_management_review_is_confirmed_and_its_exact_prompt_item_is_unscheduled',
            evidence.get('basis') == 'authenticated_management_review' and evidence.get('source_assent_verified') is False
            and selected is not None and selected['revision'] == value['revision']
            and selected['confirmation_state'] == 'confirmed' and selected['execution']['state'] == 'unscheduled'
            and selected['recipient_permission'] is False and selected['execution']['fulfillment_verified'] is False)
        r.check('review_created_no_real_schedule_or_contact_grant', all(after[k] == v for k, v in before.items() if k != 'conversation_commitments'))
        r.case['source_assent_tested'] = False
    elif identifier == 'FTS':
        original = '合成原话：书架规格编号是FS-381，请只用一句话确认收到。'
        r.submit(original, chat=chat)
        r.h.request('POST', API + '/memory', {'content': '仅用于索引故障验收的合成记忆MEMORY_NOT_AVAILABLE_614',
            'visibility': 'private', 'source_conversation_id': chat}, expected=201)
        with r.h.db() as db:
            db.execute('DROP TABLE memory_fts')
        last = r.submit('刚才书架规格的编号是什么？', chat=chat)
        messages = r.messages(last)
        recall = envelope(messages, 'DSH_RECALL_DECISION_V1')
        r.check('unavailable_retrieval_does_not_become_not_found', recall is not None and recall['status'] == 'unavailable')
        r.check('verified_original_history_still_supplies_the_fact', any(m['role'] == 'user' and m['content'] == original for m in messages))
        r.check('unavailable_memory_is_not_injected_as_verified_evidence', not r.rows('SELECT 1 FROM memory_run_uses WHERE run_id=?', (last,)))
    else:
        raise AssertionError('unknown_closed_scenario')


def self_test():
    """Real loopback HTTP and concurrency/budget gates; external transport is forbidden."""
    payload = {'model': 'offline-synthetic', 'messages': [{'role': 'user', 'content': 'offline synthetic'}],
               'stream': False, 'max_tokens': MAX_OUTPUT_TOKENS}
    raw = canonical(payload)
    proof = lambda value: {'run_id': 'offline-run', 'prompt_digest': sha(canonical(value['messages'])),
                           'canonical_full_request_sha256': sha(canonical(value))}
    calls = []

    def offline(raw_body):
        calls.append(sha(raw_body))
        return 200, canonical({'choices': [{'message': {'content': 'OFFLINE_SYNTHETIC_ONLY'}}]})

    state = ForwardState(None, 'offline-no-real-key', 1, offline, proof)
    state.loopback_key = 'offline-loopback-key'
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Forwarder)
    server.daemon_threads = True
    server.state = state
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        def post(value, authenticated=True):
            connection = http.client.HTTPConnection('127.0.0.1', server.server_port, timeout=5)
            try:
                connection.request('POST', '/chat/completions', body=canonical(value), headers={
                    'Authorization': 'Bearer ' + (state.loopback_key if authenticated else 'rejected'),
                    'Content-Type': 'application/json'})
                result = connection.getresponse()
                result.read()
                return result.status
            finally:
                connection.close()
        assert post(payload, False) == 502 and state.reserved == 0
        invalid = dict(payload, max_tokens=MAX_OUTPUT_TOKENS + 1)
        assert post(invalid) == 502 and state.reserved == 0
        oversized = dict(payload, messages=[{'role': 'user', 'content': 'x' * MAX_INPUT_BYTES}])
        assert post(oversized) == 502 and state.reserved == 0
        assert post(payload) == 200 and state.reserved == 1 and len(calls) == 1
        assert post(payload) == 502 and state.reserved == 1 and len(calls) == 1
        gate = ForwardState(None, None, 24)
        first = gate.reserve(payload, raw)
        rejected = []
        def collide():
            try:
                gate.reserve(payload, raw)
            except ConcurrentRequest:
                rejected.append(True)
        threads = [threading.Thread(target=collide) for _ in range(16)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        assert len(rejected) == 16 and gate.reserved == 1 and gate.peak_active == 1
        gate.release()
        assert first['ordinal'] == 1 and gate.active == 0
        attempts = []
        def fail_transport(_):
            attempts.append(True)
            raise TimeoutError('offline_expected_failure')
        failure_gate = ForwardState(None, None, 1, fail_transport, proof)
        try:
            failure_gate.forward(payload, raw)
            raise AssertionError('offline_failure_not_preserved')
        except TimeoutError:
            pass
        try:
            failure_gate.forward(payload, raw)
            raise AssertionError('failed_forward_budget_was_refunded')
        except BudgetExhausted:
            pass
        assert len(attempts) == 1 and failure_gate.reserved == 1 and failure_gate.records[0]['status'] == 'failed'
        assert len(SCENARIOS) == 12 and sum(case[1] for case in SCENARIOS) == MAX_REQUESTS
        assert select_scenarios(None, 24) == SCENARIOS
        assert select_scenarios(None, 1) == SCENARIOS  # preserve default budget-limited mode
        selected = select_scenarios('S05,GROUP', 6)
        assert [case[0] for case in selected] == ['GROUP', 'S05']
        assert sum(case[1] for case in selected) == 5
        for invalid in ['', ' ', 'S05,', ',GROUP', 'S05,,GROUP', 'S05,S05', 'UNKNOWN', 's05']:
            try:
                select_scenarios(invalid, 24)
                raise AssertionError('invalid_selection_accepted')
            except ValueError:
                pass
        for raw_cases, limit in [(None, 0), (None, 25), ('GROUP,S05', 4)]:
            try:
                select_scenarios(raw_cases, limit)
                raise AssertionError('invalid_selection_limit_accepted')
            except ValueError:
                pass
        return {'status': 'passed', 'checks': 13, 'original_gate_checks': 9, 'selection_checks': 4,
                'external_model_requests': 0,
                'source_data_opened': False, 'transport': 'loopback plus explicit offline-only stub'}
    finally:
        server.shutdown()
        server.server_close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('binary', 'web-dir', 'source-data-dir', 'expected-commit', 'report'):
        parser.add_argument('--' + name)
    parser.add_argument('--max-real-requests', type=int, default=24)
    parser.add_argument('--cases', help='comma separated existing case IDs; executed in stable scenario-manifest order')
    parser.add_argument('--expected-script-commit', help='require this exact clean Git commit for the validation script; independent of the program SHA')
    parser.add_argument('--openssl')
    parser.add_argument('--self-test', action='store_true', help='offline gate checks; no credentials or real model')
    args = parser.parse_args()
    try:
        selected = select_scenarios(args.cases, args.max_real_requests)
    except ValueError as error:
        parser.error(str(error))
    if args.expected_script_commit and not re.fullmatch('[0-9a-f]{40}', args.expected_script_commit):
        parser.error('--expected-script-commit must be a full lowercase commit SHA')
    if args.self_test:
        print(json.dumps(self_test()))
        return 0
    if not all((args.binary, args.web_dir, args.source_data_dir, args.expected_commit, args.report)):
        parser.error('binary, web-dir, source-data-dir, expected-commit and report are required')
    if not re.fullmatch('[0-9a-f]{40}', args.expected_commit):
        parser.error('--expected-commit must be a full lowercase commit SHA')
    try:
        script_source = script_source_metadata(args.expected_script_commit)
    except ValueError as error:
        parser.error(str(error))
    binary, web = pathlib.Path(args.binary).resolve(strict=True), pathlib.Path(args.web_dir).resolve(strict=True)
    source, output = pathlib.Path(args.source_data_dir).resolve(strict=True), pathlib.Path(args.report).resolve()
    if output.is_relative_to(source) or output.exists():
        parser.error('report must be a new file outside source-data-dir; failed evidence is never overwritten')
    report = {'schema_version': 1, 'kind': 'isolated real-model conversation-state samples',
              'status': 'running', 'started_at': base.utc_now(), 'expected_commit': args.expected_commit,
              'program_expected_commit': args.expected_commit,
              'expected_script_commit': args.expected_script_commit,
              'script_source_commit': script_source['commit'], 'script_source_clean': script_source['clean'],
              'script_source_verification': script_source['verification'],
              'binary_sha256': sha(binary.read_bytes()), 'runner_sha256': sha(pathlib.Path(__file__).read_bytes()),
              'helper_sha256': {pathlib.Path(module.__file__).name: sha(pathlib.Path(module.__file__).read_bytes())
                                for module in (coord, batch, base)},
              'architecture': platform.machine(), 'request_budget': {'limit': args.max_real_requests,
                  'reserved': 0, 'forwarded': 0, 'failed_attempts': 0, 'retry_policy': 'none'},
              'selected_case_ids': [case[0] for case in selected],
              'planned_requests': sum(case[1] for case in selected),
              'case_selection': {'explicit': args.cases is not None,
                  'requested_ids': [value.strip() for value in args.cases.split(',')] if args.cases is not None else None,
                  'execution_order': 'stable_SCENARIOS_manifest_order',
                  'all_defined_case_count': len(SCENARIOS),
                  'excluded_case_ids': [case[0] for case in SCENARIOS if case not in selected]},
              'max_input_bytes': MAX_INPUT_BYTES, 'max_output_tokens': MAX_OUTPUT_TOKENS,
              'provider_hard_timeout_seconds': NETWORK_DEADLINE, 'max_actual_concurrency': 1,
              'real_host_time': base.utc_now(), 'fixed_fixture_15_00_claimed': False,
              'actual_midnight_transition_tested': False, 'production_model_configuration_read': False,
              'production_model_credential_read': False, 'production_database_modified': False,
              'real_user_content_accessed': False, 'production_personality_read': False,
              'real_qq_tested': False, 'real_weixin_tested': False, 'client_delivery_tested': False,
              'synthetic_source_only': True, 'model_self_grading_used': False,
              'expression_status': 'review_pending', 'semantic_pass_claimed': False, 'cases': []}
    forward = server = replay = h = None
    stage, key = 'read only active model', None
    try:
        with closing(sqlite3.connect((source / 'dsh.db').as_uri() + '?mode=ro', uri=True)) as db:
            db.execute('PRAGMA query_only=ON')
            found = db.execute("SELECT json_extract(c.config_json,'$.model') FROM config_revisions c JOIN settings s ON s.key='active_revision' AND c.revision=CAST(s.value AS INTEGER)").fetchone()
        if not found:
            raise RuntimeError('active_model_unavailable')
        model = json.loads(found[0])
        report['production_model_configuration_read'] = True
        endpoint = coord.external_endpoint(model['base_url'])
        store = (source / 'secrets').resolve(strict=True)
        credential = (store / str(uuid.UUID(model['credential_ref']))).resolve(strict=True)
        if not credential.is_relative_to(store):
            raise ValueError('credential_path_escaped_private_store')
        key = credential.read_text(encoding='utf-8')
        if not key.strip():
            raise ValueError('empty_model_credential')
        report['production_model_credential_read'] = True
        forward = ForwardState(endpoint, key, args.max_real_requests)
        key = None
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Forwarder)
        server.daemon_threads = True
        server.state = forward
        threading.Thread(target=server.serve_forever, daemon=True).start()
        with tempfile.TemporaryDirectory(prefix='dsh-conversation-state-live-') as temporary:
            h = base.Harness(str(binary), str(web), temporary, forward)
            forward.harness, forward.loopback_key = h, h.key
            try:
                stage = 'isolated daemon setup'
                h.start()
                h.bootstrap()
                live, _, _ = h.request('GET', '/health/live')
                if h.commit != args.expected_commit or live.get('commit') != args.expected_commit:
                    raise AssertionError('health_does_not_match_expected_release')
                manifest = json.loads((web / 'release.json').read_text(encoding='utf-8'))
                if manifest['commit'] != args.expected_commit:
                    raise AssertionError('web_release_does_not_match_expected_release')
                report['commit'] = h.commit
                h.configure(server.server_port)
                secret = secrets.token_urlsafe(32)
                signer = batch.Signer(batch.openssl_path(args.openssl), temporary, secret)
                qq, _, _ = h.request('POST', API + '/secrets', {'kind': 'qq_app_secret', 'value': secret})
                with h.db() as db:
                    db.execute('INSERT INTO reply_account_pacing(account_id,next_ms) VALUES(?,?)', (batch.ACCOUNT, batch.PACING_FENCE_MS))
                config = copy.deepcopy(h.applied)
                config['model']['model'] = model['model']
                config['model']['image_generation'] = False
                config['assistant']['max_concurrent_runs'] = 1
                config['assistant']['timeout_seconds'] = NETWORK_DEADLINE + 15
                config['speech']['enabled'] = False
                config['qq'] = {'enabled': True, 'app_id': batch.ACCOUNT, 'credential_ref': qq['id'], 'transport': 'webhook'}
                batch.apply_config(h, config)
                current, _, _ = h.request('GET', API + '/assistant/personality')
                personality = {'name': '合成验收', 'description': '只用于隔离合成场景的临时性格。',
                    'personality': '自然、诚实、按当前问题选择详略。', 'scenario': '临时合成对话验收。',
                    'system_prompt': '用自然中文交流，只依据有来源的资料回答，不编造记忆或已经执行的动作。', 'mes_example': ''}
                h.request('PUT', API + '/assistant/personality', {'personality': personality}, headers={'If-Match': current['etag']})
                report['configuration'] = {'model_id': model['model'], 'personality': 'temporary synthetic only',
                    'qq_mode': 'signed synthetic webhook with durable pre-enable pacing fence',
                    'real_model_key_stored_in_temp_database': False, 'temperature_or_seed_forced': False}
                replay = Replay(h, forward, signer, report)
                for identifier, planned, review in selected:
                    entry = {'id': identifier, 'planned_requests': planned, 'review_instruction': review,
                             'host_status': 'running', 'expression_status': 'review_pending',
                             'host_checks': [], 'run_ids': [], 'started_at': base.utc_now()}
                    report['cases'].append(entry)
                    if forward.reserved + planned > args.max_real_requests:
                        entry.update(host_status='not_run_budget', finished_at=base.utc_now())
                        continue
                    replay.case, forward.case_id = entry, identifier
                    stage = 'scenario ' + identifier
                    before = forward.reserved
                    try:
                        scenario(replay, identifier)
                        replay.check('exact_planned_requests_no_hidden_retry', forward.reserved - before == planned)
                        entry['host_status'] = 'passed'
                    except Exception as error:
                        entry.update(host_status='failed', error_type=type(error).__name__)
                        if isinstance(error, AssertionError):
                            entry['error_label'] = forward.redact(str(error), 200)
                    finally:
                        entry.update(actual_reserved_requests=forward.reserved - before, finished_at=base.utc_now())
                    print(entry['host_status'].upper() + ' ' + identifier, flush=True)
                report['outbound_safety'] = {
                    'qq_api_calls': replay.scalar('SELECT count(*) FROM qq_api_calls'),
                    'pacing_fence_retained': replay.scalar('SELECT next_ms FROM reply_account_pacing WHERE account_id=?', (batch.ACCOUNT,)) == batch.PACING_FENCE_MS,
                    'weixin_enabled_accounts': replay.scalar('SELECT count(*) FROM weixin_account WHERE enabled=1')}
                safety = report['outbound_safety']
                if safety['qq_api_calls'] or not safety['pacing_fence_retained'] or safety['weixin_enabled_accounts']:
                    raise AssertionError('outbound_safety_fence_failed')
                failed = any(case['host_status'] == 'failed' for case in report['cases']) or bool(forward.failures)
                limited = any(case['host_status'] == 'not_run_budget' for case in report['cases'])
                report['status'] = 'host_failed' if failed else 'budget_limited' if limited else 'host_complete_review_pending'
            finally:
                # Preserve the failed Run and raw bounded synthetic model output
                # BEFORE closing/removing the temporary database.
                if replay is not None:
                    try:
                        report['synthetic_run_diagnostics'] = replay.diagnostics()
                    except Exception as error:
                        report['diagnostic_error_type'] = type(error).__name__
                        report['status'] = 'diagnostic_failed'
                h.close()
    except Exception as error:
        report.update(status='setup_or_safety_failed', failed_phase=stage, error_type=type(error).__name__)
    finally:
        key = None
        if server is not None:
            server.shutdown()
            server.server_close()
        if forward is not None:
            report['request_budget'].update(reserved=forward.reserved, forwarded=forward.forwarded,
                failed_attempts=sum(r['status'] == 'failed' for r in forward.records),
                http_200=sum(r.get('http_status') == 200 for r in forward.records),
                observed_peak_concurrency=forward.peak_active)
            report['request_evidence'] = forward.records
            report['forward_failures'] = forward.failures
            report['real_external_model_attempted'] = forward.forwarded > 0
            report['real_external_model_tested'] = any(r.get('http_status') == 200 for r in forward.records)
            forward.key = None
        report.update(finished_at=base.utc_now(), host_passed=sum(c['host_status'] == 'passed' for c in report['cases']),
                      host_failed=sum(c['host_status'] == 'failed' for c in report['cases']),
                      semantic_pass_claimed=False, expression_status='review_pending')
        output.parent.mkdir(parents=True, exist_ok=True)
        with output.open('x', encoding='utf-8') as target:
            json.dump(report, target, ensure_ascii=False, indent=2)
            target.write('\n')
        print(json.dumps({'status': report['status'], 'host_passed': report['host_passed'],
                          'host_failed': report['host_failed'], 'request_budget': report['request_budget'],
                          'expression_status': 'review_pending', 'report': str(output)}))
    return 0 if report['status'] == 'host_complete_review_pending' else 1


if __name__ == '__main__':
    raise SystemExit(main())
