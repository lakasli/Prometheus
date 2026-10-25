import contextlib
import copy
import importlib.util
import json
import pathlib
import sqlite3
import tempfile
import time
import types
import unittest

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


if __name__ == '__main__':
    unittest.main()
