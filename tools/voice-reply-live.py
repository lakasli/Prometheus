#!/usr/bin/env python3
"""Two synthetic real-model voice turns in a disposable, channel-free instance.

Reads only the active production model configuration and referenced credential.
Never reads production chats/memories or modifies production data. Uses the
reviewed personality asset and an installed Piper runtime, never real QQ output.
Reports contain metadata and check names, not credentials, text or audio.
"""
import argparse
import copy
import hashlib
import importlib.util
import io
import ipaddress
import json
import pathlib
import platform
import sqlite3
import sys
import tempfile
import threading
import time
import types
import urllib.parse
import urllib.request
import uuid
import wave
from contextlib import closing

sys.dont_write_bytecode = True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('binary', 'web-dir', 'source-data-dir', 'personality', 'runtime-root', 'report'):
        parser.add_argument('--' + name, required=True)
    args = parser.parse_args()
    spec = importlib.util.spec_from_file_location(
        'voice_live_harness', pathlib.Path(__file__).with_name('memory-smoke.py'))
    base = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(base)
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    source = pathlib.Path(args.source_data_dir).resolve(strict=True)
    runtime = pathlib.Path(args.runtime_root).resolve(strict=True)
    asset = pathlib.Path(args.personality).resolve(strict=True)
    personality = json.loads(asset.read_text(encoding='utf-8'))
    report = {
        'kind': 'isolated real-model voice reply acceptance',
        'started_at': base.utc_now(), 'status': 'running', 'commit': None,
        'architecture': platform.machine(), 'checks': [], 'voice_results': [],
        'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
        'personality_sha256': hashlib.sha256(asset.read_bytes()).hexdigest(),
        'model_request_budget': 2, 'model_requests': 0,
        'real_external_model_attempted': False, 'real_external_model_tested': False,
        'real_local_piper_tested': False, 'production_model_configuration_read': False,
        'production_model_credential_read': False, 'production_database_modified': False,
        'real_user_content_accessed': False, 'real_qq_tested': False,
        'real_weixin_tested': False, 'outbound_channels_enabled': False,
    }
    stage, code = 'initialize', 0

    def check(name, condition):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    try:
        with closing(sqlite3.connect((source / 'dsh.db').as_uri() + '?mode=ro', uri=True)) as db:
            db.execute('PRAGMA query_only=ON')
            row = db.execute("SELECT json_extract(c.config_json,'$.model') FROM config_revisions c "
                             "JOIN settings s ON s.key='active_revision' "
                             "AND c.revision=CAST(s.value AS INTEGER)").fetchone()
        if row is None:
            raise RuntimeError('active model unavailable')
        model = json.loads(row[0])
        report['production_model_configuration_read'] = True
        url = urllib.parse.urlsplit(model['base_url'])
        if url.scheme != 'https' or not url.hostname or url.hostname in {'localhost', 'localhost.localdomain'}:
            raise RuntimeError('external HTTPS provider required')
        try:
            address = ipaddress.ip_address(url.hostname)
        except ValueError:
            address = None
        if address is not None and not address.is_global:
            raise RuntimeError('private provider is not a real external model')
        secret_store = (source / 'secrets').resolve(strict=True)
        key_path = (secret_store / str(uuid.UUID(model['credential_ref']))).resolve(strict=True)
        if not key_path.is_relative_to(secret_store):
            raise RuntimeError('credential escaped private store')
        with tempfile.TemporaryDirectory(prefix='dsh-voice-reply-live-') as temporary:
            h = base.Harness(str(binary), str(web), temporary,
                             types.SimpleNamespace(hold_release=threading.Event()))
            h.environment.update(DSH_PIPER_EXECUTABLE=str(runtime / 'venv/bin/python'),
                                 DSH_PIPER_MODEL=str(runtime / 'models/zh_CN-huayan-medium.onnx'))
            try:
                h.start()
                h.bootstrap()
                report['commit'] = h.commit
                credential, _, _ = h.request('POST', base.PREFIX + '/secrets',
                                            {'kind': 'model_api_key', 'value': key_path.read_text()})
                report['production_model_credential_read'] = True
                current, _, _ = h.request('GET', base.PREFIX + '/config')
                config = copy.deepcopy(current['config'])
                config['model'] = dict(model, credential_ref=credential['id'], image_generation=False)
                config['assistant']['timeout_seconds'] = 60
                config['qq']['enabled'] = False
                config['speech'] = {'enabled': True, 'voice': 'zh_CN-huayan-medium',
                                    'timeout_seconds': 30, 'max_text_chars': 300}
                draft, _, _ = h.request('POST', base.PREFIX + '/config/drafts',
                                       {'base_revision': current['revision'], 'config': config})
                route = base.PREFIX + '/config/drafts/' + draft['id']
                validation, _, _ = h.request('POST', route + '/validate', {})
                check('temporary model and Piper configuration is valid with QQ disabled', validation['valid'])
                h.request('POST', route + '/apply', {}, expected=202,
                          headers={'If-Match': '"' + str(current['revision']) + '"'})
                original, _, _ = h.request('GET', base.PREFIX + '/assistant/personality')
                saved, _, _ = h.request('PUT', base.PREFIX + '/assistant/personality',
                                       {'personality': personality}, headers={'If-Match': original['etag']})
                check('reviewed Liu Jiahui personality is applied only to the temporary instance',
                      saved['personality'] == personality)
                speech, _, _ = h.request('GET', base.PREFIX + '/speech')
                check('installed real local Piper is available', speech['available'])
                probes = [
                    ('short voice request', '给我发个语音吧，我想听听你的声音。请只说一句简短的问候。'),
                    ('two-sentence voice request', '请用语音说这两句话：你好，我是刘嘉卉。今天也一起慢慢来吧。'),
                ]
                for label, question in probes:
                    if report['model_requests'] >= report['model_request_budget']:
                        raise RuntimeError('real-model request budget exhausted')
                    chat = h.conversation('Synthetic voice acceptance')
                    report['model_requests'] += 1
                    report['real_external_model_attempted'] = True
                    started = time.monotonic()
                    run = h.submit(chat, question)
                    deadline = time.monotonic() + 190
                    while time.monotonic() < deadline:
                        result, _, _ = h.request('GET', base.PREFIX + '/runs/' + run)
                        if result['status'] in base.TERMINAL:
                            break
                        time.sleep(0.2)
                    else:
                        raise TimeoutError('bounded real-model and Piper turn did not finish')
                    check(label + ': actual model request completes', result['status'] == 'completed')
                    report['real_external_model_tested'] = True
                    plan = result['reply_plan']
                    voices = [item for item in plan['items'] if item['type'] == 'voice']
                    check(label + ': result contains materialized voice artifacts',
                          bool(voices) and len(plan['items']) <= 4
                          and all(item.get('asset_id') for item in voices))
                    measurements = []
                    for item in voices:
                        audio_path = base.PREFIX + '/speech/assets/' + str(uuid.UUID(item['asset_id']))
                        request = urllib.request.Request(h.base + audio_path, headers={'Origin': h.base})
                        with h.client.open(request, timeout=15) as response:
                            audio = response.read(4_000_001)
                        with wave.open(io.BytesIO(audio), 'rb') as wav:
                            channels, width, rate, frames = (wav.getnchannels(), wav.getsampwidth(),
                                                             wav.getframerate(), wav.getnframes())
                        with h.db() as db:
                            stored = db.execute('SELECT source_run_id,metadata_json FROM speech_assets '
                                                'WHERE id=?', (item['asset_id'],)).fetchone()
                        metadata = json.loads(stored['metadata_json'])
                        check(label + ': real audio is complete private PCM bound to its source run',
                              (channels, width, rate) == (1, 2, 22050) and frames > 2205
                              and len(audio) <= 4_000_000 and stored['source_run_id'] == run
                              and hashlib.sha256(audio).hexdigest() == metadata['sha256'])
                        report['real_local_piper_tested'] = True
                        measurements.append({'bytes': len(audio), 'duration_ms': round(frames / rate * 1000),
                                             'sha256': metadata['sha256']})
                    report['voice_results'].append({'scenario': label, 'items': len(plan['items']),
                                                    'voice_items': len(voices), 'normalized': plan['normalized'],
                                                    'elapsed_ms': round((time.monotonic() - started) * 1000),
                                                    'audio': measurements})
                speech, _, _ = h.request('GET', base.PREFIX + '/speech')
                with h.db() as db:
                    outbox = db.execute('SELECT count(*) FROM outbox').fetchone()[0]
                    memory = db.execute('SELECT count(*) FROM memory_records').fetchone()[0]
                check('workers are reclaimed and synthetic voice turns create no outbound messages or memories',
                      speech['active_workers'] == 0 and outbox == 0 and memory == 0)
                report['status'] = 'passed'
            finally:
                h.close()
    except Exception as error:
        report.update(status='failed', failed_stage=stage, error_type=type(error).__name__)
        code = 1
    report['finished_at'] = base.utc_now()
    report['passed'] = len(report['checks'])
    pathlib.Path(args.report).write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({key: report[key] for key in ('status', 'passed', 'model_requests', 'production_database_modified')}))
    return code


if __name__ == '__main__':
    raise SystemExit(main())
