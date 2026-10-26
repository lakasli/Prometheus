#!/usr/bin/env python3
"""Three-request real-model coordination acceptance in a disposable daemon.

Read only production's active model configuration and referenced model key.
Signed synthetic QQ Webhooks use a synthetic account/secret in Webhook mode,
with a durable dispatch fence installed first. This is not real QQ/Weixin or
client delivery acceptance. No production conversations or memories are read.
The original request is held at a loopback HTTP barrier before real forwarding;
this does not claim remote-provider cancellation or remote in-flight correction.
"""
import argparse
import copy
import hashlib
import http.client
import http.server
import importlib.util
import ipaddress
import json
import pathlib
import platform
import re
import secrets
import socket
import sqlite3
import ssl
import sys
import tempfile
import threading
import time
import urllib.parse
import uuid
from contextlib import closing

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    'coordination_live_batch', pathlib.Path(__file__).with_name('qq-text-batches-smoke.py'))
batch = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(batch)
base = batch.base
MAX_REQUESTS = 3
MAX_INPUT_BYTES = 128_000
MAX_OUTPUT_TOKENS = 1024
MAX_RESPONSE_BYTES = 1_048_576


def sha(data):
    return hashlib.sha256(data).hexdigest()


def body_text(payload):
    return '\n'.join(message['content'] for message in payload['messages'])


def external_endpoint(base_url):
    parsed = urllib.parse.urlsplit(base_url)
    if (parsed.scheme != 'https' or not parsed.hostname or parsed.username is not None
            or parsed.password is not None or parsed.query or parsed.fragment
            or parsed.port not in (None, 443)):
        raise ValueError('an external HTTPS model endpoint without credentials or query is required')
    addresses = {item[4][0] for item in socket.getaddrinfo(
        parsed.hostname, 443, type=socket.SOCK_STREAM)}
    if not addresses or any(not ipaddress.ip_address(value).is_global for value in addresses):
        raise ValueError('provider DNS must resolve exclusively to public addresses')
    return parsed.hostname, sorted(addresses)[0], parsed.path.rstrip('/') + '/chat/completions'


class PinnedHTTPS(http.client.HTTPSConnection):
    """No proxy, no redirects; TLS checks the original host on the checked IP."""
    def __init__(self, host, address):
        super().__init__(host, port=443, timeout=90, context=ssl.create_default_context())
        self.checked_address = address

    def connect(self):
        connection = socket.create_connection((self.checked_address, 443), self.timeout)
        try:
            self.sock = self._context.wrap_socket(connection, server_hostname=self.host)
        except BaseException:
            connection.close()
            raise


class ForwardState(base.MockState):
    def __init__(self, endpoint, key):
        super().__init__()
        self.endpoint, self.key = endpoint, key
        self.records = []
        self.failures = []
        self.forward_attempts = 0
        self.forwarded_requests = 0
        self.harness = None

    def forward(self, payload, raw, question):
        if (payload.get('stream') is not False or payload.get('max_tokens', 0) <= 0
                or payload['max_tokens'] > MAX_OUTPUT_TOKENS):
            raise AssertionError('request exceeds the explicit output-token bound')
        with self.condition:
            if self.forward_attempts >= MAX_REQUESTS:
                raise AssertionError('real model request budget exhausted')
            self.forward_attempts += 1
            record = {'ordinal': self.forward_attempts, 'body_sha256': sha(raw),
                      'input_bytes': len(raw), 'max_output_tokens': payload['max_tokens'],
                      'status': 'started'}
            self.records.append(record)
        found = batch.rows(self.harness,
                           'SELECT p.messages_json FROM run_prompt_snapshots p '
                           'JOIN runs r ON r.id=p.run_id WHERE r.input=?', (question,))
        if len(found) != 1 or json.loads(found[0]['messages_json']) != payload['messages']:
            raise AssertionError('forwarded messages differ from the durable prompt snapshot')
        record['frozen_snapshot_matches_actual_body'] = True
        host, address, path = self.endpoint
        connection = PinnedHTTPS(host, address)
        try:
            connection.request('POST', path, body=raw, headers={
                'Content-Type': 'application/json', 'Authorization': 'Bearer ' + self.key})
            with self.condition:
                self.forwarded_requests += 1
            response = connection.getresponse()
            content = response.read(MAX_RESPONSE_BYTES + 1)
            record['http_status'] = response.status
            if len(content) > MAX_RESPONSE_BYTES:
                raise AssertionError('real provider response exceeds the bounded body limit')
            if response.status != 200:
                raise RuntimeError('real provider did not accept the bounded request')
            result = json.loads(content)
            synthetic_text = result['choices'][0]['message']['content']
            if not isinstance(synthetic_text, str):
                raise AssertionError('real provider did not return text')
            sample = synthetic_text.replace(self.key, '[model-credential-redacted]')
            record.update(status='completed', response_sha256=sha(content),
                          synthetic_response=sample[:600])
            return content
        except Exception as error:
            record.update(status='failed', error_type=type(error).__name__)
            raise
        finally:
            connection.close()


