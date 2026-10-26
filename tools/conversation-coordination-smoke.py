#!/usr/bin/env python3
"""Isolated conversation correction and delivered-history process acceptance.

Uses signed synthetic QQ inbound events, a loopback model and a durable pacing
fence installed before enabling the synthetic account. Platform receipts below
are explicitly fabricated in a disposable SQLite database. No real QQ request,
real model, production configuration, message or private history is accessed.
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
import time


sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    'coordination_batch_harness', pathlib.Path(__file__).with_name('qq-text-batches-smoke.py'))
batch = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(batch)
base = batch.base
rows = batch.rows
wait_for = batch.wait_for


class ModelState(base.MockState):
    def __init__(self):
        super().__init__()
        self.answers = {}


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        size = int(self.headers.get('Content-Length', '0'))
        if not 0 < size <= 1024 * 1024:
            self.send_error(413)
            return
        payload = json.loads(self.rfile.read(size))
        mock = self.server.mock
        question = mock.question(payload)
        with mock.condition:
            mock.requests.append(payload)
            mock.condition.notify_all()
        held = question == mock.hold_question
        if held:
            mock.hold_received.set()
            mock.hold_release.wait(timeout=45)
        answers = mock.answers.get(question, ['SYNTHETIC_SUCCESSOR_REPLY'])
        content = json.dumps([
            {'type': 'text', 'text': answer, 'delay_ms': 0 if index == 0 else 1500}
            for index, answer in enumerate(answers)], ensure_ascii=False)
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
                mock.hold_responded.set()


def snapshot(harness, run):
    found = rows(harness, 'SELECT messages_json,digest,compiler_version,created_at '
                 'FROM run_prompt_snapshots WHERE run_id=?', (run,))
    if len(found) != 1:
        raise AssertionError('a dispatched request requires one immutable prompt snapshot')
    return found[0]


def decision(harness, run):
    return rows(harness, 'SELECT * FROM run_publication_decisions WHERE run_id=?', (run,))[0]


def receive(harness, signer, user, text):
    value = batch.event(user, text)
    batch.webhook(harness, signer, value)
    return batch.received_runs(harness, [value])[0]


def complete(harness, run):
    value = harness.wait_run(run, via_api=False)
    if value['status'] != 'completed':
        raise AssertionError('isolated synthetic conversation must complete')
    return value


def hold(harness, signer, mock, user, question, answer):
    mock.hold_question = question
    mock.hold_release.clear()
    mock.hold_received.clear()
    mock.hold_responded.clear()
    mock.answers[question] = [answer]
    run = receive(harness, signer, user, question)
    if not mock.hold_received.wait(timeout=12):
        raise TimeoutError('loopback provider did not reach model response barrier')
    return run


def text(payload, role=None):
    return '\n'.join(message['content'] for message in payload['messages']
                     if role is None or message['role'] == role)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report', required=True)
    parser.add_argument('--openssl')
    args = parser.parse_args()
    binary = str(pathlib.Path(args.binary).resolve(strict=True))
    web = str(pathlib.Path(args.web_dir).resolve(strict=True))
    report = {
        'kind': 'conversation correction and actual-delivery history isolated process acceptance',
        'status': 'running', 'started_at': base.utc_now(), 'checks': [],
        'commit': None, 'architecture': platform.machine(),
        'binary_sha256': hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest(),
        'production_data_accessed': False, 'real_external_model_tested': False,
        'real_qq_tested': False, 'qq_outbound_http_tested': False,
        'client_delivery_tested': False, 'synthetic_delivery_receipts': True,
        'outbound_safety': 'durable account pacing fence installed before synthetic webhook account enable',
    }
    stage, exit_code = 'setup', 0

    def check(name, condition=True):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    mock = ModelState()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    server.daemon_threads = True
    server.mock = mock
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-conversation-coordination-') as temporary:
            h = base.Harness(binary, web, temporary, mock)
            try:
                h.start()
                h.bootstrap()
                h.configure(server.server_port)
                report['commit'] = h.commit
                secret = secrets.token_urlsafe(32)
                signer = batch.Signer(batch.openssl_path(args.openssl), temporary, secret)
                credential, _, _ = h.request('POST', batch.API + '/secrets', {
                    'kind': 'qq_app_secret', 'value': secret})
                with h.db() as database:
                    database.execute('INSERT INTO reply_account_pacing(account_id,next_ms) VALUES(?,?)',
                                     (batch.ACCOUNT, batch.PACING_FENCE_MS))
                config = copy.deepcopy(h.applied)
                config['qq'] = {'enabled': True, 'app_id': batch.ACCOUNT,
                                'credential_ref': credential['id'], 'transport': 'webhook'}
                batch.apply_config(h, config)
                check('synthetic QQ dispatch is durably fenced before the account is enabled')

                suffix = secrets.token_hex(6)
                user = 'correction_' + suffix
                question = 'ORIGINAL_PRIVATE_QUESTION_' + suffix
                stale_answer = 'NEVER_DELIVERED_OLD_ASSISTANT_' + suffix
                old = hold(h, signer, mock, user, question, stale_answer)
                before = snapshot(h, old)
                prior_payload = copy.deepcopy(mock.for_question(question))
                correction_text = '等等，改成明天'
                successor = receive(h, signer, user, correction_text)
                wait_for(lambda: decision(h, old)['decision'] == 'suppressed',
                         'explicit correction did not suppress the stale publication')
                check('explicit correction links a separate successor while the real HTTP request remains held',
                      successor != old and not mock.hold_responded.is_set()
                      and len(rows(h, 'SELECT 1 FROM native_worker_leases WHERE run_id=?', (old,))) == 1
                      and rows(h, 'SELECT status FROM runs WHERE id=?', (successor,))[0]['status'] == 'queued')
                check('an in-flight correction cannot rewrite the immutable old PromptSnapshot',
                      snapshot(h, old) == before and mock.for_question(question) == prior_payload
                      and correction_text not in text(prior_payload, 'user'))
                mock.hold_release.set()
                complete(h, old)
                complete(h, successor)
                following = mock.for_question(correction_text)
                check('the old generation settles for audit without creating any Outbox',
                      stale_answer in rows(h, 'SELECT response FROM runs WHERE id=?', (old,))[0]['response']
                      and not rows(h, 'SELECT 1 FROM outbox WHERE run_id=?', (old,))
                      and not rows(h, 'SELECT 1 FROM native_worker_leases WHERE run_id=?', (old,)))
                check('successor HTTP input retains original question and correction but excludes the unsent assistant',
                      question in text(following) and correction_text in text(following)
                      and stale_answer not in text(following)
                      and 'DSH_CONVERSATION_CORRECTION_V1' in text(following))
                check('input sequence predecessor and suppression revision persist independently of Run state',
                      decision(h, old)['revision'] > 1
                      and decision(h, successor)['predecessor_run_id'] == old
                      and decision(h, successor)['first_seq'] > decision(h, old)['last_seq']
                      and snapshot(h, old) == before)

                for label, ordinary in [('thanks', '谢谢'), ('supplement', '还想补充一个条件')]:
                    scope = 'ordinary_' + label + '_' + suffix
                    first_question = 'ORDINARY_' + label + '_' + suffix
                    original = hold(h, signer, mock, scope, first_question,
                                    'ORDINARY_OLD_ANSWER_' + label + '_' + suffix)
                    original_snapshot = snapshot(h, original)
                    later = receive(h, signer, scope, ordinary)
                    check(label + ' creates a successor without cancelling ordinary in-flight work',
                          later != original and decision(h, original)['decision'] == 'active'
                          and not rows(h, 'SELECT 1 FROM conversation_corrections WHERE run_id=?', (later,))
                          and snapshot(h, original) == original_snapshot
                          and len(rows(h, 'SELECT 1 FROM native_worker_leases WHERE run_id=?', (original,))) == 1)
                    mock.hold_release.set()
                    complete(h, original)
                    complete(h, later)
                    check(label + ' preserves generation and queued Outbox without claiming delivery',
                          len(rows(h, 'SELECT 1 FROM outbox WHERE run_id=? AND status=?',
                                   (original, 'queued'))) == 1
                          and 'ORDINARY_OLD_ANSWER_' + label + '_' + suffix not in text(mock.for_question(ordinary), 'assistant'))

                # These are synthetic platform receipts, written only to h's
                # TemporaryDirectory. They never represent actual HTTP or client evidence.
                fixtures = {}
                for kind in ['partial', 'unknown', 'expiry']:
                    scope = kind + '_' + suffix
                    source_question = kind.upper() + '_SOURCE_' + suffix
                    sent = 'SYNTHETIC_CONFIRMED_' + kind + '_' + suffix
                    unseen = 'SYNTHETIC_UNSEEN_' + kind + '_' + suffix
                    tail = 'SYNTHETIC_TAIL_' + kind + '_' + suffix
                    mock.answers[source_question] = [sent, unseen, tail]
                    source = receive(h, signer, scope, source_question)
                    complete(h, source)
                    outboxes = rows(h, 'SELECT id FROM outbox WHERE run_id=?', (source,))
                    if len(outboxes) != 1:
                        raise AssertionError('synthetic multipart history fixture needs exactly one Outbox')
                    outbox = outboxes[0]['id']
                    with h.db() as database:
                        database.execute("UPDATE outbox_reply_parts SET status='sent',platform_message_id=?,finished_ms=? WHERE outbox_id=? AND seq=1",
                                         ('synthetic-receipt-' + kind, int(time.time() * 1000), outbox))
                        if kind == 'unknown':
                            database.execute("UPDATE outbox_reply_parts SET status='unknown',error='synthetic unknown' WHERE outbox_id=? AND seq=2", (outbox,))
                            database.execute("UPDATE outbox SET status='unknown' WHERE id=?", (outbox,))
                        if kind == 'expiry':
                            database.execute('UPDATE outbox SET deadline=? WHERE id=?', (int(time.time()) - 1, outbox))
                    if kind == 'expiry':
                        wait_for(lambda: rows(h, 'SELECT status FROM outbox WHERE id=?', (outbox,))[0]['status'] == 'expired',
                                 'expired fixture was not settled by the real sender loop')
                    next_question = '等等，改成明天' if kind != 'expiry' else 'EXPIRY_FOLLOWUP_' + suffix
                    next_run = receive(h, signer, scope, next_question)
                    complete(h, next_run)
                    payload = rows(h, 'SELECT messages_json FROM run_prompt_snapshots WHERE run_id=?', (next_run,))[0]
                    messages = json.loads(payload['messages_json'])
                    assistant = '\n'.join(item['content'] for item in messages if item['role'] == 'assistant')
                    all_text = '\n'.join(item['content'] for item in messages)
                    check(kind + ' history projects only the synthetic confirmed sent segment',
                          sent in assistant and unseen not in all_text and tail not in all_text
                          and 'DSH_DELIVERY_EVIDENCE_V1' in all_text)
                    parts = rows(h, 'SELECT status,platform_message_id FROM outbox_reply_parts WHERE outbox_id=? ORDER BY seq', (outbox,))
                    check(kind + ' preserves the synthetic sent receipt and handles unclaimed segments accurately',
                          parts[0]['status'] == 'sent' and parts[0]['platform_message_id'] == 'synthetic-receipt-' + kind
                          and (kind != 'partial' or [item['status'] for item in parts] == ['sent', 'cancelled', 'cancelled'])
                          and (kind != 'unknown' or parts[1]['status'] == 'unknown')
                          and (kind != 'expiry' or [item['status'] for item in parts] == ['sent', 'expired', 'expired']))
                    if kind == 'unknown':
                        check('unknown delivery prevents automatic predecessor suppression or replay',
                              decision(h, source)['decision'] == 'active'
                              and rows(h, 'SELECT reason FROM conversation_corrections WHERE run_id=?', (next_run,))[0]['reason'] == 'delivery_in_flight_or_unknown')
                    fixtures[kind] = (source, outbox)

                count = mock.count()
                suppression = decision(h, old)
                h.stop()
                h.start()
                h.login()
                check('restart preserves suppression revision original snapshot and settles no duplicate model request',
                      decision(h, old) == suppression and snapshot(h, old) == before and mock.count() == count
                      and not rows(h, 'SELECT 1 FROM outbox WHERE run_id=?', (old,)))
                unknown_outbox = fixtures['unknown'][1]
                check('restart does not turn a synthetic unknown delivery into unsent queued work',
                      rows(h, 'SELECT status FROM outbox WHERE id=?', (unknown_outbox,))[0]['status'] == 'unknown'
                      and rows(h, 'SELECT status FROM outbox_reply_parts WHERE outbox_id=? AND seq=2', (unknown_outbox,))[0]['status'] == 'unknown')
                fence = rows(h, 'SELECT next_ms FROM reply_account_pacing WHERE account_id=?', (batch.ACCOUNT,))
                check('the original durable fence remains installed and no real QQ API call was executed',
                      fence[0]['next_ms'] == batch.PACING_FENCE_MS
                      and not rows(h, 'SELECT 1 FROM qq_api_calls')
                      and not rows(h, "SELECT 1 FROM outbox WHERE status='sending'")
                      and not rows(h, "SELECT 1 FROM outbox_reply_parts WHERE status='sending'"))
                check('synthetic coordination produces no long-term memory records',
                      not rows(h, 'SELECT 1 FROM memory_records'))
                report['status'] = 'passed'
            finally:
                h.close()
    except Exception as error:
        exit_code = 1
        report.update(status='failed', failed_phase=stage, error_type=type(error).__name__)
        print('FAIL ' + stage + ': ' + type(error).__name__, file=sys.stderr)
    finally:
        mock.hold_release.set()
        server.shutdown()
        server.server_close()
        report['finished_at'] = base.utc_now()
        report['passed'] = len(report['checks'])
        report['mock_model_http_tested'] = mock.count() > 0
        output = pathlib.Path(args.report)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    return exit_code


if __name__ == '__main__':
    raise SystemExit(main())
