import contextlib
import copy
import importlib.util
import io
import json
import pathlib
import sqlite3
import sys
import tempfile
import time
import types
import unittest
from unittest import mock

PATH = pathlib.Path(__file__).resolve().parents[1] / 'qq-multipart-live.py'
SPEC = importlib.util.spec_from_file_location('qq_multipart_live_tests', PATH)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class MultipartFixtureTests(unittest.TestCase):
    def test_only_pinned_padding_fixture_is_accepted(self):
        source = pathlib.Path(__file__).resolve().parents[2] / 'crates/dshd/testdata/synthetic-h264.mp4'
        data = source.read_bytes() + MODULE.FREE_BYTES.to_bytes(4, 'big') + b'free' + bytes(MODULE.FREE_BYTES - 8)
        with tempfile.TemporaryDirectory() as root:
            path = pathlib.Path(root) / 'synthetic.mp4'
            path.write_bytes(data)
            self.assertEqual(MODULE.checked_fixture(path), data)
            path.write_bytes(data[:-1] + b'X')
            with self.assertRaises(ValueError):
                MODULE.checked_fixture(path)
            path.write_bytes(source.read_bytes())
            with self.assertRaises(ValueError):
                MODULE.checked_fixture(path)

    def test_source_claim_cannot_be_replayed_under_another_report_name(self):
        with tempfile.TemporaryDirectory() as root:
            source = pathlib.Path(root) / 'production-data'
            source.mkdir()
            first = MODULE.source_claim_path(source, 'same-frozen-source')
            second = MODULE.source_claim_path(source, 'same-frozen-source')
            self.assertEqual(first, second)
            self.assertNotEqual(first, MODULE.source_claim_path(source, 'new-frozen-source'))
            MODULE.exclusive_json(first, {'phase': 'send_intent_recorded'})
            with self.assertRaises(FileExistsError):
                MODULE.exclusive_json(second, {'phase': 'replay'})
            self.assertEqual(json.loads(first.read_text()), {'phase': 'send_intent_recorded'})

    def test_confirmed_text_fallback_is_never_multipart_success(self):
        observation = {'outbox_status': 'sent', 'part_status': 'sent',
                       'outbox_has_error': False, 'part_has_error': False,
                       'outbox_has_message_id': True, 'part_has_message_id': True,
                       'payload_type': 'video'}
        self.assertTrue(MODULE.successful_multipart(observation))
        for key, value in [('outbox_status', 'unknown'), ('part_status', 'sending'),
                           ('outbox_has_error', True), ('part_has_error', True),
                           ('outbox_has_message_id', False), ('part_has_message_id', False),
                           ('payload_type', 'text')]:
            changed = dict(observation, **{key: value})
            self.assertFalse(MODULE.successful_multipart(changed), key)


class SourceSnapshotTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temporary.name)
        self.db = sqlite3.connect(self.root / 'dsh.db')
        self.db.executescript('CREATE TABLE conversations(id TEXT,owner_id TEXT);'
            'CREATE TABLE runs(id TEXT,owner_id TEXT,conversation_id TEXT,client_message_id TEXT,created_at INTEGER,config_json TEXT);'
            'CREATE TABLE memory_context_watermarks(conversation_id TEXT,through_run_rowid INTEGER);'
            'CREATE TABLE outbox(id TEXT,run_id TEXT,error TEXT);'
            'CREATE TABLE outbox_reply_parts(outbox_id TEXT,seq INTEGER,finished_ms INTEGER,platform_message_id TEXT,error TEXT);')
        self.db.execute("INSERT INTO conversations VALUES('conversation','owner')")
        self.db.execute('INSERT INTO runs VALUES(?,?,?,?,?,?)',
                        ('run', 'owner', 'conversation', 'fresh-input-id', int(time.time()),
                         json.dumps({'reply': {'min_interval_ms': 3500}})))
        self.db.execute("INSERT INTO outbox VALUES('native','run',NULL)")
        self.db.execute('INSERT INTO outbox_reply_parts VALUES(?,?,?,?,NULL)',
                        ('native', 1, int(time.time() * 1000), 'our-native-message'))
        self.db.commit()
        self.helper = types.SimpleNamespace(source_context=lambda *args, **kwargs:
            ({'app_id': 'synthetic'}, {'message_id': 'fresh-input-id'}))

    def tearDown(self):
        self.db.close()
        self.temporary.cleanup()

    def snapshot(self, fresh=True):
        return MODULE.source_snapshot(self.root, 'run', self.helper, 'fixed-marker', require_fresh=fresh)

    def test_current_owned_source_is_accepted(self):
        qq, target, scope, finished = self.snapshot()
        self.assertEqual(scope, ('owner', 'conversation', 'fresh-input-id', 1, 0, 3500))
        self.assertGreater(finished, 0)
        self.assertEqual(qq['app_id'], 'synthetic')

    def test_changed_conversation_owner_or_source_message_is_rejected(self):
        for statement in ["UPDATE conversations SET owner_id='peer'",
                          "UPDATE runs SET client_message_id='another-input-id'",
                          "INSERT INTO memory_context_watermarks VALUES('conversation',1)"]:
            self.db.execute(statement)
            self.db.commit()
            with self.assertRaises(RuntimeError):
                self.snapshot()
            # Restore only the synthetic fixture, never a production database.
            self.db.execute("UPDATE conversations SET owner_id='owner'")
            self.db.execute("UPDATE runs SET client_message_id='fresh-input-id'")
            self.db.execute('DELETE FROM memory_context_watermarks')
            self.db.commit()

    def test_old_marker_is_not_a_new_invocation_source(self):
        self.db.execute('UPDATE runs SET created_at=?', (int(time.time()) - 300,))
        self.db.commit()
        with self.assertRaises(RuntimeError):
            self.snapshot()
        self.snapshot(fresh=False)

    def test_extra_outbox_or_missing_native_message_id_blocks_execution(self):
        self.db.execute("INSERT INTO outbox VALUES('extra','run',NULL)")
        self.db.commit()
        with self.assertRaises(RuntimeError):
            self.snapshot()
        self.db.execute("DELETE FROM outbox WHERE id='extra'")
        self.db.execute('UPDATE outbox_reply_parts SET platform_message_id=NULL')
        self.db.commit()
        with self.assertRaises(RuntimeError):
            self.snapshot()

    def test_missing_native_completion_or_fallback_is_not_a_pacing_sample(self):
        for mutation in ['UPDATE outbox_reply_parts SET finished_ms=NULL',
                         'UPDATE outbox_reply_parts SET finished_ms=0',
                         "UPDATE outbox_reply_parts SET error='media fallback'",
                         "UPDATE outbox SET error='uncertain delivery'"]:
            self.db.execute(mutation)
            self.db.commit()
            with self.assertRaises(RuntimeError):
                self.snapshot()
            self.db.execute('UPDATE outbox_reply_parts SET finished_ms=?,error=NULL',
                            (int(time.time() * 1000),))
            self.db.execute('UPDATE outbox SET error=NULL')
            self.db.commit()