class Forwarder(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        state = self.server.forward_state
        try:
            size = int(self.headers.get('Content-Length', '0'))
            if not 0 < size <= MAX_INPUT_BYTES:
                raise AssertionError('model input byte budget exceeded')
            raw = self.rfile.read(size)
            payload = json.loads(raw)
            question = state.question(payload)
            with state.condition:
                state.requests.append(payload)
                state.condition.notify_all()
            if question == state.hold_question:
                state.hold_received.set()
                if not state.hold_release.wait(timeout=30):
                    raise TimeoutError('synthetic correction barrier timed out')
            content = state.forward(payload, raw, question)
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(content)))
            self.end_headers()
            self.wfile.write(content)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass
        except Exception as error:
            with state.condition:
                state.failures.append(type(error).__name__)
            try:
                self.send_error(502, 'bounded isolated real-model forwarding failed')
            except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                pass
        finally:
            if state.hold_received.is_set() and state.hold_release.is_set():
                state.hold_responded.set()


def receive(harness, signer, user, text):
    event = batch.event(user, text)
    batch.webhook(harness, signer, event)
    return batch.received_runs(harness, [event])[0]


def snapshot(harness, run):
    found = batch.rows(harness, 'SELECT messages_json,digest,compiler_version,created_at '
                       'FROM run_prompt_snapshots WHERE run_id=?', (run,))
    if len(found) != 1:
        raise AssertionError('one frozen prompt must exist for a dispatched synthetic request')
    return found[0]


def decision(harness, run):
    return batch.rows(harness, 'SELECT * FROM run_publication_decisions WHERE run_id=?', (run,))[0]


