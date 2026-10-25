#!/usr/bin/env python3
"""Send three synthetic rich media replies only to an explicit QQ test request.

The source Run must begin with the fixed acceptance marker, belong to current
QQ C2C credentials, retain at least 120 seconds, and have only seq=1 sent.
Replies use seq=2 Piper WAV, seq=3 generated PNG, seq=4 an owned short MP4.
No unknown request is retried. Reports contain hashes and numeric API outcomes,
never credentials, recipient/message IDs, original responses, or user content.
"""
import argparse
import base64
import copy
import datetime
import hashlib
import importlib.util
import io
import json
import pathlib
import platform
import sqlite3
import struct
import tempfile
import threading
import time
import types
import urllib.error
import urllib.request
import uuid
import wave
import zlib
from contextlib import closing

PREFIX = '/api/admin/v1'
MARKER = 'DSH富媒体验收：'
MAX_FILE_BYTES = 5 * 1024 * 1024


class LocalApiFailure(RuntimeError):
    def __init__(self, status):
        self.http_status = status
        super().__init__('isolated acceptance API returned unexpected status')


def synthetic_png():
    width, height = 224, 128
    rows = []
    for y in range(height):
        row = bytearray([0])
        for x in range(width):
            circle = (x - 64) ** 2 + (y - 64) ** 2 < 36 ** 2
            square = 128 <= x < 192 and 32 <= y < 96
            color = (45, 112, 235) if circle else (255, 172, 43) if square else (242, 246, 252)
            row.extend(color)
        rows.append(bytes(row))
    def chunk(name, data):
        return struct.pack('>I', len(data)) + name + data + struct.pack('>I', zlib.crc32(name + data) & 0xffffffff)
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(b''.join(rows), 6)) + chunk(b'IEND', b'')


def boxes(data):
    offset = 0
    while offset < len(data):
        if len(data) - offset < 8:
            raise ValueError('invalid fixture container')
        size = int.from_bytes(data[offset:offset + 4], 'big')
        kind = data[offset + 4:offset + 8]
        header = 8
        if size == 1:
            if len(data) - offset < 16:
                raise ValueError('invalid fixture container')
            size = int.from_bytes(data[offset + 8:offset + 16], 'big')
            header = 16
        elif size == 0:
            size = len(data) - offset
        if size < header or offset + size > len(data):
            raise ValueError('invalid fixture container')
        yield kind, data[offset + header:offset + size]
        offset += size


def checked_video(path):
    path = pathlib.Path(path).resolve(strict=True)
    if path.suffix.lower() != '.mp4' or path.stat().st_size > MAX_FILE_BYTES:
        raise ValueError('owned fixture must be an MP4 below 5MiB')
    data = path.read_bytes()
    root = list(boxes(data))
    if not any(k == b'ftyp' for k, _ in root) or not any(k == b'mdat' and v for k, v in root):
        raise ValueError('owned fixture requires MP4 media data')
    movies = [v for k, v in root if k == b'moov']
    if len(movies) != 1:
        raise ValueError('owned fixture requires one movie')
    headers = [v for k, v in boxes(movies[0]) if k == b'mvhd']
    if len(headers) != 1:
        raise ValueError('owned fixture requires a movie clock')
    head = headers[0]
    if head[0] == 0 and len(head) >= 20:
        scale, duration = int.from_bytes(head[12:16], 'big'), int.from_bytes(head[16:20], 'big')
    elif head[0] == 1 and len(head) >= 32:
        scale, duration = int.from_bytes(head[20:24], 'big'), int.from_bytes(head[24:32], 'big')
    else:
        raise ValueError('owned fixture movie clock unsupported')
    if scale <= 0 or duration <= 0 or duration > 5 * scale:
        raise ValueError('owned fixture must be no longer than five seconds')
    return data, round(1000 * duration / scale)


