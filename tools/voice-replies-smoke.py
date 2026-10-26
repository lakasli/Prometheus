#!/usr/bin/env python3
"""Multi-sentence voice replies through isolated signed QQ input and Piper.

All account credentials, Webhook events, model output and databases are synthetic.
QQ outbound is durably fenced before enabling the synthetic account; this script
does not send QQ messages or read production data. --runtime-root enables actual
local Piper synthesis; without it the same reply must degrade to readable text.
Reports contain only hashes, measurements and check names, never text or audio.
"""
import argparse
import copy
import hashlib
import http.server
import importlib.util
import io
import json
import pathlib
import platform
import secrets
import sys
import tempfile
import threading
import wave

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    'voice_reply_qq_harness', pathlib.Path(__file__).with_name('qq-text-batches-smoke.py'))
batch = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(batch)
base = batch.base
API = base.PREFIX
SENTENCES = ['你好，今天也一起慢慢来吧。', '遇到难事可以先歇一会儿。']


class Model(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        size = int(self.headers.get('Content-Length', '0'))
        if not 0 < size <= 1024 * 1024:
            self.send_error(413)
            return
        payload = json.loads(self.rfile.read(size))
        mock = self.server.mock
        with mock.condition:
            mock.requests.append(payload)
            mock.condition.notify_all()
        raw = json.dumps({'choices': [{'message': {
            'role': 'assistant', 'content': self.server.output,
        }}]}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)


def terminal(harness, run):
    def result():
        found = batch.rows(harness, 'SELECT * FROM runs WHERE id=?', (run,))
        return found[0] if found and found[0]['status'] in base.TERMINAL else None
    return batch.wait_for(result, 'synthetic voice reply did not finish', timeout=90)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--runtime-root')
    parser.add_argument('--openssl')
    parser.add_argument('--report', required=True)
    args = parser.parse_args()
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    runtime = pathlib.Path(args.runtime_root).resolve(strict=True) if args.runtime_root else None
    crypto = batch.openssl_path(args.openssl)
    report = {
        'kind': 'multi-sentence voice reply signed synthetic QQ acceptance',
        'status': 'running', 'started_at': base.utc_now(), 'checks': [], 'commit': None,
        'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
        'architecture': platform.machine(), 'production_data_accessed': False,
        'real_local_piper_tested': False, 'mock_model_http_tested': False,
        'signed_webhook_tested': False, 'real_qq_tested': False,
        'qq_outbound_http_tested': False, 'real_external_model_tested': False,
        'outbound_safety': 'synthetic account is durably fenced before enabling QQ and remains fenced on restart',
    }
    stage, code = 'setup', 0

    def check(name, condition=True):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    mock = base.MockState()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
    server.daemon_threads = True
    server.mock = mock
    server.output = json.dumps([{'type': 'voice', 'text': ''.join(SENTENCES)}], ensure_ascii=False)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-voice-replies-') as temporary:
            h = base.Harness(str(binary), str(web), temporary, mock)
            if runtime:
                h.environment.update(DSH_PIPER_EXECUTABLE=str(runtime / 'venv/bin/python'),
                                     DSH_PIPER_MODEL=str(runtime / 'models/zh_CN-huayan-medium.onnx'))
            try:
                h.start()
                h.bootstrap()
                h.configure(server.server_port)
                report['commit'] = h.commit
                secret = secrets.token_urlsafe(32)
                signer = batch.Signer(crypto, temporary, secret)
                credential, _, _ = h.request('POST', API + '/secrets', {
                    'kind': 'qq_app_secret', 'value': secret})
                # The fence is installed with QQ disabled and is never removed.
                with h.db() as database:
                    database.execute('INSERT INTO reply_account_pacing(account_id,next_ms) VALUES(?,?)',
                                     (batch.ACCOUNT, batch.PACING_FENCE_MS))
                config = copy.deepcopy(h.applied)
                config['qq'] = {'enabled': True, 'app_id': batch.ACCOUNT,
                                'credential_ref': credential['id'], 'transport': 'webhook'}
                config['speech'] = {'enabled': runtime is not None, 'voice': 'zh_CN-huayan-medium',
                                    'timeout_seconds': 30, 'max_text_chars': 300}
                config['assistant']['max_concurrent_runs'] = 1
                batch.apply_config(h, config)
                check('synthetic QQ account is enabled only behind a persistent outbound fence')
                if runtime:
                    speech, _, _ = h.request('GET', API + '/speech')
                    check('installed Piper runtime is available to the isolated daemon', speech['available'])

                incoming = batch.event('voice_' + secrets.token_hex(6), '请用语音说两句鼓励的话。')
                batch.webhook(h, signer, incoming)
                run = batch.received_runs(h, [incoming])[0]
                result = terminal(h, run)
                report['signed_webhook_tested'] = True
                check('signed synthetic QQ request completes after a multi-sentence voice model reply',
                      result['status'] == 'completed' and result['response'] == '\n'.join(SENTENCES))
                payload = mock.for_question(result['input'])
                check('one loopback model HTTP request receives the actual structured reply protocol',
                      mock.count() == 1 and '宿主回复协议' in payload['messages'][0]['content'])
                report['mock_model_http_tested'] = True
                plan_rows = batch.rows(h, 'SELECT plan_json FROM run_reply_plans WHERE run_id=?', (run,))
                plan_json = plan_rows[0]['plan_json'] if len(plan_rows) == 1 else '{}'
                plan = json.loads(plan_json)
                items = plan.get('items', [])
                expected_kind = 'voice' if runtime else 'text'
                check('host expands both full sentences in order and enforces the configured pacing',
                      plan.get('normalized') is True
                      and [item['text'] for item in items] == SENTENCES
                      and [item['type'] for item in items] == [expected_kind] * 2
                      and [item['delay_ms'] for item in items] == [0, 1500])
                target = json.loads(result['reply_json'])
                check('reply preserves the synthetic owner and frozen passive reply target',
                      result['owner_id'].startswith('qq:' + batch.ACCOUNT + ':c2c:')
                      and target['message_id'] == incoming['d']['id']
                      and target['target'] == incoming['d']['author']['user_openid'])
                outboxes = batch.rows(h, 'SELECT * FROM outbox WHERE run_id=?', (run,))
                parts = batch.rows(h, 'SELECT seq,payload_json,delay_ms,status FROM outbox_reply_parts '
                                   'WHERE outbox_id=? ORDER BY seq', (outboxes[0]['id'],)) if len(outboxes) == 1 else []
                check('completed run atomically queues two ordered reply parts without dispatch',
                      len(outboxes) == 1 and outboxes[0]['status'] == 'queued'
                      and [part['seq'] for part in parts] == [1, 2]
                      and [part['status'] for part in parts] == ['queued', 'queued']
                      and [json.loads(part['payload_json']) for part in parts] == items)

                assets = batch.rows(h, 'SELECT * FROM speech_assets WHERE source_run_id=? ORDER BY id', (run,))
                if runtime:
                    measurements = []
                    for index, asset in enumerate(assets, 1):
                        metadata = json.loads(asset['metadata_json'])
                        audio = asset['audio']
                        with wave.open(io.BytesIO(audio), 'rb') as reader:
                            channels, width, rate, frames = (reader.getnchannels(), reader.getsampwidth(),
                                                             reader.getframerate(), reader.getnframes())
                        check(f'real Piper sentence {index} artifact has complete mono PCM and a matching digest',
                              (channels, width, rate) == (1, 2, 22050) and frames > 2205
                              and len(audio) == metadata['bytes']
                              and hashlib.sha256(audio).hexdigest() == metadata['sha256']
                              and asset['owner_id'] == result['owner_id']
                              and asset['conversation_id'] == result['conversation_id'])
                        measurements.append({key: metadata[key] for key in
                                             ['sha256', 'bytes', 'duration_ms', 'synthesis_ms']})
                    check('each expanded voice part owns its distinct source-bound Piper artifact',
                          len(assets) == 2
                          and {asset['id'] for asset in assets} == {item['asset_id'] for item in items})
                    journal, raw, _ = h.request('GET', API + '/runtime-logs?kind=tts&run_id=' + run)
                    check('Piper sentence completions are logged without copying spoken text',
                          {asset['id'] for asset in assets} <= {
                              item['details'].get('asset_id', item['details'].get('id'))
                              for item in journal['items'] if item['status'] == 'completed'}
                          and all(sentence.encode() not in raw for sentence in SENTENCES))
                    speech, _, _ = h.request('GET', API + '/speech')
                    check('Piper workers exit after both sentence syntheses', speech['active_workers'] == 0)
                    report.update(real_local_piper_tested=True, audio_artifacts=measurements)
                else:
                    check('disabled Piper preserves both sentences as readable text without audio artifacts', not assets)

                # A one-object proposal must not bypass the four-reply budget
                # by being truncated, merged or partially synthesized.
                server.output = '[{"type":"voice","text":"一句。两句。三句。四句。五句。"}]'
                invalid_chat = h.conversation('synthetic over-budget voice reply')
                invalid_run = h.submit(invalid_chat, 'synthetic over-budget voice probe')
                rejected = terminal(h, invalid_run)
                check('expanded voice overflow fails before audio creation or partial reply publication',
                      rejected['status'] == 'failed'
                      and not batch.rows(h, 'SELECT 1 FROM run_reply_plans WHERE run_id=?', (invalid_run,))
                      and not batch.rows(h, 'SELECT 1 FROM speech_assets WHERE source_run_id=?', (invalid_run,))
                      and not batch.rows(h, 'SELECT 1 FROM outbox WHERE run_id=?', (invalid_run,)))

                model_count = mock.count()
                h.stop()
                h.start()
                h.login()
                persisted = batch.rows(h, 'SELECT plan_json FROM run_reply_plans WHERE run_id=?', (run,))
                check('reply plan and queued sequence survive restart without replaying the model',
                      persisted[0]['plan_json'] == plan_json and mock.count() == model_count
                      and batch.rows(h, 'SELECT seq,status FROM outbox_reply_parts WHERE outbox_id=? ORDER BY seq',
                                     (outboxes[0]['id'],)) == [{'seq': 1, 'status': 'queued'}, {'seq': 2, 'status': 'queued'}])
                fence = batch.rows(h, 'SELECT next_ms FROM reply_account_pacing WHERE account_id=?', (batch.ACCOUNT,))
                check('original outbound fence prevents every QQ dispatch before and after restart',
                      fence[0]['next_ms'] == batch.PACING_FENCE_MS
                      and not batch.rows(h, "SELECT 1 FROM outbox WHERE status!='queued'")
                      and not batch.rows(h, "SELECT 1 FROM outbox_reply_parts WHERE status!='queued'"))
                report['status'] = 'passed'
                report['not_executed'] = ['QQ outbound HTTP upload and delivery', 'QQ client playback',
                                          'real external model request']
                if not runtime:
                    report['not_executed'].append('real local Piper synthesis: --runtime-root not supplied')
            finally:
                h.close()
    except Exception as error:
        code = 1
        report.update(status='failed', failed_phase=stage, error_type=type(error).__name__)
        print('FAIL ' + stage + ': ' + type(error).__name__, file=sys.stderr)
    finally:
        server.shutdown()
        server.server_close()
        report['finished_at'] = base.utc_now()
        destination = pathlib.Path(args.report)
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    return code


if __name__ == '__main__':
    raise SystemExit(main())