class QueueFixtureTests(unittest.TestCase):
    def test_only_stopped_private_daemon_can_arm_one_video_at_seq2(self):
        with tempfile.TemporaryDirectory() as root:
            path = pathlib.Path(root) / 'private.db'
            with contextlib.closing(sqlite3.connect(path)) as db, db:
                db.executescript('CREATE TABLE conversations(id TEXT,owner_id TEXT,source_key TEXT);'
                    'CREATE TABLE conversation_media_assets(id TEXT,conversation_id TEXT,owner_id TEXT,kind TEXT,sha256 TEXT,bytes BLOB);'
                    'CREATE TABLE runs(id TEXT,conversation_id TEXT,owner_id TEXT,client_message_id TEXT,input TEXT,config_json TEXT,reply_json TEXT,status TEXT,response TEXT,created_at INTEGER,updated_at INTEGER,run_kind TEXT);'
                    'CREATE TABLE outbox(id TEXT,run_id TEXT,account_id TEXT,credential_ref TEXT,target_json TEXT,content TEXT,status TEXT,deadline INTEGER,created_at INTEGER);'
                    'CREATE TABLE outbox_reply_parts(outbox_id TEXT,seq INTEGER,payload_json TEXT,delay_ms INTEGER,status TEXT,due_ms INTEGER);')
                db.execute("INSERT INTO conversations VALUES('c','private-admin',NULL)")
                db.execute('INSERT INTO conversation_media_assets VALUES(?,?,?,?,?,zeroblob(?))',
                           ('asset', 'c', 'private-admin', 'video', MODULE.FIXTURE_SHA256,
                            MODULE.SOURCE_BYTES + MODULE.FREE_BYTES))
            @contextlib.contextmanager
            def connect():
                with contextlib.closing(sqlite3.connect(path)) as db, db:
                    yield db
            harness = types.SimpleNamespace(owner='private-admin', db=connect,
                process=types.SimpleNamespace(poll=lambda: None))
            target = {'account_id': 'synthetic-account', 'credential_ref': 'private-ref',
                      'message_id': 'fresh-input', 'target': 'own-test-recipient',
                      'kind': 'c2c', 'deadline': int(time.time()) + 600}
            frozen = copy.deepcopy(target)
            with self.assertRaises(RuntimeError):
                MODULE.queued_fixture(harness, 'c', 'asset', 'qq-owner', target, {})
            harness.process = types.SimpleNamespace(poll=lambda: 0)
            outbox = MODULE.queued_fixture(harness, 'c', 'asset', 'qq-owner', target, {})
            self.assertEqual(target, frozen)
            with contextlib.closing(sqlite3.connect(path)) as db:
                self.assertEqual(db.execute('SELECT status FROM runs').fetchall(), [('completed',)])
                row = db.execute('SELECT seq,payload_json FROM outbox_reply_parts WHERE outbox_id=?', (outbox,)).fetchone()
                self.assertEqual(row[0], 2)
                self.assertEqual(json.loads(row[1])['type'], 'video')
                self.assertEqual(db.execute('SELECT count(*) FROM outbox').fetchone()[0], 1)
                self.assertEqual(db.execute('SELECT owner_id FROM conversation_media_assets').fetchone()[0], 'qq-owner')


class CleanupTests(unittest.TestCase):
    def test_live_exact_child_keeps_private_copy_and_restricted_diagnostics(self):
        root = pathlib.Path(tempfile.mkdtemp(prefix='dsh-qq-multipart-live-')).resolve()
        (root / 'daemon.log').write_text('synthetic diagnostic', encoding='utf-8')
        def broken_close():
            raise TimeoutError('synthetic close failure')
        harness = types.SimpleNamespace(close=broken_close, process=types.SimpleNamespace(poll=lambda: None))
        with tempfile.TemporaryDirectory() as reports:
            diagnostic = pathlib.Path(reports) / 'private.daemon.log'
            result = MODULE.cleanup_private(harness, root, credential_copied=True, diagnostic=diagnostic)
            self.assertFalse(result['daemon_exit_confirmed'])
            self.assertFalse(result['private_cleanup_complete'])
            self.assertTrue(result['credentials_copy_retained'])
            self.assertTrue(result['diagnostic_log_retained'])
            self.assertTrue(root.exists())
            # The fake child now exits; only then may the helper delete its root.
            harness.process = types.SimpleNamespace(poll=lambda: -15)
            result = MODULE.cleanup_private(harness, root, credential_copied=True)
            self.assertTrue(result['daemon_exit_confirmed'])
            self.assertEqual(result['daemon_exit_code'], -15)
            self.assertTrue(result['private_cleanup_complete'])
            self.assertFalse(result['credentials_copy_retained'])
            self.assertFalse(root.exists())

    def test_dead_child_cleanup_records_exact_exit_and_keeps_failed_log(self):
        root = pathlib.Path(tempfile.mkdtemp(prefix='dsh-qq-multipart-live-')).resolve()
        (root / 'daemon.log').write_text('synthetic failure log', encoding='utf-8')
        harness = types.SimpleNamespace(close=lambda: None, process=types.SimpleNamespace(poll=lambda: 9))
        with tempfile.TemporaryDirectory() as reports:
            diagnostic = pathlib.Path(reports) / 'private.daemon.log'
            result = MODULE.cleanup_private(harness, root, failed=True, diagnostic=diagnostic)
            self.assertTrue(result['daemon_exit_confirmed'])
            self.assertEqual(result['daemon_exit_code'], 9)
            self.assertTrue(result['private_cleanup_complete'])
            self.assertTrue(result['diagnostic_log_retained'])
            self.assertEqual(diagnostic.read_text(), 'synthetic failure log')


