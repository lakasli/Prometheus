#!/usr/bin/env python3
"""Standalone PNG analysis acceptance in a real, private temporary daemon.

The provider is numeric loopback, explicitly enabled by --allow-private-model.
Images, questions, credentials and database fixtures are synthetic. No platform
account is connected. Reports contain metadata and checks, never images or text.
"""
import argparse
import base64
import binascii
import hashlib
import http.server
import importlib.util
import json
import pathlib
import platform
import re
import secrets
import sqlite3
import struct
import subprocess
import sys
import tempfile
import threading
import time
import uuid
import zlib
from contextlib import closing

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    'image_candidate_helpers', pathlib.Path(__file__).with_name('memory-candidate-smoke.py'))
helpers = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(helpers)
PREFIX = helpers.PREFIX
ROOT = PREFIX + '/image-analysis'
TERMINAL = {'completed', 'failed', 'cancelled', 'interrupted', 'expired', 'deleted'}
SIGNATURE = b'\x89PNG\r\n\x1a\n'


def chunk(kind, content):
    return (struct.pack('!I', len(content)) + kind + content +
            struct.pack('!I', binascii.crc32(kind + content) & 0xffffffff))


def chunks(picture):
    if not picture.startswith(SIGNATURE):
        raise AssertionError('PNG signature missing')
    position, result = 8, []
    while position < len(picture):
        size = struct.unpack('!I', picture[position:position + 4])[0]
        end = position + size + 12
        kind, content = picture[position + 4:position + 8], picture[position + 8:end - 4]
        if end > len(picture) or len(content) != size:
            raise AssertionError('PNG fixture is truncated')
        checksum = struct.unpack('!I', picture[end - 4:end])[0]
        if checksum != binascii.crc32(kind + content) & 0xffffffff:
            raise AssertionError('PNG fixture CRC mismatch')
        result.append((kind, content))
        position = end
    return result


def png(width=8, height=8, color=(30, 80, 160), color_type=2, metadata=()):
    channels = {0: 1, 2: 3, 4: 2, 6: 4}[color_type]
    sample = bytes(color[:channels])
    raw = (b'\0' + sample * width) * height
    return (SIGNATURE + chunk(b'IHDR', struct.pack('!2I5B', width, height, 8, color_type, 0, 0, 0)) +
            b''.join(chunk(kind, value) for kind, value in metadata) +
            chunk(b'IDAT', zlib.compress(raw, 9)) + chunk(b'IEND', b''))


def add_metadata(picture, marker):
    source = chunks(picture)
    extra = [(b'tEXt', b'comment\0' + marker),
             (b'iTXt', b'comment\0\0\0\0\0' + marker),
             (b'eXIf', b'II*\0' + marker),
             (b'iCCP', b'profile\0\0' + zlib.compress(marker))]
    return SIGNATURE + b''.join(chunk(kind, data) for kind, data in source[:1] + extra + source[1:])


def exact_size_png(size):
    picture = png()
    padding = size - len(picture) - 12
    if padding < 2:
        raise AssertionError('requested fixture size is too small')
    source = chunks(picture)
    metadata = chunk(b'tEXt', b'x\0' + b'p' * (padding - 2))
    return SIGNATURE + chunk(*source[0]) + metadata + b''.join(chunk(*item) for item in source[1:])