def completed(harness, run):
    deadline = time.monotonic() + 150
    while time.monotonic() < deadline:
        found = batch.rows(harness, 'SELECT status,response,error FROM runs WHERE id=?', (run,))
        if found and found[0]['status'] in base.TERMINAL:
            if found[0]['status'] != 'completed':
                raise AssertionError('synthetic real-model run did not complete')
            return found[0]
        time.sleep(0.1)
    raise TimeoutError('bounded real-model run exceeded 150 seconds')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--source-data-dir', required=True)
    parser.add_argument('--report', required=True)
    parser.add_argument('--expected-commit', required=True)
    parser.add_argument('--openssl')
    args = parser.parse_args()
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    source = pathlib.Path(args.source_data_dir).resolve(strict=True)
    report = {
        'kind': 'isolated real-model conversation correction and source isolation acceptance',
        'status': 'running', 'started_at': base.utc_now(), 'checks': [],
        'expected_commit': args.expected_commit, 'commit': None,
        'architecture': platform.machine(), 'binary_sha256': sha(binary.read_bytes()),
        'model_request_budget': MAX_REQUESTS, 'max_input_bytes_per_request': MAX_INPUT_BYTES,
        'max_output_tokens_per_request': MAX_OUTPUT_TOKENS,
        'real_external_model_attempted': False, 'real_external_model_tested': False,
        'production_model_configuration_read': False, 'production_model_credential_read': False,
        'production_database_modified': False, 'real_user_content_accessed': False,
        'real_qq_tested': False, 'real_weixin_tested': False, 'client_delivery_tested': False,
        'signed_synthetic_qq_inbound': True, 'synthetic_source_only': True,
        'synthetic_delivery_receipts': False, 'provider_inflight_cancellation_tested': False,
        'barrier_scope': 'loopback HTTP held before forwarding to the real external model',
        'outbound_safety': 'synthetic Webhook account; durable pacing fence installed before enable',
    }
    stage, key, forward_state, server = 'setup', None, None, None

    def check(name, condition=True):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    try:
        if not re.fullmatch('[0-9a-f]{40}', args.expected_commit):
            raise ValueError('a full expected release SHA is required')
        with closing(sqlite3.connect((source / 'dsh.db').as_uri() + '?mode=ro', uri=True)) as db:
            db.execute('PRAGMA query_only=ON')
            row = db.execute("SELECT json_extract(c.config_json,'$.model') FROM config_revisions c "
                             "JOIN settings s ON s.key='active_revision' "
                             "AND c.revision=CAST(s.value AS INTEGER)").fetchone()
        if not row:
            raise RuntimeError('active model configuration is unavailable')
        model = json.loads(row[0])
        report['production_model_configuration_read'] = True
        endpoint = external_endpoint(model['base_url'])
        store = (source / 'secrets').resolve(strict=True)
        credential = (store / str(uuid.UUID(model['credential_ref']))).resolve(strict=True)
        if not credential.is_relative_to(store):
            raise ValueError('model credential escaped its private store')
        key = credential.read_text(encoding='utf-8')
        report['production_model_credential_read'] = True
        forward_state = ForwardState(endpoint, key)
        key = None
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Forwarder)
        server.daemon_threads = True
        server.forward_state = forward_state
        threading.Thread(target=server.serve_forever, daemon=True).start()
        with tempfile.TemporaryDirectory(prefix='dsh-coordination-live-') as temporary:
            h = base.Harness(str(binary), str(web), temporary, forward_state)
            forward_state.harness = h
            try:
                h.start()
                h.bootstrap()
                report['commit'] = h.commit
                check('temporary daemon matches the exact deployed release SHA', h.commit == args.expected_commit)
                h.configure(server.server_port)
                synthetic_secret = secrets.token_urlsafe(32)
                signer = batch.Signer(batch.openssl_path(args.openssl), temporary, synthetic_secret)
                qq_key, _, _ = h.request('POST', batch.API + '/secrets', {
                    'kind': 'qq_app_secret', 'value': synthetic_secret})
                with h.db() as db:
                    db.execute('INSERT INTO reply_account_pacing(account_id,next_ms) VALUES(?,?)',
                               (batch.ACCOUNT, batch.PACING_FENCE_MS))
                config = copy.deepcopy(h.applied)
                config['model']['model'] = model['model']
                config['model']['image_generation'] = False
                config['assistant']['timeout_seconds'] = 120
                config['assistant']['max_concurrent_runs'] = 2
                config['speech']['enabled'] = False
                config['qq'] = {'enabled': True, 'app_id': batch.ACCOUNT,
                                'credential_ref': qq_key['id'], 'transport': 'webhook'}
                batch.apply_config(h, config)
                check('real model key stays only in forwarder memory and all QQ credentials are synthetic',
                      h.applied['model']['base_url'].startswith('http://127.0.0.1:'))
                suffix = secrets.token_hex(6)
                a_marker, b_marker = 'A_PRIVATE_' + suffix, 'B_PRIVATE_' + suffix
                old_marker = 'STALE_CANDIDATE_' + suffix
                original_text = f'合成资料{a_marker}，记录数字17。此轮只回答{old_marker}。'
                corrected_text = '等等，改成23，记录数字改为23。只回答当前数字。'
                b_text = f'合成独立资料{b_marker}。请计算19乘17，只回答结果323。'
                forward_state.hold_question = original_text
                old = receive(h, signer, 'synthetic_a_' + suffix, original_text)
                if not forward_state.hold_received.wait(timeout=15):
                    raise TimeoutError('original request did not reach the loopback HTTP barrier')
                before = snapshot(h, old)
                before_payload = copy.deepcopy(forward_state.for_question(original_text))
                successor = receive(h, signer, 'synthetic_a_' + suffix, corrected_text)
                batch.wait_for(lambda: decision(h, old)['decision'] == 'suppressed',
                               'same-source correction did not suppress old publication', timeout=10)
                check('same-source correction queues a successor while the original HTTP lease is held',
                      successor != old and not forward_state.hold_release.is_set()
                      and len(batch.rows(h, 'SELECT 1 FROM native_worker_leases WHERE run_id=?', (old,))) == 1
                      and batch.rows(h, 'SELECT status FROM runs WHERE id=?', (successor,))[0]['status'] == 'queued')
                check('correction never rewrites the original in-flight prompt',
                      snapshot(h, old) == before and before_payload == forward_state.for_question(original_text)
                      and corrected_text not in body_text(before_payload))
                other = receive(h, signer, 'synthetic_b_' + suffix, b_text)
                forward_state.hold_release.set()
                stage = 'bounded real-model requests settle'
                old_result = completed(h, old)
                successor_result = completed(h, successor)
                b_result = completed(h, other)
                a_old = forward_state.for_question(original_text)
                a_next = forward_state.for_question(corrected_text)
                b_request = forward_state.for_question(b_text)
                check('actual forwarded A/B request bodies contain only their own private synthetic markers',
                      a_marker in body_text(a_old) and b_marker not in body_text(a_old)
                      and a_marker in body_text(a_next) and b_marker not in body_text(a_next)
                      and b_marker in body_text(b_request) and a_marker not in body_text(b_request)
                      and old_marker not in body_text(b_request))
                check('real stale answer is retained only for audit without any QQ Outbox',
                      old_marker in old_result['response']
                      and not batch.rows(h, 'SELECT 1 FROM outbox WHERE run_id=?', (old,))
                      and old_marker not in body_text(a_next) and snapshot(h, old) == before)
                check('successor real model uses the corrected fact without the unsent old candidate',
                      re.fullmatch(r'\s*23[。.!！]?\s*', successor_result['response']) is not None
                      and 'DSH_CONVERSATION_CORRECTION_V1' in body_text(a_next))
                check('independent B real model request completes its synthetic calculation',
                      re.fullmatch(r'\s*323[。.!！]?\s*', b_result['response']) is not None)
                check('exactly three bounded real HTTPS requests use the actual frozen prompt bodies',
                      forward_state.forward_attempts == MAX_REQUESTS
                      and forward_state.forwarded_requests == MAX_REQUESTS
                      and len(forward_state.records) == MAX_REQUESTS and not forward_state.failures
                      and all(item['status'] == 'completed' and item['frozen_snapshot_matches_actual_body']
                              for item in forward_state.records))
                check('durable QQ fence remains in place with no sender claim, memories, or summary model call',
                      batch.rows(h, 'SELECT next_ms FROM reply_account_pacing WHERE account_id=?',
                                 (batch.ACCOUNT,))[0]['next_ms'] == batch.PACING_FENCE_MS
                      and not batch.rows(h, 'SELECT 1 FROM qq_api_calls')
                      and not batch.rows(h, "SELECT 1 FROM outbox WHERE status<>'queued'")
                      and not batch.rows(h, "SELECT 1 FROM outbox_reply_parts WHERE status<>'queued'")
                      and not batch.rows(h, 'SELECT 1 FROM memory_records')
                      and not batch.rows(h, 'SELECT 1 FROM summary_jobs')
                      and not batch.rows(h, 'SELECT 1 FROM native_worker_leases'))
                report.update(status='passed', real_external_model_tested=True)
            finally:
                forward_state.hold_release.set()
                h.close()
    except Exception as error:
        report.update(status='failed', failed_phase=stage, error_type=type(error).__name__)
        print('FAIL isolated real-model coordination; inspect bounded synthetic report', flush=True)
    finally:
        key = None
        if forward_state is not None:
            forward_state.hold_release.set()
            report['real_external_model_attempted'] = forward_state.forward_attempts > 0
            report['real_external_model_tested'] = any(
                item['status'] == 'completed' for item in forward_state.records)
            report['model_requests'] = forward_state.forwarded_requests
            report['forward_attempts'] = forward_state.forward_attempts
            report['request_evidence'] = forward_state.records
            report['forward_failure_types'] = forward_state.failures
            forward_state.key = None
        if server is not None:
            server.shutdown()
            server.server_close()
        report.update(finished_at=base.utc_now(), passed=len(report['checks']))
        output = pathlib.Path(args.report).resolve()
        if output.is_relative_to(source):
            raise ValueError('validation report must be outside the production data directory')
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
        print(json.dumps({'status': report['status'], 'passed': report['passed'],
                          'model_requests': report.get('model_requests', 0),
                          'production_database_modified': False, 'real_qq_tested': False}))
    return 0 if report['status'] == 'passed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
