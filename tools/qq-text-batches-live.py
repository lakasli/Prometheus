#!/usr/bin/env python3
"""One synthetic QQ batch and one real model call in an isolated instance.

Only read the active production model and single personality plus referenced
model key. Signed synthetic QQ events exercise durable reception and batching;
durable pacing prevents any QQ outbound HTTP. Production is never written.
"""
import argparse
import copy
import hashlib
import importlib.util
import ipaddress
import json
import pathlib
import secrets
import sqlite3
import sys
import tempfile
import threading
import time
import types
import urllib.parse
import uuid
from contextlib import closing

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    'qq_batch_live_helpers', pathlib.Path(__file__).with_name('qq-text-batches-smoke.py'))
batch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(batch)
base = batch.base


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--source-data-dir', required=True)
    parser.add_argument('--report', required=True)
    args = parser.parse_args()
    binary = pathlib.Path(args.binary).resolve(strict=True)
    web = pathlib.Path(args.web_dir).resolve(strict=True)
    source = pathlib.Path(args.source_data_dir).resolve(strict=True)
    report = {'kind': 'isolated signed synthetic QQ batch with real configured model',
              'status': 'running', 'started_at': base.utc_now(), 'commit': None,
              'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'model_request_budget': 1, 'real_external_model_attempted': False,
              'real_external_model_tested': False, 'real_qq_tested': False,
              'qq_outbound_http_tested': False, 'production_database_modified': False,
              'real_user_content_accessed': False, 'synthetic_conversation_only': True,
              'checks': []}
    stage, key = 'read-only model and personality setup', None

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
            model_row = db.execute("SELECT json_extract(c.config_json,'$.model') FROM config_revisions c "
                                   "JOIN settings s ON s.key='active_revision' AND c.revision=CAST(s.value AS INTEGER)").fetchone()
            persona_row = db.execute("SELECT value FROM settings WHERE key='assistant.personality.v1'").fetchone()
        if not model_row or not persona_row:
            raise RuntimeError('active model and saved single personality are required')
        model = json.loads(model_row[0])
        stored_persona = json.loads(persona_row[0])
        fields = ('name', 'description', 'personality', 'scenario', 'system_prompt', 'mes_example')
        personality = {name: stored_persona['card']['data'].get(name, '') for name in fields}
        report['source_personality_revision'] = stored_persona['revision']
        report['production_model_configuration_read'] = True
        url = urllib.parse.urlsplit(model['base_url'])
        if url.scheme != 'https' or not url.hostname or url.hostname in {'localhost', 'localhost.localdomain'}:
            raise RuntimeError('real-model acceptance requires an external HTTPS provider')
        try:
            address = ipaddress.ip_address(url.hostname)
        except ValueError:
            address = None
        if address is not None and not address.is_global:
            raise RuntimeError('private mock cannot count as a real external model')
        store = (source / 'secrets').resolve(strict=True)
        credential_path = (store / str(uuid.UUID(model['credential_ref']))).resolve(strict=True)
        if not credential_path.is_relative_to(store):
            raise RuntimeError('model credential escaped its private store')
        key = credential_path.read_text()
        report['production_model_credential_read'] = True
        with tempfile.TemporaryDirectory(prefix='dsh-qq-batch-live-') as temporary:
            fixture = types.SimpleNamespace(hold_release=threading.Event())
            h = base.Harness(str(binary), str(web), temporary, fixture)
            try:
                h.start()
                h.bootstrap()
                report['commit'] = h.commit
                active, _, _ = h.request('GET', base.PREFIX + '/config')
                secret, _, _ = h.request('POST', base.PREFIX + '/secrets',
                                       {'kind': 'model_api_key', 'value': key})
                key = None
                qq_secret = secrets.token_urlsafe(32)
                qq_key, _, _ = h.request('POST', base.PREFIX + '/secrets',
                                        {'kind': 'qq_app_secret', 'value': qq_secret})
                signer = batch.Signer(batch.openssl_path(), temporary, qq_secret)
                with h.db() as db:
                    db.execute('INSERT INTO reply_account_pacing(account_id,next_ms) VALUES(?,?)',
                               (batch.ACCOUNT, batch.PACING_FENCE_MS))
                config = copy.deepcopy(active['config'])
                config['model'] = dict(model, credential_ref=secret['id'], image_generation=False)
                config['speech']['enabled'] = False
                config['assistant']['timeout_seconds'] = 120
                config['assistant']['max_concurrent_runs'] = 1
                config['qq'] = {'enabled': True, 'app_id': batch.ACCOUNT,
                                'credential_ref': qq_key['id'], 'transport': 'webhook'}
                batch.apply_config(h, config)
                current, _, _ = h.request('GET', base.PREFIX + '/assistant/personality')
                saved, _, _ = h.request('PUT', base.PREFIX + '/assistant/personality',
                                       {'personality': personality}, headers={'If-Match': current['etag']})
                check('temporary instance uses the unchanged production single personality', saved['personality'] == personality)
                # Hold the sole model slot until the three receipts are proven to
                # form exactly one Run. A slow signer must not spend extra calls.
                fence_chat = h.conversation('synthetic one-request budget fence')
                fence_run = str(uuid.uuid4())
                with h.db() as db:
                    db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at) VALUES(?,?,?,?,?,?,'cancelled',?,?)",
                               (fence_run, fence_chat, h.owner, fence_run, 'synthetic model-slot fence',
                                json.dumps(config), int(time.time()), int(time.time())))
                    db.execute('INSERT INTO native_worker_leases(run_id,claimed_at) VALUES(?,?)',
                               (fence_run, int(time.time())))
                user = 'synthetic_live_' + secrets.token_hex(6)
                texts = ['合成验收，我会分三条说完一个问题。', '请结合这几条统一回答。', '17乘19，只回复计算结果。']
                events = [batch.event(user, text) for text in texts]
                for event in events:
                    batch.webhook(h, signer, event)
                runs = batch.received_runs(h, events)
                check('three signed synthetic QQ messages share one queued batch', len(set(runs)) == 1)
                run = runs[0]
                check('one-request budget fence prevents inference before batch verification',
                      batch.rows(h, 'SELECT status FROM runs WHERE id=?', (run,))[0]['status'] == 'queued'
                      and not batch.rows(h, 'SELECT 1 FROM run_prompt_snapshots'))
                with h.db() as db:
                    db.execute('DELETE FROM native_worker_leases WHERE run_id=?', (fence_run,))
                report['real_external_model_attempted'] = True
                deadline = time.monotonic() + 150
                while time.monotonic() < deadline:
                    found = batch.rows(h, 'SELECT status,response,input,reply_json,character_snapshot_json FROM runs WHERE id=?', (run,))
                    if found and found[0]['status'] in base.TERMINAL:
                        break
                    time.sleep(0.1)
                else:
                    raise TimeoutError('bounded real-model batch did not complete')
                result = found[0]
                check('single real-model batch completes and answers the combined question',
                      result['status'] == 'completed' and '323' in result['response'])
                report['real_external_model_tested'] = True
                report['synthetic_response'] = result['response'][:400]
                with h.db() as db:
                    fragments = db.execute('SELECT content FROM qq_text_fragments WHERE run_id=? ORDER BY ordinal', (run,)).fetchall()
                    prompt = json.loads(db.execute('SELECT messages_json FROM run_prompt_snapshots WHERE run_id=?', (run,)).fetchone()[0])
                    outbox = db.execute('SELECT status FROM outbox WHERE run_id=?', (run,)).fetchall()
                    count = db.execute('SELECT count(*) FROM runs WHERE id<>?', (fence_run,)).fetchone()[0]
                    summary_count = db.execute('SELECT count(*) FROM summary_jobs').fetchone()[0]
                    memories = db.execute('SELECT count(*) FROM memory_records').fetchone()[0]
                check('original three fragments and ordered combined prompt are preserved',
                      [row[0] for row in fragments] == texts and result['input'] == '\n'.join(texts)
                      and prompt[-1] == {'role': 'user', 'content': '\n'.join(texts)}
                      and 'DSH_QQ_TEXT_BATCH_V1' in prompt[0]['content']
                      and 'DSH_CONVERSATION_TIME_V1' in prompt[0]['content'])
                check('one logical request produces one fenced Outbox without QQ dispatch',
                      count == 1 and len(outbox) == 1 and outbox[0][0] == 'queued'
                      and not batch.rows(h, "SELECT 1 FROM outbox WHERE status!='queued'")
                      and not batch.rows(h, "SELECT 1 FROM outbox_reply_parts WHERE status!='queued'")
                      and batch.rows(h, 'SELECT next_ms FROM reply_account_pacing WHERE account_id=?', (batch.ACCOUNT,))[0]['next_ms'] == batch.PACING_FENCE_MS
                      and summary_count == 0 and memories == 0)
                check('reply is bound to the latest actual synthetic platform message',
                      json.loads(result['reply_json'])['message_id'] == events[-1]['d']['id'])
                report.update(status='passed', logical_model_requests=1, fragment_count=3)
            finally:
                h.close()
    except Exception as error:
        report.update(status='failed', failure_after_check=stage, error_type=type(error).__name__)
        print('FAIL bounded real-model QQ batch acceptance; inspect report metadata', flush=True)
    finally:
        key = None
        report.update(finished_at=base.utc_now(), passed=len(report['checks']))
        output = pathlib.Path(args.report).resolve()
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps({'status': report['status'], 'passed': report['passed'],
                          'real_qq_tested': False, 'production_database_modified': False}))
    return 0 if report['status'] == 'passed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
