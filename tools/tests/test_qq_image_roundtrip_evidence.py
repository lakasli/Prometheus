import hashlib
import importlib.util
import json
import pathlib
import sqlite3
import tempfile
import unittest

PATH = pathlib.Path(__file__).resolve().parents[1] / 'qq-image-roundtrip-evidence.py'
SPEC = importlib.util.spec_from_file_location('qq_image_roundtrip_evidence_tests', PATH)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
RUN = 'b11e71d5-e4dd-4223-b72a-e820be070408'
ASSET = 'c1775219-16e9-4c56-9f35-78546b80e749'
INPUT = '请用一句话描述附图，再把这张原图发给我。'
COMMIT = 'a' * 40
AT = 1792930000


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.source = pathlib.Path(self.temporary.name)
        self.db = sqlite3.connect(self.source / 'dsh.db')
        self.db.executescript('''
            CREATE TABLE conversations(id TEXT,owner_id TEXT);
            CREATE TABLE runs(id TEXT,conversation_id TEXT,owner_id TEXT,input TEXT,status TEXT,
                run_kind TEXT,reply_json TEXT,config_json TEXT,created_at INTEGER);
            CREATE TABLE memory_context_watermarks(conversation_id TEXT,through_run_rowid INTEGER);
            CREATE TABLE messages(id TEXT,run_id TEXT,conversation_id TEXT,role TEXT,content TEXT);
            CREATE TABLE conversation_images(run_id TEXT,owner_id TEXT,conversation_id TEXT,
                source_message_id TEXT,status TEXT,image_sha256 TEXT,width INTEGER,height INTEGER);
            CREATE TABLE run_reply_plans(run_id TEXT,plan_json TEXT);
            CREATE TABLE conversation_media_assets(id TEXT,owner_id TEXT,conversation_id TEXT,
                kind TEXT,mime TEXT,sha256 TEXT,bytes BLOB,expires_at INTEGER,context_floor INTEGER,
                source_run_id TEXT);
            CREATE TABLE outbox(id TEXT,run_id TEXT,status TEXT,error TEXT,account_id TEXT,
                credential_ref TEXT,target_json TEXT,deadline INTEGER);
            CREATE TABLE outbox_reply_parts(outbox_id TEXT,seq INTEGER,status TEXT,error TEXT,
                payload_json TEXT,platform_message_id TEXT,finished_ms INTEGER,delay_ms INTEGER);
        ''')
        self.png = b'\x89PNG\r\n\x1a\n' + b'synthetic-scope-integrity-fixture'
        self.sha = hashlib.sha256(self.png).hexdigest()
        self.owner = 'qq:synthetic-account:c2c:synthetic-recipient'
        target = {'kind': 'c2c', 'target': 'synthetic-recipient', 'account_id': 'synthetic-account',
                  'credential_ref': 'synthetic-reference', 'message_id': 'synthetic-inbound',
                  'deadline': AT + 3600}
        self.items = [{'type': 'text', 'text': '合成图案。', 'delay_ms': 0},
                      {'type': 'image', 'text': '原图。', 'asset_id': ASSET, 'delay_ms': 1500}]
        self.db.execute('INSERT INTO conversations VALUES(?,?)', ('c', self.owner))
        self.db.execute('INSERT INTO runs VALUES(?,?,?,?,?,?,?,?,?)',
            (RUN, 'c', self.owner, INPUT, 'completed', 'model', json.dumps(target),
             json.dumps({'reply': {'min_interval_ms': 1500}, 'DO_NOT_EXPORT': 'frozen-config-canary'}), AT))
        self.db.execute('INSERT INTO messages VALUES(?,?,?,?,?)', ('source', RUN, 'c', 'user', INPUT))
        self.db.execute('INSERT INTO conversation_images VALUES(?,?,?,?,?,?,?,?)',
            (RUN, self.owner, 'c', 'source', 'described', self.sha, 1, 1))
        self.db.execute('INSERT INTO run_reply_plans VALUES(?,?)',
            (RUN, json.dumps({'version': 1, 'normalized': True, 'items': self.items})))
        self.db.execute('INSERT INTO conversation_media_assets VALUES(?,?,?,?,?,?,?,?,?,?)',
            (ASSET, self.owner, 'c', 'image', 'image/png', self.sha, self.png, AT + 86400, 0, None))
        self.db.execute('INSERT INTO outbox VALUES(?,?,?,?,?,?,?,?)',
            ('out', RUN, 'sent', None, 'synthetic-account', 'synthetic-reference', json.dumps(target), target['deadline']))
        for index, item in enumerate(self.items, 1):
            self.db.execute('INSERT INTO outbox_reply_parts VALUES(?,?,?,?,?,?,?,?)',
                ('out', index, 'sent', None, json.dumps(item), 'synthetic-platform-' + str(index),
                 AT * 1000 + index * 1500, item['delay_ms']))
        self.db.commit()

    def tearDown(self):
        self.db.close()
        self.temporary.cleanup()

    def collect(self, **kwargs):
        self.db.commit()
        return MODULE.collect(self.source, RUN, kwargs.get('input', INPUT),
                              kwargs.get('sha', self.sha), COMMIT, 'b' * 40, at=AT + 60)

    def test_valid_delivery_has_private_metadata_only_and_never_promotes_client_or_commit(self):
        report, result = self.collect()
        self.assertEqual(result, 0, report)
        self.assertTrue(report['real_qq_tested'])
        self.assertFalse(report['client_verified'])
        self.assertFalse(report['binary_commit_verified_by_script'])
        self.assertEqual(report['reply_item_count'], 2)
        self.assertEqual(report['parts'][1]['gap_ms'], 1500)
        exported = json.dumps(report)
        for canary in [INPUT, ASSET, RUN, self.owner, 'synthetic-recipient', 'synthetic-platform',
                       'synthetic-reference', 'frozen-config-canary', '合成图案', '原图。']:
            self.assertNotIn(canary, exported)

    def test_wrong_input_hash_message_or_web_scope_cannot_pass(self):
        self.assertEqual(self.collect(input='different synthetic input')[1], 1)
        self.assertEqual(self.collect(sha='0' * 64)[1], 1)
        self.db.execute("UPDATE messages SET content='not current input'")
        self.assertEqual(self.collect()[1], 1)
        self.db.execute('UPDATE messages SET content=?', (INPUT,))
        self.db.execute('UPDATE runs SET reply_json=NULL')
        self.assertEqual(self.collect()[1], 1)

    def test_sent_text_fallback_and_unknown_delivery_are_failures_not_image_success(self):
        self.db.execute("UPDATE outbox_reply_parts SET error='媒体发送失败后已改发文字' WHERE seq=2")
        self.assertEqual(self.collect()[1], 1)
        self.db.execute('UPDATE outbox_reply_parts SET error=NULL')
        self.db.execute("UPDATE outbox SET status='unknown'")
        report, result = self.collect()
        self.assertEqual(result, 1)
        self.assertFalse(report['real_qq_tested'])

    def test_incomplete_is_two_and_spacing_mismatch_is_failure(self):
        self.db.execute("UPDATE runs SET status='running'")
        self.assertEqual(self.collect()[1], 2)
        self.db.execute("UPDATE runs SET status='completed'")
        self.db.execute('UPDATE outbox_reply_parts SET finished_ms=finished_ms-1 WHERE seq=2')
        self.assertEqual(self.collect()[1], 1)

    def test_asset_bytes_floor_ttl_and_plan_reference_are_rechecked(self):
        self.db.execute("UPDATE conversation_media_assets SET bytes=x'89504e470d0a1a0a'")
        self.assertEqual(self.collect()[1], 1)
        self.db.execute('UPDATE conversation_media_assets SET bytes=?', (self.png,))
        self.db.execute('UPDATE conversation_media_assets SET expires_at=0')
        self.assertEqual(self.collect()[1], 1)
        self.db.execute('UPDATE conversation_media_assets SET expires_at=?', (AT + 86400,))
        self.db.execute('INSERT INTO memory_context_watermarks VALUES(?,?)', ('c', 1))
        self.assertEqual(self.collect()[1], 1)

    def test_all_source_access_is_read_only_and_no_network_is_needed(self):
        before = (self.source / 'dsh.db').read_bytes()
        self.assertEqual(self.collect()[1], 0)
        self.assertEqual((self.source / 'dsh.db').read_bytes(), before)


if __name__ == '__main__':
    unittest.main()