class PrepareOnlyTests(unittest.TestCase):
    def test_prepare_mode_never_arms_outbox_and_same_source_cannot_switch_to_full_mode(self):
        with tempfile.TemporaryDirectory() as test_root:
            root = pathlib.Path(test_root)
            source, web = root / 'source-data', root / 'web'
            (source / 'secrets').mkdir(parents=True)
            web.mkdir()
            credential = '11111111-1111-4111-8111-111111111111'
            (source / 'secrets' / credential).write_text('synthetic-secret', encoding='utf-8')
            binary = root / 'synthetic-binary'
            binary.write_bytes(b'synthetic executable; never launched')
            revision = 'a' * 40
            report = root / 'prepare.json'
            source_run = '22222222-2222-4222-8222-222222222222'
            target = {'kind': 'c2c', 'account_id': '123', 'target': 'test', 'message_id': 'own-input',
                      'credential_ref': credential, 'deadline': int(time.time()) + 600}
            qq = {'enabled': True, 'app_id': '123', 'credential_ref': credential, 'transport': 'websocket'}
            scope = ('qq:123:c2c:test', 'own-conversation', 'own-input', 1, 0, 1500)
            config = {'qq': {}, 'model': {'credential_ref': None}, 'speech': {'enabled': False},
                      'reply': {'min_interval_ms': 1500, 'max_interval_ms': 8000}}
            instances = []
            class PrivateHarness:
                def __init__(self, _binary, _web, temporary, _mock):
                    self.owner, self.commit, self.running = 'private-admin', revision, False
                    self.process = types.SimpleNamespace(poll=lambda: None if self.running else 0)
                    self.starts = 0
                    instances.append(self)
                def start(self):
                    self.starts += 1
                    self.running = True
                def bootstrap(self):
                    pass
                def close(self):
                    self.running = False
                def conversation(self, _title):
                    return 'synthetic-conversation'
                def request(self, method, path, body=None, **_kwargs):
                    if path.endswith('/config'):
                        value = {'config': copy.deepcopy(config), 'revision': 1, 'applied_revision': 1}
                    elif path.endswith('/secrets'):
                        value = {'id': '33333333-3333-4333-8333-333333333333'}
                    elif path.endswith('/config/drafts'):
                        value = {'id': 'draft'}
                    elif path.endswith('/validate'):
                        value = {'valid': True}
                    elif path.endswith('/apply'):
                        value = {}
                    elif path.endswith('/media'):
                        value = {'id': 'asset', 'kind': 'video', 'sha256': MODULE.FIXTURE_SHA256,
                                 'bytes': len(b'synthetic video')}
                    else:
                        raise AssertionError('unexpected local route')
                    return value, b'', {}
            helper = types.SimpleNamespace(Harness=PrivateHarness)
            media = types.SimpleNamespace(INTERFACE_MARKER='DSH接口验收：', MARKER='DSH富媒体验收：')
            arguments = ['qq-multipart-live.py', '--binary', str(binary), '--web-dir', str(web),
                         '--source-data-dir', str(source), '--source-run-id', source_run,
                         '--expected-commit', revision, '--video-fixture', str(root / 'fixture.mp4'),
                         '--report', str(report), '--prepare-only']
            with mock.patch.object(sys, 'argv', arguments), mock.patch.object(MODULE.platform, 'system', return_value='Linux'), \
                 mock.patch.object(MODULE, 'load_module', side_effect=lambda _name, filename: helper if filename == 'memory-smoke.py' else media), \
                 mock.patch.object(MODULE, 'source_snapshot', return_value=(qq, target, scope, int(time.time() * 1000) - 5000)), \
                 mock.patch.object(MODULE, 'script_commit', return_value=revision), \
                 mock.patch.object(MODULE, 'checked_fixture', return_value=b'synthetic video'), \
                 mock.patch.object(MODULE, 'prepare_probe', return_value=({'operation': MODULE.OPERATIONS[0]}, 'prepare_platform_accepted_no_message_sent', False)) as probe, \
                 mock.patch.object(MODULE, 'queued_fixture', side_effect=AssertionError('must not arm Outbox')) as queue, \
                 contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(MODULE.main(), 0)
                probe.assert_called_once()
                queue.assert_not_called()
                self.assertEqual(instances[0].starts, 1)
                recorded = json.loads(report.read_text(encoding='utf-8'))
                self.assertTrue(recorded['prepare_intent_recorded'])
                self.assertFalse(recorded['real_qq_send_intent_recorded'])
                self.assertEqual(recorded['mode'], 'prepare_only')
                self.assertTrue(recorded['cleanup']['daemon_exit_confirmed'])
                self.assertTrue(recorded['cleanup']['private_cleanup_complete'])
                arguments.remove('--prepare-only')
                arguments[arguments.index('--report') + 1] = str(root / 'different-full-report.json')
                self.assertEqual(MODULE.main(), 2)
                probe.assert_called_once()
                queue.assert_not_called()

    def test_request_body_exactly_matches_rust_for_the_pinned_fixture(self):
        source = pathlib.Path(__file__).resolve().parents[2] / 'crates/dshd/testdata/synthetic-h264.mp4'
        data = source.read_bytes() + MODULE.FREE_BYTES.to_bytes(4, 'big') + b'free' + bytes(MODULE.FREE_BYTES - 8)
        self.assertEqual(MODULE.prepare_body(data), {
            'file_type': 2, 'file_size': '6302964', 'file_name': 'dsh-video.mp4',
            'md5': 'dedad711dcbd08497171b4db976d9a3f',
            'sha1': '6d9a0f840a2c4007640a8bf9c7f4cba8efbabd6a',
            'md5_10m': 'dedad711dcbd08497171b4db976d9a3f'})

    def test_response_shape_never_copies_upload_credentials_or_unknown_text(self):
        secret = 'DO_NOT_PERSIST_SYNTHETIC_SECRET'
        response = {'upload_id': secret, 'block_size': secret,
                    'parts': [{'index': 0, 'presigned_url': 'https://cos.qq.com/private?token=' + secret}],
                    'unknown': {'token': secret},
                    'data': {'upload_id': {'$qq_ref': secret}, 'block_size': '5242880',
                             'parts': [{'index': 0, 'presigned_url': '[server-held reference]'}]}}
        entry = MODULE.safe_prepare_result({'outcome': 'platform_accepted', 'platform_accepted': True,
            'http_status': 200, 'business_code': 0, 'trace_id': 'https://secret.invalid/' + secret,
            'object_ref': secret, 'file_info': secret, 'response': response})
        self.assertNotIn(secret, json.dumps(entry))
        self.assertNotIn('https://', json.dumps(entry))
        self.assertIsNone(entry['trace_id'])
        self.assertTrue(entry['trace_id_suppressed'])
        shape = entry['response_shape']
        self.assertTrue(shape['root_fields']['upload_id_raw_nonempty_string'])
        self.assertFalse(shape['root_fields']['upload_id_protected_reference'])
        self.assertIsNone(shape['root_fields']['block_size'])
        self.assertTrue(shape['data_fields']['upload_id_protected_reference'])
        self.assertFalse(shape['data_fields']['upload_id_raw_nonempty_string'])
        self.assertEqual(shape['data_fields']['block_size'], 5242880)
        self.assertTrue(shape['data_fields']['part_indices_ordered'])
        self.assertTrue(shape['data_fields']['all_part_urls_are_protected_references'])
        excess = MODULE.response_shape({'parts': [{'index': index} for index in range(129)]})
        self.assertIsNone(excess['root_fields']['parts_count'])
        self.assertFalse(excess['root_fields']['parts_within_128_bound'])
        boolean_code = MODULE.safe_prepare_result({'outcome': 'platform_accepted',
            'platform_accepted': True, 'http_status': 200, 'business_code': False})
        self.assertFalse(boolean_code['platform_accepted'])

    def probe(self, result=None, *, exception=None, mismatched_record=False, extra_outbox=False):
        with tempfile.TemporaryDirectory() as root:
            path = pathlib.Path(root) / 'private.db'
            with contextlib.closing(sqlite3.connect(path)) as db:
                db.executescript('CREATE TABLE qq_api_calls(id TEXT,owner_id TEXT,account_id TEXT,operation_id TEXT,'
                    'outcome TEXT,http_status INTEGER,business_code INTEGER,trace_id TEXT);CREATE TABLE outbox(id TEXT);')
                if extra_outbox:
                    db.execute("INSERT INTO outbox VALUES('unexpected')")
                    db.commit()
            @contextlib.contextmanager
            def connect():
                with contextlib.closing(sqlite3.connect(path)) as db, db:
                    yield db
            harness = types.SimpleNamespace(owner='private-admin', db=connect)
            target = {'target': 'frozen-test-recipient', 'account_id': 'frozen-account'}
            original_target = copy.deepcopy(target)
            calls = []
            def invoke(runtime, actual_target, body):
                calls.append((actual_target, body))
                if exception:
                    raise exception
                with connect() as db:
                    db.execute('INSERT INTO qq_api_calls VALUES(?,?,?,?,?,?,?,?)',
                        (result['call_id'], 'private-admin', 'frozen-account', MODULE.OPERATIONS[0],
                         'wrong-record' if mismatched_record else result['outcome'], result.get('http_status'),
                         result.get('business_code'), result.get('trace_id')))
                return result
            with mock.patch.object(MODULE, 'queued_fixture', side_effect=AssertionError('prepare must never queue')) as queue:
                actual = MODULE.prepare_probe(harness, target, b'synthetic-fixture-bytes', invoke=invoke)
                queue.assert_not_called()
            self.assertEqual(len(calls), 1)
            self.assertEqual(target, original_target)
            self.assertEqual(calls[0][1]['file_name'], 'dsh-video.mp4')
            with connect() as db:
                self.assertEqual(db.execute('SELECT count(*) FROM outbox').fetchone()[0], int(extra_outbox))
            return actual

    def test_prepare_only_accepts_one_matching_record_and_never_queues(self):
        result = {'call_id': 'private-call', 'outcome': 'platform_accepted', 'platform_accepted': True,
                  'http_status': 200, 'business_code': 0, 'trace_id': '0123456789abcdef0123456789abcdef',
                  'response': {'upload_id': '[server-held reference]', 'block_size': '5242880',
                               'parts': [{'index': 0, 'presigned_url': '[server-held reference]'},
                                         {'index': 1, 'presigned_url': '[server-held reference]'}]}}
        entry, status, failed = self.probe(result)
        self.assertFalse(failed)
        self.assertEqual(status, 'prepare_platform_accepted_no_message_sent')
        self.assertTrue(entry['call_record_matches'])
        self.assertEqual(entry['private_outbox_count'], 0)
        self.assertEqual(entry['trace_id'], result['trace_id'])
        for arguments in [{'mismatched_record': True}, {'extra_outbox': True}]:
            entry, status, failed = self.probe(result, **arguments)
            self.assertTrue(failed)
            self.assertEqual(status, 'prepare_evidence_unconfirmed_no_replay')

    def test_rejection_unknown_and_local_error_are_one_shot_and_not_success(self):
        rejected = {'call_id': 'private-call', 'outcome': 'platform_permission_denied', 'platform_accepted': False,
                    'http_status': 403, 'business_code': 11253, 'trace_id': None, 'response': {}}
        entry, status, failed = self.probe(rejected)
        self.assertTrue(failed)
        self.assertEqual(status, 'prepare_platform_rejected_no_message_sent')
        self.assertEqual(entry['http_status'], 403)
        entry, status, failed = self.probe(exception=TimeoutError('synthetic timeout'))
        self.assertTrue(failed)
        self.assertEqual(status, 'prepare_unknown_or_unconfirmed_no_replay')
        self.assertIsNone(entry['http_status'])
        entry, status, failed = self.probe(exception=MODULE.LocalPrepareFailure(400))
        self.assertTrue(failed)
        self.assertEqual(status, 'prepare_local_request_rejected_no_message_sent')
        self.assertEqual(entry['local_http_status'], 400)


if __name__ == '__main__':
    unittest.main()
