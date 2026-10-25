#!/usr/bin/env python3
"""One-shot synthetic QQ multipart delivery through the real Rust Outbox.

Requires a fresh explicit DSH接口验收： or DSH富媒体验收： C2C Run with only
native seq=1 sent. Only a pinned padded synthetic MP4 is accepted. Production
SQLite is read-only; a webhook-only temporary daemon owns all fixture mutations.
No model request, Gateway connection, HTTP retry or message retraction is made.
Internal upload HTTP statuses are not observable and are never invented.
"""
import argparse
import base64
import copy
import datetime
import hashlib
import importlib.util
import json
import os
import pathlib
import platform
import shutil
import sqlite3
import subprocess
import tempfile
import threading
import time
import types
import urllib.error
import urllib.request
import uuid
from contextlib import closing

PREFIX = '/api/admin/v1'
SOURCE_SHA256 = '517f27e7ec76c46b8ebb4a37fa1cfaab97342599ca380177ff698fcfe6345b04'
FIXTURE_SHA256 = '27c9497d696165a9cd044ac786929a3a2dab1fe87c44645a54b7f4bc0bd9fe82'
SOURCE_BYTES = 11508
FREE_BYTES = 6 * 1024 * 1024
DIRECT_LIMIT = 5 * 1024 * 1024
MEDIA_LIMIT = 20 * 1024 * 1024
OPERATIONS = ['post_v2_users_user_id_upload_prepare',
              'post_v2_users_user_id_upload_part_finish']
PREPARE_OUTCOMES = {'platform_accepted', 'platform_rejected', 'platform_permission_denied',
                    'asynchronous_pending', 'result_unknown', 'transport_failed'}


def load_module(name, filename):
    spec = importlib.util.spec_from_file_location(name, pathlib.Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def exclusive_json(path, value):
    """A durable, private, non-overwritable execution claim."""
    path = pathlib.Path(path)
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL |
                         getattr(os, 'O_NOFOLLOW', 0), 0o600)
    with os.fdopen(descriptor, 'w', encoding='utf-8') as output:
        json.dump(value, output, ensure_ascii=False, indent=2)
        output.write('\n')
        output.flush()
        os.fsync(output.fileno())
    sync_directory(path.parent)


def sync_directory(path):
    if os.name == 'posix':
        descriptor = os.open(path, os.O_RDONLY | getattr(os, 'O_DIRECTORY', 0))
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)


def write_private(path, value):
    temporary = path.with_name(path.name + '.' + uuid.uuid4().hex + '.tmp')
    exclusive_json(temporary, value)
    os.replace(temporary, path)
    sync_directory(path.parent)


def source_claim_path(source, source_run):
    # Independent of --report: changing its filename cannot replay this source.
    digest = hashlib.sha256(source_run.encode('ascii')).hexdigest()
    return source.parent / 'acceptance-journals' / ('qq-multipart-' + digest + '.json')


def checked_fixture(path):
    path = pathlib.Path(path).resolve(strict=True)
    size = path.stat().st_size
    if path.suffix.lower() != '.mp4' or not DIRECT_LIMIT < size < MEDIA_LIMIT:
        raise ValueError('multipart fixture must be an MP4 strictly between 5 and 20 MiB')
    data = path.read_bytes()
    if (len(data) != SOURCE_BYTES + FREE_BYTES or
            hashlib.sha256(data).hexdigest() != FIXTURE_SHA256 or
            hashlib.sha256(data[:SOURCE_BYTES]).hexdigest() != SOURCE_SHA256 or
            data[SOURCE_BYTES:SOURCE_BYTES + 8] != FREE_BYTES.to_bytes(4, 'big') + b'free' or
            any(data[SOURCE_BYTES + 8:])):
        raise ValueError('only the pinned synthetic clip plus trailing zero-filled free box is allowed')
    return data