def rgba_pixels(picture):
    entries = chunks(picture)
    width, height, depth, color, compression, filtering, interlace = struct.unpack('!2I5B', entries[0][1])
    if depth != 8 or color not in {0, 2, 4, 6} or compression or filtering or interlace:
        raise AssertionError('unexpected canonical PNG format')
    channels = {0: 1, 2: 3, 4: 2, 6: 4}[color]
    stride = width * channels
    stream = b''.join(data for kind, data in entries if kind == b'IDAT')
    decoder = zlib.decompressobj()
    raw = decoder.decompress(stream, 10 * 1024 * 1024)
    if len(raw) != (stride + 1) * height or not decoder.eof:
        raise AssertionError('unexpected canonical PNG decoded size')
    previous = bytearray(stride)
    pixels = bytearray()

    def paeth(left, above, upper_left):
        prediction = left + above - upper_left
        distances = (abs(prediction - left), abs(prediction - above), abs(prediction - upper_left))
        return (left, above, upper_left)[distances.index(min(distances))]

    for row_index in range(height):
        start = row_index * (stride + 1)
        mode, row = raw[start], bytearray(raw[start + 1:start + stride + 1])
        for index in range(stride):
            left = row[index - channels] if index >= channels else 0
            above = previous[index]
            upper_left = previous[index - channels] if index >= channels else 0
            if mode == 1:
                row[index] = (row[index] + left) & 255
            elif mode == 2:
                row[index] = (row[index] + above) & 255
            elif mode == 3:
                row[index] = (row[index] + (left + above) // 2) & 255
            elif mode == 4:
                row[index] = (row[index] + paeth(left, above, upper_left)) & 255
            elif mode != 0:
                raise AssertionError('unsupported PNG row filter')
        for index in range(0, stride, channels):
            sample = row[index:index + channels]
            if color == 6:
                pixels.extend(sample)
            elif color == 2:
                pixels.extend(sample + b'\xff')
            elif color == 4:
                pixels.extend(bytes([sample[0], sample[0], sample[0], sample[1]]))
            else:
                pixels.extend(bytes([sample[0], sample[0], sample[0], 255]))
        previous = row
    return width, height, bytes(pixels)


class Mock:
    def __init__(self):
        self.lock = threading.Lock()
        self.plans = {}
        self.requests = []
        self.hold_release = threading.Event()
        self.hold_received = threading.Event()
        self.hold_responded = threading.Event()
        self.active = self.active_vision = self.maximum = self.maximum_vision = 0

    def arm(self, question, mode='normal', held=False):
        with self.lock:
            self.plans[question] = {'mode': mode, 'held': held}
        if held:
            self.hold_release.clear()
            self.hold_received.clear()
            self.hold_responded.clear()

    def visions(self):
        with self.lock:
            return [item for item in self.requests if item['vision']]

    def count(self):
        return len(self.visions())

    def respond(self, payload, authorization):
        messages = payload.get('messages') or []
        vision = bool(messages and messages[0].get('content', '').startswith('DSH_STANDALONE_IMAGE_ANALYSIS_V1'))
        content = messages[-1].get('content', '') if messages else ''
        question = next((part.get('text', '') for part in content if part.get('type') == 'text'), '') if isinstance(content, list) else content
        with self.lock:
            plan = self.plans.get(question, {'mode': 'normal', 'held': False})
            self.requests.append({'vision': vision, 'payload': payload, 'question': question,
                                  'authorization_sha256': hashlib.sha256(authorization.encode()).hexdigest()})
            self.active += 1
            self.active_vision += int(vision)
            self.maximum = max(self.maximum, self.active)
            self.maximum_vision = max(self.maximum_vision, self.active_vision)
        held = vision and plan['held']
        try:
            if held:
                self.hold_received.set()
                self.hold_release.wait(timeout=40)
            mode = plan['mode'] if vision else 'normal'
            answer = 'Synthetic image analysis completed.' if vision else 'SYNTHETIC-CHAT-ACK'
            if not vision and messages and messages[0].get('content', '').startswith('DSH_MEMORY_CANDIDATE_EXTRACTION_V1'):
                answer = '{"candidates":[]}'
            if mode == 'empty':
                answer = ''
            elif mode == 'too_large':
                answer = '鲸' * 2700
            elif mode == 'null_byte':
                answer = 'synthetic\0output'
            if mode == 'malformed':
                return 200, b'{not-json'
            if mode == 'missing_content':
                return 200, b'{"choices":[]}'
            if mode == 'body_limit':
                return 200, b' ' * 65537
            if mode == 'http_failure':
                return 503, b'{"error":"synthetic unavailable"}'
            return 200, json.dumps({'choices': [{'message': {'role': 'assistant', 'content': answer}}]}).encode()
        finally:
            with self.lock:
                self.active -= 1
                self.active_vision -= int(vision)
            if held:
                self.hold_responded.set()


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        size = int(self.headers.get('Content-Length', '0'))
        if not 0 < size <= 4 * 1024 * 1024:
            self.send_error(413)
            return
        payload = json.loads(self.rfile.read(size))
        status, raw = self.server.mock.respond(payload, self.headers.get('Authorization', ''))
        try:
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass


def body(picture, question, client=None):
    return {'client_request_id': client or str(uuid.uuid4()),
            'image_base64': base64.b64encode(picture).decode(), 'question': question}


def create(harness, picture, question, client=None, expected=202):
    return harness.call('POST', ROOT + '/jobs', body(picture, question, client), expected=expected)[0]


def job_path(identifier):
    return ROOT + '/jobs/' + identifier


def wait_job(harness, identifier, via_api=True):
    deadline = time.monotonic() + 38
    while time.monotonic() < deadline:
        if via_api:
            value, _, headers = harness.call('GET', job_path(identifier))
            if headers.get('ETag') != '"' + str(value['revision']) + '"':
                raise AssertionError('job ETag does not match integer revision')
        else:
            with harness.db() as database:
                row = database.execute('SELECT * FROM image_analysis_jobs WHERE id=?', (identifier,)).fetchone()
            value = dict(row) if row else None
        if value and value['status'] in TERMINAL:
            return value
        time.sleep(0.05)
    raise TimeoutError('image job did not reach a terminal state')


def private_row(harness, identifier):
    with harness.db() as database:
        return dict(database.execute('SELECT * FROM image_analysis_jobs WHERE id=?', (identifier,)).fetchone())


def public_image(payload):
    blocks = payload['messages'][-1]['content']
    link = next(item['image_url']['url'] for item in blocks if item['type'] == 'image_url')
    if not link.startswith('data:image/png;base64,'):
        raise AssertionError('provider image is not an inline canonical PNG')
    return base64.b64decode(link.partition(',')[2], validate=True)


def unrelated_counts(harness):
    with harness.db() as database:
        return {table: database.execute('SELECT count(*) FROM ' + table).fetchone()[0]
                for table in ['runs', 'messages', 'memory_records', 'summary_jobs', 'memory_candidate_jobs']}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report')
    args = parser.parse_args()
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    with binary.open('rb') as handle:
        binary_hash = hashlib.file_digest(handle, 'sha256').hexdigest()
    report = {'kind': 'standalone image-analysis real-process acceptance with isolated mock',
              'started_at': helpers.acceptance.utc_now(), 'commit': None, 'deployment_sha': None,
              'binary_sha256': binary_hash, 'platform': platform.system(), 'architecture': platform.machine(),
              'status': 'running', 'real_external_vision_tested': False, 'real_user_images_accessed': False,
              'production_data_accessed': False, 'real_qq_tested': False, 'real_weixin_private_chat_tested': False,
              'synthetic_image_fixtures_used': True, 'checks': []}
    stage = 'setup'

    def check(name, condition):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    mock = Mock()
    provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    provider.daemon_threads = True
    provider.mock = mock
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    failed = False
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-image-analysis-smoke-') as temporary:
            harness = helpers.Harness(str(binary), str(web), temporary, mock)
            try:
                harness.start()
                report['commit'] = report['deployment_sha'] = harness.commit
                harness.call('GET', ROOT, expected=401, authenticated=False)
                harness.bootstrap()
                initial, _, _ = harness.call('GET', ROOT)
                with harness.db() as database:
                    initial_schema = database.execute('SELECT max(version) FROM dsh_schema_migrations').fetchone()[0]
                check('empty image workspace requires auth and stays unavailable without credentials',
                      initial['items'] == [] and not initial['available'] and not initial['credential_configured']
                      and initial['provider'] == 'DeepSeek' and initial['model'] == 'deepseek-flash'
                      and initial_schema < 6)
                harness.configure(provider.server_port)
                page, _, _ = harness.call('GET', ROOT)
                check('explicit loopback test configuration enables fixed-model vision with bounded policy',
                      page['available'] and page['credential_configured']
                      and all(page['policy'].get(key) == value for key, value in {
                          'max_image_bytes': 1048576, 'max_width': 2048, 'max_height': 2048,
                          'max_pixels': 2097152, 'max_question_bytes': 2000, 'result_ttl_seconds': 86400}.items()))
                picture = png()
                request = body(picture, '合成图片验收问题。')
                calls = mock.count()
                harness.call('POST', ROOT + '/jobs', request, expected=401, authenticated=False)
                harness.call('POST', ROOT + '/jobs', request, expected=403, with_csrf=False)
                harness.call('POST', ROOT + '/jobs', request, expected=403, headers={'Origin': 'https://example.invalid'})
                check('image submission enforces authentication Origin and CSRF before provider work', mock.count() == calls)
                bad_question = ['SyntheticPrivateQuestion' + secrets.token_hex(8)]
                invalid_bodies = [dict(request, client_request_id='not-a-uuid'), dict(request, question=''),
                                  dict(request, question='文' * 667), dict(request, question='bad\0question'),
                                  dict(request, question=bad_question), dict(request, remote_url='https://example.invalid/image.png')]
                for invalid in invalid_bodies:
                    _, raw, _ = harness.call('POST', ROOT + '/jobs', invalid, expected=(400, 422))
                    if bad_question[0].encode() in raw:
                        raise AssertionError('validation response echoes private question')
                check('UUID question-byte NUL type and unknown-field validation never schedules vision', mock.count() == calls)

                source_chunks = chunks(picture)
                ihdr, idat, iend = source_chunks
                half = len(idat[1]) // 2
                invalid_pngs = {
                    'non_png': b'GIF89a synthetic',
                    'truncated': picture[:-5],
                    'bad_crc': picture[:-1] + bytes([picture[-1] ^ 1]),
                    'trailing_bytes': picture + b'synthetic trailing data',
                    'missing_iend': picture[:-12],
                    'duplicate_iend': picture + chunk(b'IEND', b''),
                    'unknown_critical': SIGNATURE + chunk(*ihdr) + chunk(b'ABCD', b'x') + chunk(*idat) + chunk(*iend),
                    'apng': SIGNATURE + chunk(*ihdr) + chunk(b'acTL', struct.pack('!2I', 1, 0)) + chunk(*idat) + chunk(*iend),
                    'bad_idat': SIGNATURE + chunk(*ihdr) + chunk(b'IDAT', b'not-zlib') + chunk(*iend),
                    'split_idat': SIGNATURE + chunk(*ihdr) + chunk(b'IDAT', idat[1][:half]) +
                                  chunk(b'tEXt', b'x\0y') + chunk(b'IDAT', idat[1][half:]) + chunk(*iend),
                    'zero_dimension': png(width=0),
                    'width_limit': png(width=2049, height=1),
                    'height_limit': png(width=1, height=2049),
                    'pixel_limit': png(width=2048, height=1025),
                    'raw_byte_limit': exact_size_png(1048577),
                }
                for name, invalid in invalid_pngs.items():
                    create(harness, invalid, '合成拒绝格式 ' + name, expected=400)
                for encoded in ['data:image/png;base64,' + request['image_base64'], '%%%not-base64%%%',
                                'https://example.invalid/image.png']:
                    harness.call('POST', ROOT + '/jobs', dict(request, image_base64=encoded), expected=400)
                check('strict PNG rejects fifteen structural resource and format boundaries before model work',
                      mock.count() == calls)

                baseline = unrelated_counts(harness)
                marker = ('SYNTHETIC-PNG-METADATA-' + secrets.token_hex(12)).encode()
                private_png = add_metadata(picture, marker)
                question = '合成元数据清理和凭据冻结检查。'
                client = str(uuid.uuid4())
                mock.arm(question, held=True)
                first = create(harness, private_png, question, client)
                if not mock.hold_received.wait(timeout=10):
                    raise AssertionError('metadata image request was not observed')
                row = private_row(harness, first['id'])
                canonical = row['image']
                transmitted = public_image(mock.visions()[-1]['payload'])
                check('PNG metadata is stripped before private storage and provider transmission',
                      canonical == transmitted and marker not in canonical and marker not in transmitted
                      and {kind for kind, _ in chunks(canonical)} <= {b'IHDR', b'IDAT', b'IEND'}
                      and chunks(canonical)[0][1][8:10] == bytes([8, 6])
                      and rgba_pixels(canonical) == rgba_pixels(picture)
                      and len(canonical) <= 2097152 and hashlib.sha256(canonical).hexdigest() == first['image_sha256'])
                payload = mock.visions()[-1]['payload']
                check('provider request fixes DeepSeek flash and image blocks without tools or persona authority',
                      payload['model'] == 'deepseek-flash' and payload['stream'] is False
                      and payload['thinking'] == {'type': 'disabled'} and 'tools' not in payload
                      and payload['messages'][0]['content'].startswith('DSH_STANDALONE_IMAGE_ANALYSIS_V1'))
                with harness.db() as database:
                    reader = database.execute('SELECT max(version) FROM dsh_schema_migrations').fetchone()[0]
                check('first persisted image job raises reader requirement to schema six', reader == 6)
                repeat = create(harness, picture, question, client)
                create(harness, picture, question + ' changed', client, expected=409)
                create(harness, png(color=(1, 2, 3)), question, client, expected=409)
                create(harness, picture, '合成同owner active限制', expected=409)
                check('canonical pixels plus original question provide idempotency without duplicate billing',
                      repeat['id'] == first['id'] and mock.count() == calls + 1)

                original_ref = harness.applied['model']['credential_ref']
                original_authorization = hashlib.sha256(('Bearer ' + harness.key).encode()).hexdigest()
                with helpers.other_admin(harness):
                    harness.call('GET', job_path(first['id']), expected=404)
                    harness.call('DELETE', job_path(first['id']), expected=404, headers={'If-Match': '"1"'})
                    foreign_page, _, _ = harness.call('GET', ROOT)
                    second_question = '合成第二owner排队凭据冻结检查。'
                    second = create(harness, picture, second_question, client)
                    create(harness, picture, '合成第二owner active限制', expected=409)
                    with helpers.other_admin(harness):
                        create(harness, picture, '合成全局两任务限制', expected=409)
                    check('private job ACL owner-scoped request IDs and global two-job admission are enforced',
                          first['id'] not in {item['id'] for item in foreign_page['items']}
                          and second['id'] != first['id'] and mock.count() == calls + 1)
                    harness.key = secrets.token_urlsafe(32)
                    harness.configure(provider.server_port)
                    harness.call('DELETE', PREFIX + '/secrets/' + original_ref, expected=409)
                    mock.hold_release.set()
                    if not mock.hold_responded.wait(timeout=5):
                        raise AssertionError('metadata handler did not release')
                    second_done = wait_job(harness, second['id'])
                    second_request = next(item for item in mock.visions() if item['question'] == second_question)
                    check('queued and running images protect their frozen credential until actual dispatch finishes',
                          second_done['status'] == 'completed'
                          and second_request['authorization_sha256'] == original_authorization)
                first_done = wait_job(harness, first['id'])
                harness.call('DELETE', PREFIX + '/secrets/' + original_ref)
                check('completed job retains private result while dropping its image and releasing old credential',
                      first_done['status'] == 'completed' and first_done.get('result')
                      and private_row(harness, first['id'])['image'] is None)
                check('standalone analysis never creates chat Runs messages memories summaries or extraction jobs',
                      unrelated_counts(harness) == baseline)

                for name, valid in [('max_width', png(width=2048, height=1)),
                                    ('max_height', png(width=1, height=2048)),
                                    ('max_pixels', png(width=2048, height=1024)),
                                    ('max_raw_bytes', exact_size_png(1048576))]:
                    edge = create(harness, valid, '合成合法边界 ' + name)
                    edge_done = wait_job(harness, edge['id'])
                    check('PNG accepts exact ' + name + ' boundary and clears terminal image bytes',
                          edge_done['status'] == 'completed' and private_row(harness, edge['id'])['image'] is None)
                grey_client = str(uuid.uuid4())
                grey = create(harness, png(color=(128,), color_type=0), '合成相同灰度像素幂等', grey_client)
                wait_job(harness, grey['id'])
                grey_again = create(harness, png(color=(128, 128, 128, 255), color_type=6), '合成相同灰度像素幂等', grey_client)
                check('grayscale and opaque RGBA with identical pixels share one canonical request', grey_again['id'] == grey['id'])

                for mode in ['http_failure', 'malformed', 'missing_content', 'empty', 'too_large', 'null_byte', 'body_limit']:
                    failure_question = '合成模型失败 ' + mode
                    mock.arm(failure_question, mode)
                    before = mock.count()
                    failure = create(harness, picture, failure_question)
                    failure_done = wait_job(harness, failure['id'])
                    failure_row = private_row(harness, failure['id'])
                    check('single-attempt vision failure ' + mode + ' clears all input and output bodies',
                          failure_done['status'] == 'failed'
                          and re.fullmatch('[a-z][a-z0-9_]*', failure_done.get('error_code') or '') is not None
                          and all(failure_row[key] is None for key in ['image', 'question', 'result'])
                          and mock.count() == before + 1)

                cancel_question = '合成取消后不重发和native槽检查。'
                mock.arm(cancel_question, held=True)
                cancel_job = create(harness, picture, cancel_question)
                if not mock.hold_received.wait(timeout=10):
                    raise AssertionError('cancellable image request was not observed')
                chat = harness.conversation('synthetic ordinary chat during image analysis')
                chat_run = harness.submit(chat, '合成普通聊天仍可完成。')
                check('ordinary native chat completes while one vision request occupies the other slot',
                      harness.wait_run(chat_run)['status'] == 'completed')
                current, _, _ = harness.call('GET', job_path(cancel_job['id']))
                harness.call('DELETE', job_path(cancel_job['id']), expected=428)
                harness.call('DELETE', job_path(cancel_job['id']), expected=403, with_csrf=False)
                harness.call('DELETE', job_path(cancel_job['id']), expected=400, headers={'If-Match': '"0"'})
                harness.call('DELETE', job_path(cancel_job['id']), expected=409,
                             headers={'If-Match': '"' + str(current['revision'] + 100) + '"'})
                before = mock.count()
                cancelled, _, _ = harness.call('DELETE', job_path(cancel_job['id']),
                    headers={'If-Match': '"' + str(current['revision']) + '"'})
                mock.hold_release.set()
                if not mock.hold_responded.wait(timeout=5):
                    raise AssertionError('cancelled provider handler did not release')
                check('revision-checked cancellation clears all bodies and never replays a dispatched request',
                      cancelled['status'] == 'cancelled'
                      and all(private_row(harness, cancel_job['id'])[key] is None for key in ['image', 'question', 'result'])
                      and mock.count() == before)

                expiry, _, _ = harness.call('GET', job_path(first['id']))
                with harness.db() as database:
                    database.execute('UPDATE image_analysis_jobs SET expires_at=? WHERE id=?',
                                     (int(time.time()) - 1, first['id']))
                expired, _, _ = harness.call('GET', job_path(first['id']))
                check('result TTL expires all bodies and advances the review revision',
                      expired['status'] == 'expired' and expired['revision'] > expiry['revision']
                      and all(private_row(harness, first['id'])[key] is None for key in ['image', 'question', 'result']))
                target = grey_again
                current, _, _ = harness.call('GET', job_path(target['id']))
                deleted, _, _ = harness.call('DELETE', job_path(target['id']),
                    headers={'If-Match': '"' + str(current['revision']) + '"'})
                harness.call('DELETE', job_path(target['id']), expected=409,
                             headers={'If-Match': '"' + str(current['revision']) + '"'})
                harness.call('DELETE', job_path(target['id']),
                             headers={'If-Match': '"' + str(deleted['revision']) + '"'})
                tombstone = create(harness, png(color=(128,), color_type=0), '合成相同灰度像素幂等', grey_client)
                check('deletion clears all bodies while retaining the idempotency tombstone',
                      tombstone['status'] == 'deleted' and tombstone['id'] == grey['id']
                      and all(private_row(harness, grey['id'])[key] is None for key in ['image', 'question', 'result']))

                timeout_question = '合成三十秒超时单次尝试检查。'
                mock.arm(timeout_question, held=True)
                before = mock.count()
                timeout_job = create(harness, picture, timeout_question)
                if not mock.hold_received.wait(timeout=10):
                    raise AssertionError('timeout image request was not observed')
                timeout_done = wait_job(harness, timeout_job['id'])
                mock.hold_release.set()
                mock.hold_responded.wait(timeout=5)
                check('thirty-second provider timeout clears private bodies without automatic retry',
                      timeout_done['status'] == 'failed' and timeout_done.get('error_code') == 'model_timeout'
                      and mock.count() == before + 1
                      and all(private_row(harness, timeout_job['id'])[key] is None for key in ['image', 'question', 'result']))

                restart_question = '合成服务重启不重复发送图片。'
                mock.arm(restart_question, held=True)
                restart_job = create(harness, picture, restart_question)
                if not mock.hold_received.wait(timeout=10):
                    raise AssertionError('restart image request was not observed')
                before = mock.count()
                harness.stop()
                mock.hold_release.set()
                mock.hold_responded.wait(timeout=5)
                harness.start()
                harness.login()
                restarted = wait_job(harness, restart_job['id'])
                check('restart interrupts in-flight vision and clears private bodies without resending',
                      restarted['status'] == 'interrupted' and mock.count() == before
                      and all(private_row(harness, restart_job['id'])[key] is None for key in ['image', 'question', 'result']))
                check('vision stays single-flight inside the global two native model slots',
                      mock.maximum_vision <= 1 and mock.maximum <= 2)

                with harness.db() as database:
                    sample = dict(database.execute('SELECT * FROM image_analysis_jobs LIMIT 1').fetchone())
                    count = database.execute('SELECT count(*) FROM image_analysis_jobs').fetchone()[0]
                    for _ in range(1000 - count):
                        fixture = dict(sample, id=str(uuid.uuid4()), client_request_id=str(uuid.uuid4()),
                                       image=None, question=None, result=None, status='deleted', revision=1,
                                       request_started_at=None, request_finished_at=None,
                                       expires_at=int(time.time()) + 86400)
                        names = list(fixture)
                        database.execute('INSERT INTO image_analysis_jobs(' + ','.join(names) + ') VALUES(' +
                                         ','.join('?' for _ in names) + ')', tuple(fixture[name] for name in names))
                before = mock.count()
                capacity, _, _ = harness.call('POST', ROOT + '/jobs', body(picture, '合成元数据总容量上限'), expected=409)
                check('thousand-record metadata ceiling rejects new work without dropping idempotency history',
                      capacity.get('error', {}).get('code') == 'image_analysis_capacity' and mock.count() == before)
                harness.stop()
                future_dir = harness.root / 'future-image-schema'
                future_dir.mkdir()
                with closing(sqlite3.connect(harness.database)) as source:
                    with closing(sqlite3.connect(future_dir / 'dsh.db')) as destination:
                        source.backup(destination)
                        destination.execute('INSERT INTO image_analysis_schema(version,applied_at) VALUES(2,?)', (int(time.time()),))
                        destination.commit()
                with open(harness.root / 'future.log', 'wb') as log:
                    future_port = helpers.acceptance.free_port()
                    rejected = subprocess.run([str(binary), 'serve', '--data-dir', str(future_dir),
                        '--listen', '127.0.0.1:' + str(future_port),
                        '--origin', 'http://127.0.0.1:' + str(future_port)],
                        env=harness.environment, stdout=log, stderr=log, timeout=10)
                check('future image schema fails closed during daemon startup',
                      rejected.returncode != 0 and b'unsupported image analysis schema' in
                      (harness.root / 'future.log').read_bytes())
                report['status'] = 'passed'
            finally:
                harness.close()
    except Exception as error:
        failed = True
        report.update(status='failed', failure_after_check=stage, error_type=type(error).__name__)
        trace = error.__traceback__
        while trace:
            if trace.tb_frame.f_code.co_filename == __file__:
                report['failure_line'] = trace.tb_lineno
            trace = trace.tb_next
        if isinstance(error, helpers.ResponseStatusError):
            report.update(actual_http_status=error.actual, expected_http_statuses=error.expected)
        print('FAIL isolated image analysis; inspect sanitized metadata', flush=True)
    finally:
        mock.hold_release.set()
        provider.shutdown()
        provider.server_close()
        report.update(finished_at=helpers.acceptance.utc_now(), passed=len(report['checks']),
                      mock_vision_requests=mock.count(), observed_max_model_requests=mock.maximum,
                      observed_max_vision_requests=mock.maximum_vision)
        if args.report:
            destination = pathlib.Path(args.report).resolve()
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps({'status': report['status'], 'passed': report['passed'],
                          'real_external_vision_tested': False, 'real_user_images_accessed': False}))
    return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
