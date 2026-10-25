import importlib.util
import json
import pathlib
import sqlite3
import tempfile
import time
import unittest

PATH = pathlib.Path(__file__).resolve().parents[1] / 'qq-media-live.py'
SPEC = importlib.util.spec_from_file_location('qq_media_live_tests', PATH)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class SourceScopeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temporary.name)
        self.db = sqlite3.connect(self.root / 'dsh.db')
        self.db.executescript('CREATE TABLE config_revisions(revision INTEGER,config_json TEXT);CREATE TABLE settings(key TEXT,value TEXT);CREATE TABLE runs(id TEXT,owner_id TEXT,input TEXT,reply_json TEXT,status TEXT,created_at INTEGER);CREATE TABLE outbox(id TEXT,run_id TEXT,status TEXT,target_json TEXT,account_id TEXT,credential_ref TEXT,deadline INTEGER);CREATE TABLE outbox_reply_parts(outbox_id TEXT,seq INTEGER,status TEXT);')
        self.qq = {'enabled': True, 'app_id': '123', 'credential_ref': 'synthetic-key', 'transport': 'websocket'}
        self.target = {'kind': 'c2c', 'target': 'synthetic-recipient', 'message_id': 'synthetic-inbound', 'account_id': '123', 'credential_ref': 'synthetic-key', 'deadline': int(time.time()) + 600}
        self.db.execute('INSERT INTO config_revisions VALUES(1,?)', (json.dumps({'qq': self.qq}),))
        self.db.execute("INSERT INTO settings VALUES('active_revision','1')")
        self.db.execute('INSERT INTO runs VALUES(?,?,?,?,?,?)', ('run-id', 'qq:123:c2c:synthetic-recipient', MODULE.MARKER + 'synthetic request', json.dumps(self.target), 'completed', int(time.time())))
        self.db.execute('INSERT INTO outbox VALUES(?,?,?,?,?,?,?)', ('outbox-id', 'run-id', 'sent', json.dumps(self.target), '123', 'synthetic-key', self.target['deadline']))
        self.db.execute("INSERT INTO outbox_reply_parts VALUES('outbox-id',1,'sent')")
        self.db.commit()

    def tearDown(self):
        self.db.close()
        self.temporary.cleanup()

    def test_valid_explicit_test_context(self):
        qq, target = MODULE.source_context(self.root, 'run-id')
        self.assertEqual(qq, self.qq)
        self.assertEqual(target, self.target)

    def test_plain_chat_cannot_authorize_media_probe(self):
        self.db.execute("UPDATE runs SET input='ordinary user request'")
        self.db.commit()
        with self.assertRaises(RuntimeError):
            MODULE.source_context(self.root, 'run-id')

    def test_interface_probe_requires_its_distinct_explicit_marker(self):
        self.db.execute('UPDATE runs SET input=?', (MODULE.INTERFACE_MARKER + 'synthetic controls request',))
        self.db.commit()
        with self.assertRaises(RuntimeError):
            MODULE.source_context(self.root, 'run-id')
        qq, target = MODULE.source_context(self.root, 'run-id', required_marker=MODULE.INTERFACE_MARKER)
        self.assertEqual(qq, self.qq)
        self.assertEqual(target, self.target)

    def test_arbitrary_or_empty_marker_cannot_weaken_the_guard(self):
        with self.assertRaises(ValueError):
            MODULE.source_context(self.root, 'run-id', required_marker='')

    def test_more_than_one_existing_reply_blocks_probe(self):
        self.db.execute("INSERT INTO outbox_reply_parts VALUES('outbox-id',2,'queued')")
        self.db.commit()
        with self.assertRaises(RuntimeError):
            MODULE.source_context(self.root, 'run-id')

    def test_unknown_existing_delivery_blocks_probe(self):
        self.db.execute("UPDATE outbox SET status='unknown'")
        self.db.commit()
        with self.assertRaises(RuntimeError):
            MODULE.source_context(self.root, 'run-id')

    def test_changed_credential_blocks_probe(self):
        changed = dict(self.qq, credential_ref='another-credential')
        self.db.execute('UPDATE config_revisions SET config_json=?', (json.dumps({'qq': changed}),))
        self.db.commit()
        with self.assertRaises(RuntimeError):
            MODULE.source_context(self.root, 'run-id')

    def test_short_reply_window_blocks_probe(self):
        self.db.execute('UPDATE outbox SET deadline=?', (int(time.time()) + 100,))
        self.db.commit()
        with self.assertRaises(RuntimeError):
            MODULE.source_context(self.root, 'run-id')

    def test_source_owner_must_match_frozen_recipient(self):
        self.db.execute("UPDATE runs SET owner_id='qq:123:c2c:someone-else'")
        self.db.commit()
        with self.assertRaises(RuntimeError):
            MODULE.source_context(self.root, 'run-id')

    def test_synthetic_image_is_an_actual_bounded_png(self):
        data = MODULE.synthetic_png()
        self.assertTrue(data.startswith(b'\x89PNG\r\n\x1a\n'))
        self.assertLess(len(data), 5 * 1024 * 1024)
        self.assertEqual(data[-8:-4], b'IEND')


if __name__ == '__main__':
    unittest.main()