def script_commit():
    repository = pathlib.Path(__file__).resolve().parents[1]
    files = ['tools/qq-multipart-live.py', 'tools/memory-smoke.py', 'tools/qq-media-live.py']
    subprocess.run(['git', '-C', str(repository), 'ls-files', '--error-unmatch', '--', *files],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
    subprocess.run(['git', '-C', str(repository), 'diff', '--quiet', 'HEAD', '--', *files],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
    return subprocess.run(['git', '-C', str(repository), 'rev-parse', 'HEAD'], check=True,
                          capture_output=True, text=True, timeout=10).stdout.strip()


def source_snapshot(source, run, helper, marker, *, require_fresh=False):
    qq, target = helper.source_context(source, run, required_marker=marker)
    with closing(sqlite3.connect((source / 'dsh.db').as_uri() + '?mode=ro', uri=True)) as db:
        db.execute('PRAGMA query_only=ON')
        row = db.execute('SELECT r.owner_id,r.conversation_id,r.client_message_id,r.rowid,'
                         'r.created_at,c.owner_id,coalesce(w.through_run_rowid,0),r.config_json '
                         'FROM runs r JOIN conversations c ON c.id=r.conversation_id '
                         'LEFT JOIN memory_context_watermarks w ON w.conversation_id=r.conversation_id '
                         'WHERE r.id=?', (run,)).fetchone()
        if (row is None or row[0] != row[5] or row[2] != target['message_id'] or row[3] <= row[6]):
            raise RuntimeError('source conversation ownership or history floor is invalid')
        if require_fresh and not 0 <= time.time() - int(row[4]) <= 180:
            raise RuntimeError('source must be a new GUI marker received within three minutes')
        count = db.execute('SELECT count(*) FROM outbox WHERE run_id=?', (run,)).fetchone()[0]
        part = db.execute('SELECT p.finished_ms,p.platform_message_id,p.error,o.error FROM outbox_reply_parts p '
                          'JOIN outbox o ON o.id=p.outbox_id WHERE o.run_id=? AND p.seq=1',
                          (run,)).fetchone()
        if (count != 1 or part is None or not part[1] or part[2] is not None or part[3] is not None or
                not isinstance(part[0], int) or part[0] <= 0 or part[0] > time.time() * 1000 + 60000):
            raise RuntimeError('source must have exactly one confirmed native delivery with a message ID')
        interval = json.loads(row[7]).get('reply', {}).get('min_interval_ms', 1500)
        if not isinstance(interval, int) or isinstance(interval, bool) or not 1000 <= interval <= 10000:
            raise RuntimeError('source frozen reply interval is invalid')
    scope = (row[0], row[1], row[2], row[3], row[6], interval)
    return qq, target, scope, part[0]


def queued_fixture(harness, conversation, asset, owner, target, config):
    """Mutate only the stopped daemon's private TemporaryDirectory database."""
    if harness.process and harness.process.poll() is None:
        raise RuntimeError('private daemon must be stopped before arming delivery')
    run, outbox = str(uuid.uuid4()), str(uuid.uuid4())
    caption = '这是合成分片上传验收视频。'
    timestamp = int(time.time())
    payload = {'type': 'video', 'text': caption, 'delay_ms': 0, 'asset_id': asset}
    with harness.db() as db:
        updated = db.execute('UPDATE conversations SET owner_id=?,source_key=? WHERE id=? AND owner_id=?',
                             (owner, owner, conversation, harness.owner)).rowcount
        if updated != 1:
            raise RuntimeError('private conversation ownership changed')
        updated = db.execute('UPDATE conversation_media_assets SET owner_id=? '
                             'WHERE id=? AND conversation_id=? AND owner_id=? AND kind=\'video\' '
                             'AND sha256=? AND length(bytes)=?',
                             (owner, asset, conversation, harness.owner, FIXTURE_SHA256,
                              SOURCE_BYTES + FREE_BYTES)).rowcount
        if updated != 1:
            raise RuntimeError('Rust-validated private video ownership changed')
        db.execute('INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,'
                   'reply_json,status,response,created_at,updated_at,run_kind) '
                   'VALUES(?,?,?,?,?,?,?,\'completed\',?,?,?,\'model\')',
                   (run, conversation, owner, run, 'synthetic multipart delivery fixture',
                    json.dumps(config), json.dumps(target), caption, timestamp, timestamp))
        db.execute('INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,'
                   'deadline,created_at) VALUES(?,?,?,?,?,?,\'queued\',?,?)',
                   (outbox, run, target['account_id'], target['credential_ref'], json.dumps(target),
                    caption, target['deadline'], timestamp))
        db.execute('INSERT INTO outbox_reply_parts(outbox_id,seq,payload_json,delay_ms,status,due_ms) '
                   'VALUES(?,2,?,0,\'queued\',?)', (outbox, json.dumps(payload), timestamp * 1000))
    return outbox


def observed_delivery(harness, outbox):
    with harness.db() as db:
        row = db.execute('SELECT o.status,o.error,o.platform_message_id,p.status,p.error,'
                         'p.platform_message_id,p.finished_ms,p.payload_json '
                         'FROM outbox o JOIN outbox_reply_parts p ON p.outbox_id=o.id '
                         'WHERE o.id=? AND p.seq=2', (outbox,)).fetchone()
        calls = db.execute('SELECT count(*) FROM qq_api_calls WHERE operation_id IN (?,?)',
                           OPERATIONS).fetchone()[0]
    if row is None:
        raise RuntimeError('private delivery record disappeared')
    return {'outbox_status': row[0], 'part_status': row[3],
            'outbox_has_error': row[1] is not None, 'part_has_error': row[4] is not None,
            'outbox_has_message_id': bool(row[2]), 'part_has_message_id': bool(row[5]),
            'finished_ms': row[6], 'payload_type': json.loads(row[7]).get('type'),
            'direct_admin_call_records': calls}


def successful_multipart(observation):
    return (observation['outbox_status'] == observation['part_status'] == 'sent' and
            not observation['outbox_has_error'] and not observation['part_has_error'] and
            observation['outbox_has_message_id'] and observation['part_has_message_id'] and
            observation['payload_type'] == 'video')


def prepare_body(video):
    return {'file_type': 2, 'file_size': str(len(video)), 'file_name': 'dsh-video.mp4',
            'md5': hashlib.md5(video).hexdigest(), 'sha1': hashlib.sha1(video).hexdigest(),
            'md5_10m': hashlib.md5(video[:min(len(video), 10_002_432)]).hexdigest()}


def bounded_integer(value, maximum=MEDIA_LIMIT):
    if isinstance(value, bool):
        return None
    if isinstance(value, str) and 1 <= len(value) <= 12 and value.isascii() and value.isdigit():
        value = int(value)
    return value if isinstance(value, int) and 0 <= value <= maximum else None


def response_shape(response):
    """Only fixed field names, booleans and bounded numbers may escape."""
    def index_type(part, field='index'):
        if not isinstance(part, dict):
            return 'other'
        if field not in part:
            return 'missing'
        value = part[field]
        if isinstance(value, bool):
            return 'bool'
        if isinstance(value, int):
            return 'integer'
        return 'string' if isinstance(value, str) else 'other'

    def fields(value):
        if not isinstance(value, dict):
            return None
        upload = value.get('upload_id')
        protected = (upload == '[server-held reference]' or
                     isinstance(upload, dict) and '$qq_ref' in upload)
        parts = value.get('parts')
        parts_array = isinstance(parts, list)
        bounded = parts_array and len(parts) <= 128
        return {'upload_id_present': 'upload_id' in value,
                'upload_id_protected_reference': protected,
                'upload_id_raw_nonempty_string': isinstance(upload, str) and bool(upload) and not protected,
                'block_size_present': 'block_size' in value,
                'block_size': bounded_integer(value.get('block_size')),
                'parts_is_array': parts_array,
                'parts_count': len(parts) if bounded else None,
                'parts_within_128_bound': bounded,
                'part_indices': [bounded_integer(part.get('index'), 128) if isinstance(part, dict)
                                 else None for part in parts] if bounded else None,
                'part_indices_present': [isinstance(part, dict) and 'index' in part
                                         for part in parts] if bounded else None,
                'part_index_types': [index_type(part) for part in parts] if bounded else None,
                'part_index_fields_present': [isinstance(part, dict) and 'part_index' in part
                                              for part in parts] if bounded else None,
                'part_index_values': [bounded_integer(part.get('part_index'), 128) if isinstance(part, dict)
                                      else None for part in parts] if bounded else None,
                'part_index_value_types': [index_type(part, 'part_index') for part in parts]
                                           if bounded else None,
                'part_block_sizes': [bounded_integer(part.get('block_size')) if isinstance(part, dict)
                                     else None for part in parts] if bounded else None,
                'part_indices_ordered': bounded and all(isinstance(part, dict) and
                    type(part.get('index')) is int and part['index'] == index for index, part in enumerate(parts)),
                'all_parts_have_presigned_url_field': bounded and bool(parts) and
                    all(isinstance(part, dict) and 'presigned_url' in part for part in parts),
                'all_part_urls_are_protected_references': bounded and bool(parts) and
                    all(isinstance(part, dict) and (part.get('presigned_url') == '[server-held reference]' or
                        isinstance(part.get('presigned_url'), dict) and '$qq_ref' in part['presigned_url'])
                        for part in parts)}
    root_object = isinstance(response, dict)
    return {'root_is_object': root_object, 'data_wrapper_present': root_object and 'data' in response,
            'data_wrapper_is_object': root_object and isinstance(response.get('data'), dict),
            'root_fields': fields(response),
            'data_fields': fields(response.get('data')) if root_object else None}


def safe_prepare_result(result):
    outcome = result.get('outcome')
    trace = result.get('trace_id')
    # Tencent trace IDs are opaque diagnostics, never arbitrary response text.
    trace_safe = (isinstance(trace, str) and 16 <= len(trace) <= 128 and
                  all(c in '0123456789abcdefABCDEF-' for c in trace))
    return {'operation': OPERATIONS[0],
            'outcome': outcome if isinstance(outcome, str) and outcome in PREPARE_OUTCOMES else 'unrecognized_result',
            'platform_accepted': result.get('platform_accepted') is True and outcome == 'platform_accepted'
                and type(result.get('http_status')) is int and result['http_status'] in (200, 204)
                and (result.get('business_code') is None or
                     type(result.get('business_code')) is int and result['business_code'] == 0),
            'http_status': bounded_integer(result.get('http_status'), 599)
                if type(result.get('http_status')) is int and result['http_status'] >= 100 else None,
            'business_code': result.get('business_code') if type(result.get('business_code')) is int
                and -(2 ** 63) <= result['business_code'] < 2 ** 63 else None,
            'trace_id': trace if trace_safe else None,
            'trace_id_present': trace is not None,
            'trace_id_suppressed': trace is not None and not trace_safe,
            'response_shape': response_shape(result.get('response'))}


class LocalPrepareFailure(RuntimeError):
    def __init__(self, status):
        self.http_status = status
        super().__init__('isolated prepare API did not return its expected envelope')


def admin_prepare_request(harness, target, body):
    payload = {'parameters': {'user_id': target['target']}, 'body': body, 'acknowledge_effects': True}
    request = urllib.request.Request(harness.base + PREFIX + '/qq/api/operations/' + OPERATIONS[0],
        json.dumps(payload).encode(), {'Origin': harness.base, 'Content-Type': 'application/json',
                                       'X-CSRF-Token': harness.csrf}, method='POST')
    try:
        response = harness.client.open(request, timeout=60)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        raw, status = response.read(1024 * 1024 + 1), response.code
    if status != 200 or len(raw) > 1024 * 1024:
        raise LocalPrepareFailure(status)
    result = json.loads(raw)
    if not isinstance(result, dict):
        raise LocalPrepareFailure(status)
    return result


def prepare_probe(harness, target, video, invoke=admin_prepare_request):
    """Exactly one prepare request; no PUT, finish, merge, send or queue."""
    try:
        result = invoke(harness, target, prepare_body(video))
    except Exception as error:
        record = {'operation': OPERATIONS[0], 'outcome': 'result_unknown',
                  'platform_accepted': False, 'http_status': None, 'business_code': None,
                  'trace_id': None, 'call_record_matches': False, 'exception_type': type(error).__name__}
        if isinstance(error, LocalPrepareFailure):
            record['local_http_status'] = error.http_status
            if 400 <= error.http_status < 500:
                record['outcome'] = 'local_request_rejected'
                return record, 'prepare_local_request_rejected_no_message_sent', True
        return record, 'prepare_unknown_or_unconfirmed_no_replay', True
    record = safe_prepare_result(result)
    try:
        with harness.db() as db:
            row = db.execute('SELECT outcome,http_status,business_code,trace_id FROM qq_api_calls '
                             'WHERE id=? AND owner_id=? AND account_id=? AND operation_id=?',
                             (result.get('call_id'), harness.owner, target['account_id'], OPERATIONS[0])).fetchone()
            queue_count = db.execute('SELECT count(*) FROM outbox').fetchone()[0]
    except Exception as error:
        record['call_record_matches'] = False
        record['observation_exception_type'] = type(error).__name__
        return record, 'prepare_evidence_unconfirmed_no_replay', True
    expected = tuple(result.get(key) for key in ['outcome', 'http_status', 'business_code', 'trace_id'])
    record['call_record_matches'] = row is not None and tuple(row) == expected
    record['private_outbox_count'] = queue_count
    if not record['call_record_matches'] or queue_count != 0:
        return record, 'prepare_evidence_unconfirmed_no_replay', True
    if record['platform_accepted']:
        return record, 'prepare_platform_accepted_no_message_sent', False
    if record['outcome'] in {'platform_rejected', 'platform_permission_denied'}:
        return record, 'prepare_platform_rejected_no_message_sent', True
    return record, 'prepare_unknown_or_unconfirmed_no_replay', True


def cleanup_private(harness, temporary, *, credential_copied=False, failed=False, diagnostic=None):
    """Never delete a private runtime until its exact child is confirmed dead."""
    result = {'daemon_exit_confirmed': harness is None, 'daemon_exit_code': None,
              'private_cleanup_complete': temporary is None, 'credentials_copy_retained': False,
              'diagnostic_log_retained': False}
    if harness:
        try:
            harness.close()
        except Exception as error:
            result['daemon_close_exception_type'] = type(error).__name__
        try:
            process = harness.process
            status = process.poll() if process is not None else None
            result['daemon_exit_confirmed'] = process is None or status is not None
            result['daemon_exit_code'] = status
        except Exception as error:
            result['daemon_poll_exception_type'] = type(error).__name__
    if temporary is not None:
        root = pathlib.Path(temporary)
        if diagnostic is not None and (failed or not result['daemon_exit_confirmed'] or
                                        'daemon_close_exception_type' in result):
            try:
                log = root / 'daemon.log'
                if log.is_file():
                    with log.open('rb') as source:
                        source.seek(0, 2)
                        source.seek(max(0, source.tell() - 1024 * 1024))
                        data = source.read(1024 * 1024)
                    descriptor = os.open(diagnostic, os.O_WRONLY | os.O_CREAT | os.O_EXCL |
                                         getattr(os, 'O_NOFOLLOW', 0), 0o600)
                    with os.fdopen(descriptor, 'wb') as output:
                        output.write(data)
                        output.flush()
                        os.fsync(output.fileno())
                    result['diagnostic_log_retained'] = True
            except Exception as error:
                result['diagnostic_exception_type'] = type(error).__name__
        if result['daemon_exit_confirmed']:
            try:
                base = pathlib.Path(tempfile.gettempdir()).resolve(strict=True)
                if root.is_symlink() or root.resolve(strict=True) != root or root.parent != base or not root.name.startswith('dsh-qq-multipart-live-'):
                    raise RuntimeError('private cleanup path guard failed')
                shutil.rmtree(root)
                result['private_cleanup_complete'] = True
            except Exception as error:
                result['private_cleanup_exception_type'] = type(error).__name__
        result['credentials_copy_retained'] = bool(credential_copied and not result['private_cleanup_complete'])
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ['binary', 'web-dir', 'source-data-dir', 'source-run-id',
                 'expected-commit', 'video-fixture', 'report']:
        parser.add_argument('--' + flag, required=True)
    parser.add_argument('--marker', choices=['interface', 'media'], default='interface')
    parser.add_argument('--prepare-only', action='store_true',
                        help='call only upload_prepare once; never queue or send a message')
    args = parser.parse_args()
    source = pathlib.Path(args.source_data_dir).resolve(strict=True)
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    source_run = str(uuid.UUID(args.source_run_id))
    if len(args.expected_commit) != 40 or any(c not in '0123456789abcdef' for c in args.expected_commit):
        parser.error('--expected-commit must be the pushed full lowercase Git SHA')
    destination = pathlib.Path(args.report).resolve()
    claim = source_claim_path(source, source_run)
    if destination.exists() or claim.exists():
        print('BLOCKED prior report or fixed-source execution journal exists; no replay', flush=True)
        return 2
    report = {'kind': 'explicit QQ C2C real Rust multipart delivery acceptance',
              'started_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
              'mode': 'prepare_only' if args.prepare_only else 'full_multipart_delivery',
              'commit': None, 'expected_commit': args.expected_commit,
              'script_commit': None, 'script_sha256': hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest(),
              'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'source_run_sha256': hashlib.sha256(source_run.encode()).hexdigest(),
              'platform': platform.system(), 'architecture': platform.machine(),
              'production_database_modified': False, 'production_message_history_copied': False,
              'production_credential_read': False, 'second_gateway_started': False,
              'model_requests_executed': False, 'automatic_retries': False,
              'client_verified': False, 'real_qq_send_intent_recorded': False,
              'prepare_intent_recorded': False,
              'checks': [], 'operations': [], 'status': 'running'}
    journal = {'kind': 'one-shot QQ multipart source claim', 'source_run_sha256':
               report['source_run_sha256'], 'seq': None if args.prepare_only else 2,
               'mode': report['mode'], 'phase': 'claimed', 'automatic_retry': False}
    try:
        exclusive_json(destination, report)
    except FileExistsError:
        print('BLOCKED report was claimed concurrently', flush=True)
        return 2
    helpers = load_module('qq_multipart_daemon_helpers', 'memory-smoke.py')
    media = load_module('qq_multipart_source_helpers', 'qq-media-live.py')
    marker = media.INTERFACE_MARKER if args.marker == 'interface' else media.MARKER
    harness, temporary, key = None, None, None
    claim_owned, failed, credential_copied = False, False, False
    stage = 'verify offline fixture and explicit source'

    def save():
        errors = []
        try:
            write_private(destination, report)
        except Exception as error:
            errors.append(error)
        if claim_owned:
            try:
                write_private(claim, journal)
            except Exception as error:
                errors.append(error)
        if errors:
            raise RuntimeError('private report or journal persistence failed') from None

    def check(name, condition=True):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        if name not in report['checks']:
            report['checks'].append(name)
            print('PASS ' + name, flush=True)

    def call(method, path, body=None, expected=200, headers=None):
        return harness.request(method, path, body, expected=expected, headers=headers)[0]

    def unchanged():
        current_qq, current_target, current_scope, _ = source_snapshot(source, source_run, media, marker)
        check('frozen explicit source account credential target and floor remain unchanged',
              current_qq == qq and current_target == target and current_scope == scope)

    try:
        if platform.system() != 'Linux':
            raise RuntimeError('live execution is restricted to the verified Linux deployment host')
        report['script_commit'] = script_commit()
        video = checked_fixture(args.video_fixture)
        check('pinned synthetic padded MP4 exceeds direct-upload bound and stays below 20MiB')
        report['fixture'] = {'bytes': len(video), 'sha256': FIXTURE_SHA256,
                             'duration_ms': 2000, 'padding_free_box_bytes': FREE_BYTES,
                             'padding_is_not_video_bitrate': True}
        qq, target, scope, native_finished_ms = source_snapshot(source, source_run, media, marker,
                                                               require_fresh=True)
        check('new fixed-marker C2C source has exactly native seq1 sent and at least 120 seconds')
        journal.update(account_sha256=hashlib.sha256(qq['app_id'].encode()).hexdigest(),
                       target_sha256=hashlib.sha256(target['target'].encode()).hexdigest())
        exclusive_json(claim, journal)
        claim_owned = True
        store = (source / 'secrets').resolve(strict=True)
        secret = (store / str(uuid.UUID(qq['credential_ref']))).resolve(strict=True)
        if not secret.is_relative_to(store):
            raise RuntimeError('QQ credential escaped its private store')
        key = secret.read_text(encoding='utf-8')
        if not key.strip():
            raise RuntimeError('QQ credential is empty')
        report['production_credential_read'] = True
        temporary = pathlib.Path(tempfile.mkdtemp(prefix='dsh-qq-multipart-live-')).resolve(strict=True)
        harness = helpers.Harness(str(binary), str(web), str(temporary),
                                  types.SimpleNamespace(hold_release=threading.Event()))
        harness.start()
        report['commit'] = report['deployment_sha'] = harness.commit
        check('private daemon binary reports the required pushed commit', harness.commit == args.expected_commit)
        harness.bootstrap()
        current = call('GET', PREFIX + '/config')
        credential = call('POST', PREFIX + '/secrets', {'kind': 'qq_app_secret', 'value': key})
        credential_copied = True
        key = None
        candidate = copy.deepcopy(current['config'])
        candidate['qq'] = dict(qq, enabled=True, transport='webhook', credential_ref=credential['id'])
        candidate['model']['credential_ref'] = None
        candidate['speech']['enabled'] = False
        candidate['reply']['min_interval_ms'] = scope[5]
        candidate['reply']['max_interval_ms'] = max(candidate['reply']['max_interval_ms'], scope[5])
        draft = call('POST', PREFIX + '/config/drafts',
                     {'base_revision': current['revision'], 'config': candidate})
        route = PREFIX + '/config/drafts/' + draft['id']
        check('private QQ configuration validates without Gateway or model credentials',
              call('POST', route + '/validate', {})['valid'])
        call('POST', route + '/apply', {}, expected=202,
             headers={'If-Match': '"' + str(current['revision']) + '"'})
        applied = call('GET', PREFIX + '/config')
        check('private webhook configuration is applied', applied['revision'] == applied['applied_revision'])
        conversation = harness.conversation('synthetic multipart video acceptance')
        asset = call('POST', PREFIX + '/conversations/' + conversation + '/media',
                     {'kind': 'video', 'data_base64': base64.b64encode(video).decode('ascii')}, expected=201)
        check('actual Rust MP4 validator accepts the pinned padded video',
              asset['kind'] == 'video' and asset['sha256'] == FIXTURE_SHA256 and asset['bytes'] == len(video))
        report['fixture']['Rust_validator_accepted'] = True
        if args.prepare_only:
            unchanged()
            stage = 'single prepare-only diagnostic with no Outbox or message'
            journal['phase'] = 'prepare_only_upload_intent_recorded'
            report['prepare_intent_recorded'] = True
            save()
            record, report['status'], failed = prepare_probe(harness, target, video)
            report['operations'].append(record)
            report['evidence_boundary'] = ('One authenticated Admin upload_prepare call only. '
                'HTTP metadata is cross-checked against private qq_api_calls. Response shape contains '
                'only booleans and bounded numbers; protected references are not raw upload IDs. '
                'No presigned PUT, upload_part_finish, files merge, Outbox or message send was attempted.')
            journal['phase'] = report['status']
            save()
        else:
            harness.stop()
            unchanged()
            check('source native send interval has elapsed',
                  int(time.time() * 1000) >= native_finished_ms + scope[5])
            private_target = dict(target, credential_ref=credential['id'])
            journal['phase'] = 'video_seq2_send_intent_recorded'
            report['real_qq_send_intent_recorded'] = True
            save()
            outbox = queued_fixture(harness, conversation, asset['id'], scope[0], private_target, applied['config'])
            unchanged()
            check('binary is unchanged before private queue execution',
                  hashlib.sha256(binary.read_bytes()).hexdigest() == report['binary_sha256'])
            # This restart is the only point at which the armed fixture can send.
            # The durable source claim and send intent already exist on disk.
            harness.start()
            stage = 'wait for exactly one real Rust multipart video delivery'
            until = time.monotonic() + min(180, max(1, target['deadline'] - int(time.time()) - 5))
            next_guard = time.monotonic()
            observation = None
            while time.monotonic() < until:
                observation = observed_delivery(harness, outbox)
                if observation['outbox_status'] in {'sent', 'failed', 'unknown', 'expired', 'cancelled'}:
                    break
                if time.monotonic() >= next_guard:
                    unchanged()
                    next_guard = time.monotonic() + 2
                time.sleep(0.2)
            report['delivery'] = observation
            success = observation is not None and successful_multipart(observation)
            for operation in OPERATIONS:
                report['operations'].append({'operation': operation,
                    'outcome': 'required_pipeline_passed' if success else 'not_independently_verified',
                    'evidence': ('over-5MiB Rust branch plus confirmed video delivery without text fallback'
                        if success else 'Rust branch attempted; upload phase and video delivery were not verified'),
                    'http_status': None, 'business_code': None, 'raw_HTTP_status_observed': False})
            report['evidence_boundary'] = ('qq_media does not persist its internal platform requests in '
                'qq_api_calls. A no-fallback sent video implies prepare, every PUT/finish and files merge '
                'were accepted along the fixed over-5MiB branch; exact statuses, trace IDs and part count '
                'were not observed. This does not replace direct per-operation HTTP evidence or QQ client playback.')
            if success:
                check('real Rust multipart video completed with message ID and no text fallback')
                report['status'] = 'required_pipeline_passed_client_verification_pending'
                journal['phase'] = 'completed_without_fallback'
            else:
                failed = True
                report['status'] = 'failed_or_unknown_no_replay'
                journal['phase'] = 'terminal_or_unknown_no_replay'
            save()
    except Exception as error:
        failed = True
        report['status'] = 'failed_or_unknown_no_replay'
        report['failure'] = {'stage': stage, 'exception_type': type(error).__name__}
        if claim_owned:
            journal['phase'] = 'failed_or_unknown_no_replay'
        print('FAIL ' + stage + ' (' + type(error).__name__ + ')', flush=True)
    finally:
        key = None
        try:
            report['cleanup'] = cleanup_private(harness, temporary, credential_copied=credential_copied,
                failed=failed, diagnostic=destination.with_name(destination.name + '.daemon.log'))
            if not report['cleanup']['daemon_exit_confirmed'] or not report['cleanup']['private_cleanup_complete']:
                failed = True
                report['status'] = 'private_cleanup_unconfirmed_no_replay'
                if claim_owned:
                    journal['phase'] = 'private_cleanup_unconfirmed_no_replay'
        except Exception as error:
            failed = True
            report['status'] = 'private_cleanup_unconfirmed_no_replay'
            report['cleanup'] = {'daemon_exit_confirmed': False, 'private_cleanup_complete': False,
                                 'credentials_copy_retained': credential_copied,
                                 'exception_type': type(error).__name__}
            if claim_owned:
                journal['phase'] = 'private_cleanup_unconfirmed_no_replay'
        finally:
            report['finished_at'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
            save()
    return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
