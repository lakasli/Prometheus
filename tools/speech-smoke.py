#!/usr/bin/env python3
"""Authenticated Piper management and optional real local Chinese synthesis.

Runs a private temporary daemon with synthetic accounts/text only. No production
configuration, model API credential, message, QQ account or user memory is read.
--runtime-root selects an already installed, hash-locked local Piper runtime.
Reports contain only hashes, measurements and check names; never text/audio.
"""
import argparse
import copy
import hashlib
import importlib.util
import io
import json
import os
import pathlib
import platform
import secrets
import sys
import tempfile
import threading
import time
import types
import urllib.request
import uuid
import wave

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('speech_acceptance_helpers', pathlib.Path(__file__).with_name('memory-smoke.py'))
helpers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helpers)
PREFIX = helpers.PREFIX
ROOT = PREFIX + '/speech'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--runtime-root')
    parser.add_argument('--report', required=True)
    args = parser.parse_args()
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    runtime = pathlib.Path(args.runtime_root).resolve(strict=True) if args.runtime_root else None
    report = {'kind': 'isolated Piper TTS acceptance', 'started_at': helpers.utc_now(), 'status': 'running',
              'platform': platform.system(), 'architecture': platform.machine(), 'commit': None,
              'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'production_database_modified': False, 'real_user_content_accessed': False,
              'real_qq_tested': False, 'real_local_piper_tested': False, 'checks': []}
    phase = 'initialize'

    def check(name, condition):
        nonlocal phase
        phase = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    def apply(h, speech):
        current, _, _ = h.request('GET', PREFIX + '/config')
        config = copy.deepcopy(current['config'])
        config['speech'] = speech
        draft, _, _ = h.request('POST', PREFIX + '/config/drafts', {'base_revision': current['revision'], 'config': config})
        route = PREFIX + '/config/drafts/' + draft['id']
        result, _, _ = h.request('POST', route + '/validate', {})
        check('speech configuration validates without a cloud credential', result['valid'])
        h.request('POST', route + '/apply', {}, expected=202, headers={'If-Match': '"' + str(current['revision']) + '"'})
        h.request('POST', route + '/apply', {}, expected=409, headers={'If-Match': '"' + str(current['revision']) + '"'})
        return config

    exit_code = 0
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-piper-smoke-') as temporary:
            h = helpers.Harness(str(binary), str(web), temporary, types.SimpleNamespace(hold_release=threading.Event()))
            h.environment.update(DSH_PIPER_EXECUTABLE=str(pathlib.Path(temporary) / 'absent-python'),
                                 DSH_PIPER_MODEL=str(pathlib.Path(temporary) / 'absent-model'))
            try:
                h.start()
                report['commit'] = h.commit
                h.bootstrap()
                h.request('GET', ROOT, expected=401, authenticated=False)
                h.request('POST', ROOT + '/syntheses', {'text': '合成测试。'}, expected=403, with_csrf=False)
                h.request('POST', ROOT + '/syntheses', {'text': '合成测试。'}, expected=409)
                check('speech authentication CSRF and disabled guard reject requests', True)
                page, raw, _ = h.request('GET', ROOT)
                check('speech diagnostics expose no paths or credentials and no resident worker', not page['runtime_present'] and page['active_workers'] == 0 and str(pathlib.Path(temporary)).encode() not in raw)
                config = apply(h, {'enabled': True, 'voice': 'zh_CN-huayan-medium', 'timeout_seconds': 30, 'max_text_chars': 300})
                h.request('POST', ROOT + '/syntheses', {'text': '合成测试。'}, expected=503)
                h.request('POST', ROOT + '/syntheses', {'text': '字' * 301}, expected=400)
                h.request('POST', ROOT + '/syntheses', {'text': '```code```'}, expected=400)
                h.request('POST', ROOT + '/syntheses', {'text': '合成测试。', 'executable': '/bin/sh'}, expected=422)
                check('missing runtime limits code-only text and arbitrary executable input fail closed', True)
                current, _, _ = h.request('GET', PREFIX + '/config')
                invalid = copy.deepcopy(current['config'])
                invalid['speech']['voice'] = '../../untrusted'
                draft, _, _ = h.request('POST', PREFIX + '/config/drafts', {'base_revision': current['revision'], 'config': invalid})
                result, _, _ = h.request('POST', PREFIX + '/config/drafts/' + draft['id'] + '/validate', {})
                check('voice cannot select arbitrary files or undeployed models', not result['valid'])

                if runtime:
                    h.stop()
                    h.environment.update(DSH_PIPER_EXECUTABLE=str(runtime / 'venv/bin/python'), DSH_PIPER_MODEL=str(runtime / 'models/zh_CN-huayan-medium.onnx'))
                    h.start()
                    h.login()
                    page, _, _ = h.request('GET', ROOT)
                    check('installed local runtime is available after restart', page['available'])
                    started = time.monotonic()
                    asset, _, _ = h.request('POST', ROOT + '/syntheses', {'text': '你好，这是树莓派上的本地中文语音测试。'}, expected=201)
                    request = urllib.request.Request(h.base + asset['audio_url'], headers={'Origin': h.base})
                    with h.client.open(request, timeout=15) as response:
                        audio, headers = response.read(), response.headers
                    with wave.open(io.BytesIO(audio), 'rb') as reader:
                        channels, width, rate, frames = reader.getnchannels(), reader.getsampwidth(), reader.getframerate(), reader.getnframes()
                    check('real Piper returns complete mono PCM Chinese speech with matching digest', (channels, width, rate) == (1, 2, 22050) and frames > 2205 and hashlib.sha256(audio).hexdigest() == asset['sha256'] and len(audio) == asset['bytes'])
                    check('private audio is no-store WAV and requires authentication', headers.get('Content-Type') == 'audio/wav' and 'no-store' in headers.get('Cache-Control', ''))
                    h.request('GET', asset['audio_url'], expected=401, authenticated=False)
                    token = secrets.token_hex(32)
                    with h.db() as db:
                        db.execute("INSERT INTO admins VALUES('other-speech-owner','other-speech-owner','unused',0)")
                        db.execute('INSERT INTO sessions VALUES(?,?,?,?)', (hashlib.sha256(token.encode()).hexdigest(), 'other-speech-owner', 'unused', int(time.time()) + 60))
                    h.request('GET', asset['audio_url'], expected=404, authenticated=False, headers={'Cookie': 'dsh_session=' + token})
                    check('a second authenticated administrator cannot read another owners audio', True)
                    with h.db() as db:
                        retained_text = db.execute("SELECT count(*) FROM speech_assets WHERE instr(metadata_json,?)>0", ('你好，这是树莓派上的本地中文语音测试。',)).fetchone()[0]
                        audit_text = db.execute("SELECT count(*) FROM audit_events WHERE instr(resource,?)>0", ('你好，这是树莓派上的本地中文语音测试。',)).fetchone()[0]
                    h.log.flush()
                    check('spoken text is absent from persisted artifact metadata audit records and daemon logs', retained_text == 0 and audit_text == 0 and '你好，这是树莓派上的本地中文语音测试。'.encode() not in (pathlib.Path(temporary) / 'daemon.log').read_bytes())
                    page, _, _ = h.request('GET', ROOT)
                    check('Piper worker exits and returns its only admission slot after synthesis', page['active_workers'] == 0)
                    disposable = copy.deepcopy(asset)
                    disposable['id'] = str(uuid.uuid4())
                    disposable['audio_url'] = ROOT + '/assets/' + disposable['id']
                    with h.db() as db:
                        db.execute('INSERT INTO speech_assets(id,owner_id,conversation_id,context_floor,metadata_json,audio,created_at,expires_at) VALUES(?,?,NULL,0,?,?,?,?)', (disposable['id'], h.owner, json.dumps(disposable), audio, int(time.time()), int(time.time()) + 60))
                    h.request('DELETE', disposable['audio_url'], expected=403, with_csrf=False)
                    h.request('DELETE', disposable['audio_url'])
                    h.request('GET', disposable['audio_url'], expected=404)
                    check('explicit audio cleanup requires CSRF and removes the private artifact', True)
                    with h.db() as db:
                        db.execute('UPDATE speech_assets SET expires_at=0 WHERE id=?', (asset['id'],))
                    h.request('GET', asset['audio_url'], expected=404)
                    with h.db() as db:
                        retained = db.execute('SELECT count(*) FROM speech_assets').fetchone()[0]
                    check('expired audio is inaccessible and pruned', retained == 0)
                    apply(h, {'enabled': True, 'voice': 'zh_CN-huayan-medium', 'timeout_seconds': 5, 'max_text_chars': 400})
                    # A deterministic local process fixture exercises admission
                    # and reaping. It makes no Piper performance claim and never
                    # touches the installed runtime or production service.
                    h.stop()
                    slow = pathlib.Path(temporary) / 'slow-speech-fixture.py'
                    slow.write_text('#!/usr/bin/python3\nimport os,pathlib,sys,time\nsys.stdin.buffer.read()\npathlib.Path(__file__).with_suffix(".pid").write_text(str(os.getpid()))\ntime.sleep(30)\n', encoding='utf-8')
                    slow.chmod(0o700)
                    h.environment['DSH_PIPER_EXECUTABLE'] = str(slow)
                    h.start()
                    h.login()
                    held = {'error': None, 'status': None}

                    def overlong_synthesis():
                        try:
                            h.request('POST', ROOT + '/syntheses', {'text': '合成进程超时控制测试。'}, expected=504)
                            held['status'] = 'timeout_rejected'
                        except Exception as cause:
                            held['error'] = type(cause).__name__

                    task = threading.Thread(target=overlong_synthesis, daemon=True)
                    task.start()
                    deadline = time.monotonic() + 3
                    while time.monotonic() < deadline:
                        page, _, _ = h.request('GET', ROOT)
                        if page['active_workers'] == 1:
                            break
                        time.sleep(0.03)
                    check('a controlled slow process holds the single worker admission lease', page['active_workers'] == 1)
                    h.request('POST', ROOT + '/syntheses', {'text': '并发测试。'}, expected=409)
                    task.join(timeout=12)
                    page, _, _ = h.request('GET', ROOT)
                    worker_pid = int(slow.with_suffix('.pid').read_text())
                    exited = not pathlib.Path('/proc', str(worker_pid)).exists() if os.name == 'posix' else True
                    check('controlled process fixture rejects concurrency and timeout reaps its exact PID before releasing admission', not task.is_alive() and held['status'] == 'timeout_rejected' and held['error'] is None and page['active_workers'] == 0 and exited)
                    report['timeout_fixture'] = 'private synthetic sleeping process; not a Piper latency measurement'
                    report.update(real_local_piper_tested=True, audio_sha256=asset['sha256'], audio_bytes=len(audio),
                                  audio_duration_ms=asset['duration_ms'], synthesis_ms=asset['synthesis_ms'],
                                  elapsed_ms=round((time.monotonic() - started) * 1000), sample_rate=rate)
                else:
                    report['not_executed'] = ['real local Piper synthesis: --runtime-root not supplied', 'QQ platform delivery']
                report['status'] = 'passed'
            finally:
                h.close()
    except Exception as error:
        exit_code = 1
        report.update(status='failed', failed_phase=phase, error_type=type(error).__name__)
        print('FAIL ' + phase + ': ' + type(error).__name__, file=sys.stderr)
    finally:
        report['finished_at'] = helpers.utc_now()
        destination = pathlib.Path(args.report)
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    return exit_code


if __name__ == '__main__':
    raise SystemExit(main())