def source_context(source, run_id):
    with closing(sqlite3.connect((source / 'dsh.db').as_uri() + '?mode=ro', uri=True)) as db:
        db.execute('PRAGMA query_only=ON')
        db.row_factory = sqlite3.Row
        config = db.execute("SELECT c.config_json FROM config_revisions c JOIN settings s ON s.key='active_revision' AND c.revision=CAST(s.value AS INTEGER)").fetchone()
        run = db.execute('SELECT owner_id,input,reply_json,status,created_at FROM runs WHERE id=?', (run_id,)).fetchone()
        outbox = db.execute('SELECT id,status,target_json,account_id,credential_ref,deadline FROM outbox WHERE run_id=?', (run_id,)).fetchone()
        if config is None or run is None or outbox is None:
            raise RuntimeError('explicit source test context missing')
        config = json.loads(config[0])
        qq = config['qq']
        target = json.loads(run['reply_json']) if run['reply_json'] else None
        if not str(run['input']).startswith(MARKER) or run['status'] != 'completed' or not isinstance(target, dict):
            raise RuntimeError('source Run is not the explicit completed media acceptance request')
        if target.get('kind') != 'c2c' or not qq.get('enabled'):
            raise RuntimeError('source test request is not enabled QQ C2C')
        if target.get('account_id') != qq['app_id'] or target.get('credential_ref') != qq['credential_ref']:
            raise RuntimeError('source account or credential changed')
        if outbox['account_id'] != qq['app_id'] or outbox['credential_ref'] != qq['credential_ref'] or json.loads(outbox['target_json']) != target:
            raise RuntimeError('source outbox does not match the frozen reply context')
        expected_owner = 'qq:' + qq['app_id'] + ':c2c:' + str(target.get('target', ''))
        if run['owner_id'] != expected_owner or not target.get('target') or not target.get('message_id'):
            raise RuntimeError('source reply ownership is invalid')
        if min(int(target['deadline']), int(outbox['deadline'])) < int(time.time()) + 120:
            raise RuntimeError('source test reply window has fewer than 120 seconds')
        if int(run['created_at']) > int(time.time()) + 60:
            raise RuntimeError('source test timestamp is invalid')
        parts = [tuple(row) for row in db.execute('SELECT seq,status FROM outbox_reply_parts WHERE outbox_id=? ORDER BY seq', (outbox['id'],))]
        if outbox['status'] != 'sent' or parts != [(1, 'sent')]:
            raise RuntimeError('source request must have exactly seq=1 confirmed sent')
    return qq, target


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--source-data-dir', required=True)
    parser.add_argument('--source-run-id', required=True)
    parser.add_argument('--runtime-root', required=True)
    parser.add_argument('--video-fixture', required=True)
    parser.add_argument('--report', required=True)
    args = parser.parse_args()
    source = pathlib.Path(args.source_data_dir).resolve(strict=True)
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    runtime = pathlib.Path(args.runtime_root).resolve(strict=True)
    run_id = str(uuid.UUID(args.source_run_id))
    destination = pathlib.Path(args.report).resolve()
    if destination.exists():
        print('BLOCKED prior media execution report exists; unknown sends are never replayed', flush=True)
        return 2
    report = {
        'kind': 'explicit QQ C2C synthetic voice image video acceptance',
        'started_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'commit': None, 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
        'source_run_sha256': hashlib.sha256(run_id.encode()).hexdigest(),
        'platform': platform.system(), 'architecture': platform.machine(),
        'production_database_modified': False, 'production_qq_configuration_read': False,
        'production_credential_read': False, 'existing_user_content_copied': False,
        'real_qq_send_attempted': False, 'synthetic_media_only': True,
        'client_verified': False, 'automatic_retries': False,
        'checks': [], 'media': [], 'status': 'running',
    }
    destination.parent.mkdir(parents=True, exist_ok=True)
    key, harness, temporary = None, None, None
    stage = 'verify explicit source test request'
    failed = False

    def save():
        # The file is a local durable send-intent marker. A rerun refuses to
        # overwrite it even if a preceding network request had an unknown result.
        destination.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')

    def check(name, condition=True):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    def call(method, path, body=None, expected=200, raw=False):
        headers = {'Origin': harness.base, 'Content-Type': 'application/json'}
        if method != 'GET':
            headers['X-CSRF-Token'] = harness.csrf
        request = urllib.request.Request(harness.base + path, None if body is None else json.dumps(body).encode(), headers, method=method)
        try:
            response = harness.client.open(request, timeout=60)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            data = response.read()
            status = response.code
            content_type = response.headers.get_content_type()
        if status != expected:
            raise LocalApiFailure(status)
        return (data, content_type) if raw else json.loads(data)

    try:
        qq, target = source_context(source, run_id)
        report['production_qq_configuration_read'] = True
        check('source marker owner account frozen deadline and exactly one sent reply verified')
        credential = str(uuid.UUID(qq['credential_ref']))
        store = (source / 'secrets').resolve(strict=True)
        secret = (store / credential).resolve(strict=True)
        if not secret.is_relative_to(store):
            raise RuntimeError('QQ credential escaped its private store')
        key = secret.read_text()
        report['production_credential_read'] = True
        video, video_duration = checked_video(args.video_fixture)
        check('owned synthetic MP4 fixture is bounded and no longer than five seconds')
        spec = importlib.util.spec_from_file_location('qq_media_acceptance_helpers', pathlib.Path(__file__).with_name('memory-smoke.py'))
        helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(helper)
        temporary = tempfile.TemporaryDirectory(prefix='dsh-qq-media-live-')
        harness = helper.Harness(str(binary), str(web), temporary.name, types.SimpleNamespace(hold_release=threading.Event()))
        harness.environment.update(DSH_PIPER_EXECUTABLE=str(runtime / 'venv/bin/python'), DSH_PIPER_MODEL=str(runtime / 'models/zh_CN-huayan-medium.onnx'))
        harness.start()
        report['commit'] = report['deployment_sha'] = harness.commit
        harness.bootstrap()
        current = call('GET', PREFIX + '/config')
        stored = call('POST', PREFIX + '/secrets', {'kind': 'qq_app_secret', 'value': key})
        key = None
        config = copy.deepcopy(current['config'])
        config['qq'] = dict(qq, enabled=True, transport='webhook', credential_ref=stored['id'])
        config['speech'] = {'enabled': True, 'voice': 'zh_CN-huayan-medium', 'timeout_seconds': 30, 'max_text_chars': 300}
        config['model']['credential_ref'] = None
        draft = call('POST', PREFIX + '/config/drafts', {'base_revision': current['revision'], 'config': config})
        route = PREFIX + '/config/drafts/' + draft['id']
        check('private webhook-only account and local Piper configuration validate', call('POST', route + '/validate', {})['valid'])
        harness.request('POST', route + '/apply', {}, expected=202, headers={'If-Match': '"' + str(current['revision']) + '"'})
        health = call('GET', PREFIX + '/speech')
        check('real local Piper runtime is available', health['available'])
        asset = call('POST', PREFIX + '/speech/syntheses', {'text': '你好，这是树莓派在本地合成的语音测试。'}, expected=201)
        wav, wav_type = call('GET', PREFIX + '/speech/assets/' + asset['id'], raw=True)
        with wave.open(io.BytesIO(wav), 'rb') as audio:
            duration = round(1000 * audio.getnframes() / audio.getframerate())
            check('backend Piper asset is mono PCM WAV with matching SHA256', wav_type == 'audio/wav' and audio.getnchannels() == 1 and audio.getsampwidth() == 2 and hashlib.sha256(wav).hexdigest() == asset['sha256'])
        media = [('voice', 2, 3, 'dsh-piper-acceptance.wav', wav, duration),
                 ('image', 3, 1, 'dsh-synthetic-acceptance.png', synthetic_png(), None),
                 ('video', 4, 2, 'dsh-synthetic-acceptance.mp4', video, video_duration)]
        check('all synthetic files fit the documented direct-upload bound', all(0 < len(data) <= MAX_FILE_BYTES for _, _, _, _, data, _ in media))
        save()
        blocked = False
        for kind, seq, file_type, name, data, duration_ms in media:
            item = {'kind': kind, 'seq': seq, 'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest(), 'duration_ms': duration_ms, 'platform_accepted': False, 'client_verified': False}
            report['media'].append(item)
            if blocked:
                item['outcome'] = 'blocked_by_prior_unknown_delivery'
                failed = True
                save()
                continue
            # Re-read only the frozen synthetic source and current QQ identity;
            # a disabled/switched production account aborts further sends.
            latest_qq, latest_target = source_context(source, run_id)
            check('authorized source context remains unchanged for ' + kind, latest_qq == qq and latest_target == target)
            stage = kind + ' upload'
            item['upload_intent_recorded'] = True
            save()
            upload = call('POST', PREFIX + '/qq/api/operations/post_v2_users_user_openid_files', {
                'parameters': {'user_openid': target['target']}, 'acknowledge_effects': True,
                'body': {'file_type': file_type, 'file_name': name, 'file_data': base64.b64encode(data).decode('ascii'), 'srv_send_msg': False}})
            item['upload'] = {key: upload.get(key) for key in ['outcome', 'http_status', 'business_code']}
            upload_response = upload.get('response')
            if not upload.get('platform_accepted') or not upload.get('object_ref') or not isinstance(upload_response, dict) or 'file_info' not in upload_response:
                item['outcome'] = 'upload_not_accepted'
                failed = True
                save()
                print('QQ ' + kind + ': upload_not_accepted HTTP=' + str(item['upload'].get('http_status')) + ' code=' + str(item['upload'].get('business_code')), flush=True)
                continue
            stage = kind + ' message submit'
            item['send_intent_recorded'] = True
            report['real_qq_send_attempted'] = True
            save()
            sent = call('POST', PREFIX + '/qq/api/operations/post_v2_users_user_openid_messages', {
                'parameters': {'user_openid': target['target']}, 'acknowledge_effects': True,
                'body': {'msg_type': 7, 'content': ' ', 'media': {'file_info': {'$qq_ref': upload['object_ref'], 'field': 'file_info'}}, 'msg_id': target['message_id'], 'msg_seq': seq}})
            item['send'] = {key: sent.get(key) for key in ['outcome', 'http_status', 'business_code']}
            sent_response = sent.get('response')
            has_message_id = isinstance(sent_response, dict) and isinstance(sent_response.get('id'), str) and bool(sent_response['id'])
            item['outcome'] = sent.get('outcome', 'result_unknown')
            item['platform_accepted'] = bool(sent.get('platform_accepted')) and has_message_id
            if sent.get('platform_accepted') and not has_message_id:
                item['outcome'] = 'result_unknown'
            if not item['platform_accepted']:
                failed = True
                blocked = item['outcome'] in {'result_unknown', 'asynchronous_pending'}
            save()
            print('QQ ' + kind + ': ' + str(item['outcome']) + ' HTTP=' + str(item['send'].get('http_status')) + ' code=' + str(item['send'].get('business_code')), flush=True)
            time.sleep(3)
        report['status'] = 'failed' if failed else 'platform_accepted_client_verification_pending'
    except Exception as error:
        failed = True
        report['status'] = 'failed'
        report['failure'] = {'stage': stage, 'exception_type': type(error).__name__}
        if isinstance(error, LocalApiFailure):
            report['failure']['local_http_status'] = error.http_status
        if report['media']:
            item = report['media'][-1]
            if item.get('send_intent_recorded') and 'send' not in item:
                item['outcome'] = 'admin_request_rejected' if isinstance(error, LocalApiFailure) and error.http_status < 500 else 'result_unknown'
            elif item.get('upload_intent_recorded') and 'upload' not in item:
                item['outcome'] = 'upload_request_failed_result_unconfirmed'
        print('FAIL ' + stage + ' (' + type(error).__name__ + ')', flush=True)
    finally:
        key = None
        if harness:
            harness.close()
        if temporary:
            temporary.cleanup()
        report['finished_at'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        save()
    return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
