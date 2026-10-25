#!/usr/bin/env python3
"""Read-only evidence for one explicitly identified synthetic QQ image Run.

No discovery, retries, model calls, QQ calls, configuration/credential loading or
message-body exports. SQLite returns only equality results and needed metadata;
the only blob read is the explicitly referenced, scoped PNG. --expected-commit
is a caller assertion unless --verify-local-health checks the fixed loopback
ready endpoint. Use a fresh report filename for each repeat observation.
"""
import argparse
from contextlib import closing
import datetime
import hashlib
import json
import os
import pathlib
import re
import sqlite3
import subprocess
import time
import urllib.request
import uuid

READY_URL = 'http://127.0.0.1:8419/health/ready'
SHA256 = re.compile(r'^[0-9a-f]{64}$')
COMMIT = re.compile(r'^[0-9a-f]{40}$')


class EvidenceFailure(Exception):
    def __init__(self, check, incomplete=False):
        super().__init__(check)
        self.check = check
        self.incomplete = incomplete


def check(report, name, passed, incomplete=False):
    report['checks'].append({'check': name, 'passed': bool(passed)})
    if not passed:
        raise EvidenceFailure(name, incomplete)


def collect(source, run_id, expected_input, expected_sha, expected_commit,
            script_commit=None, at=None):
    """All source access is one read-only SQLite snapshot for this exact Run."""
    at = int(time.time()) if at is None else int(at)
    report = {
        'kind': 'read-only QQ current-image roundtrip evidence',
        'observed_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'script_commit': script_commit,
        'expected_binary_commit': expected_commit,
        'binary_commit_verified_by_script': False,
        'binary_commit_check_scope': 'current_readiness_at_observation_not_historical_run_provenance',
        'run_sha256': hashlib.sha256(run_id.encode()).hexdigest(),
        'expected_input_sha256': hashlib.sha256(expected_input.encode()).hexdigest(),
        'expected_image_sha256': expected_sha,
        'production_database_modified': False,
        'credential_read': False,
        'message_bodies_exported': False,
        'model_or_qq_requests': 0,
        'automatic_retries': False,
        'client_verified': False,
        'real_qq_tested': False,
        'checks': [],
        'parts': [],
    }
    try:
        check(report, 'valid_arguments',
              str(uuid.UUID(run_id)) == run_id and bool(expected_input) and
              len(expected_input.encode()) <= 16000 and
              SHA256.fullmatch(expected_sha) and COMMIT.fullmatch(expected_commit) and
              (script_commit is None or COMMIT.fullmatch(script_commit)))
        database = pathlib.Path(source).resolve(strict=True) / 'dsh.db'
        with closing(sqlite3.connect(database.as_uri() + '?mode=ro', uri=True)) as db:
            db.row_factory = sqlite3.Row
            db.execute('PRAGMA query_only=ON')
            db.execute('BEGIN')
            run = db.execute('''SELECT r.status,r.created_at,r.rowid,
                r.input=:expected_input AS input_matches,r.owner_id=c.owner_id AS owner_matches,
                r.run_kind='model' AS ordinary_model,
                coalesce(w.through_run_rowid,0) AS floor,
                json_extract(r.config_json,'$.reply.min_interval_ms') AS min_interval,
                json_extract(r.reply_json,'$.kind')='c2c'
                  AND length(json_extract(r.reply_json,'$.target'))>0
                  AND length(json_extract(r.reply_json,'$.message_id'))>0
                  AND length(json_extract(r.reply_json,'$.account_id'))>0
                  AND length(json_extract(r.reply_json,'$.credential_ref'))>0
                  AND r.owner_id='qq:'||json_extract(r.reply_json,'$.account_id')
                      ||':c2c:'||json_extract(r.reply_json,'$.target') AS c2c_owner
                FROM runs r JOIN conversations c ON c.id=r.conversation_id
                LEFT JOIN memory_context_watermarks w ON w.conversation_id=r.conversation_id
                WHERE r.id=:run''', {'run': run_id, 'expected_input': expected_input}).fetchone()
            check(report, 'exact_run_exists', run is not None)
            check(report, 'exact_input_matches_without_export', run['input_matches'])
            check(report, 'frozen_qq_c2c_owner', run['c2c_owner'] and run['owner_matches'] and run['ordinary_model'])
            check(report, 'run_completed', run['status'] == 'completed', run['status'] in {'queued', 'running'})
            check(report, 'run_after_forget_floor', run['rowid'] > run['floor'])
            report['run_created_at'] = run['created_at']
            minimum = run['min_interval']
            check(report, 'frozen_interval_valid', type(minimum) is int and 1000 <= minimum <= 10000)
            report['frozen_min_interval_ms'] = minimum
            image = db.execute('''SELECT i.status,i.image_sha256,i.width,i.height,
                i.owner_id=r.owner_id AND i.conversation_id=r.conversation_id AS scope_matches,
                m.run_id=r.id AND m.role='user' AND m.content=r.input
                  AND m.conversation_id=r.conversation_id AS source_message_matches
                FROM conversation_images i JOIN runs r ON r.id=i.run_id
                LEFT JOIN messages m ON m.id=i.source_message_id WHERE i.run_id=?''', (run_id,)).fetchone()
            check(report, 'current_image_source_exists', image is not None)
            check(report, 'current_image_source_scope_and_message', image['scope_matches'] and image['source_message_matches'])
            check(report, 'current_image_described', image['status'] == 'described', image['status'] in {'queued', 'running'})
            check(report, 'canonical_source_hash_matches', image['image_sha256'] == expected_sha)
            report['image_dimensions'] = [image['width'], image['height']]
            plan = db.execute('''SELECT version,
                json_type(plan_json,'$.items')='array' AS items_array,
                json_array_length(plan_json,'$.items') AS count FROM
                (SELECT plan_json,json_extract(plan_json,'$.version') AS version
                 FROM run_reply_plans WHERE run_id=?)''', (run_id,)).fetchone()
            check(report, 'persisted_reply_plan_exists', plan is not None)
            check(report, 'reply_plan_version_and_array', plan['version'] == 1 and plan['items_array'])
            items = db.execute('''SELECT json_extract(j.value,'$.type') AS kind,
                json_extract(j.value,'$.asset_id') AS asset_id,
                json_extract(j.value,'$.prompt') IS NULL AS no_generation,
                json_extract(j.value,'$.delay_ms') AS delay_ms
                FROM run_reply_plans p,json_each(p.plan_json,'$.items') j
                WHERE p.run_id=? ORDER BY CAST(j.key AS INTEGER)''', (run_id,)).fetchall()
            kinds = [item['kind'] for item in items]
            check(report, 'exactly_one_current_image_reply', kinds in [['image'], ['text', 'image']] and len(items) == plan['count'])
            check(report, 'reply_plan_delays_and_text_fields_valid',
                  all(type(item['delay_ms']) is int and item['delay_ms'] >= 0 and item['no_generation']
                      for item in items) and items[0]['delay_ms'] == 0 and
                  (len(items) == 1 or (items[0]['asset_id'] is None and items[1]['delay_ms'] >= minimum)))
            image_item = items[-1]
            check(report, 'asset_reference_without_generation', image_item['no_generation'] and
                  isinstance(image_item['asset_id'], str) and
                  str(uuid.UUID(image_item['asset_id'])) == image_item['asset_id'])
            asset = db.execute('''SELECT a.bytes,a.sha256,a.expires_at,
                a.owner_id=r.owner_id AND a.conversation_id=r.conversation_id AS scoped,
                a.context_floor=coalesce(w.through_run_rowid,0) AS current_floor,
                a.kind='image' AND a.mime='image/png' AS png,
                a.source_run_id IS NULL OR EXISTS(SELECT 1 FROM runs s
                  WHERE s.id=a.source_run_id AND s.owner_id=a.owner_id
                  AND s.conversation_id=a.conversation_id AND s.status='completed') AS source_active
                FROM conversation_media_assets a JOIN runs r ON r.id=:run
                LEFT JOIN memory_context_watermarks w ON w.conversation_id=r.conversation_id
                WHERE a.id=:asset_id AND length(a.bytes)<=4194304''',
                {'run': run_id, 'asset_id': image_item['asset_id']}).fetchone()
            check(report, 'referenced_asset_exists_within_bound', asset is not None)
            check(report, 'asset_scope_floor_ttl_and_kind', asset['scoped'] and asset['current_floor'] and
                  asset['png'] and asset['source_active'] and asset['expires_at'] > at)
            data = asset['bytes']
            actual_sha = hashlib.sha256(data).hexdigest()
            check(report, 'actual_png_bytes_match_current_source', data.startswith(b'\x89PNG\r\n\x1a\n') and
                  actual_sha == asset['sha256'] == expected_sha)
            report['asset_bytes'] = len(data)
            report['asset_sha256'] = actual_sha
            outbox = db.execute('''SELECT o.id,o.status,o.error IS NULL AS no_error,
                o.account_id=json_extract(r.reply_json,'$.account_id')
                AND o.credential_ref=json_extract(r.reply_json,'$.credential_ref')
                AND json_extract(o.target_json,'$.kind')=json_extract(r.reply_json,'$.kind')
                AND json_extract(o.target_json,'$.target')=json_extract(r.reply_json,'$.target')
                AND json_extract(o.target_json,'$.message_id')=json_extract(r.reply_json,'$.message_id')
                AND json_extract(o.target_json,'$.account_id')=json_extract(r.reply_json,'$.account_id')
                AND json_extract(o.target_json,'$.credential_ref')=json_extract(r.reply_json,'$.credential_ref')
                AND json_extract(o.target_json,'$.deadline')=json_extract(r.reply_json,'$.deadline')
                AND o.deadline=json_extract(r.reply_json,'$.deadline') AS frozen_target_matches
                FROM outbox o JOIN runs r ON r.id=o.run_id WHERE o.run_id=?''', (run_id,)).fetchone()
            check(report, 'outbox_exists', outbox is not None)
            check(report, 'outbox_matches_frozen_c2c_target', outbox['frozen_target_matches'])
            check(report, 'outbox_sent_without_error', outbox['status'] == 'sent' and outbox['no_error'],
                  outbox['status'] in {'queued', 'sending'})
            parts = db.execute('''SELECT p.seq,p.status,p.finished_ms,p.error IS NULL AS no_error,
                p.platform_message_id IS NOT NULL AND length(p.platform_message_id)>0 AS has_platform_id,
                json_extract(p.payload_json,'$.type') AS kind,
                json_extract(p.payload_json,'$.asset_id') IS json_extract(q.plan_json,'$.items['||(p.seq-1)||'].asset_id')
                AND json_extract(p.payload_json,'$.text') IS json_extract(q.plan_json,'$.items['||(p.seq-1)||'].text')
                AND json_extract(p.payload_json,'$.prompt') IS json_extract(q.plan_json,'$.items['||(p.seq-1)||'].prompt')
                AND p.delay_ms=json_extract(q.plan_json,'$.items['||(p.seq-1)||'].delay_ms') AS plan_matches
                FROM outbox_reply_parts p JOIN outbox o ON o.id=p.outbox_id
                JOIN run_reply_plans q ON q.run_id=o.run_id WHERE o.run_id=? ORDER BY p.seq''', (run_id,)).fetchall()
            check(report, 'parts_count_and_sequences_match_plan', len(parts) == len(items) and
                  [part['seq'] for part in parts] == list(range(1, len(items) + 1)))
            for index, part in enumerate(parts):
                check(report, 'part_' + str(part['seq']) + '_matches_plan',
                      part['kind'] == items[index]['kind'] and part['plan_matches'])
                check(report, 'part_' + str(part['seq']) + '_sent_without_fallback',
                      part['status'] == 'sent' and part['no_error'] and part['has_platform_id'],
                      part['status'] in {'queued', 'sending'})
                check(report, 'part_' + str(part['seq']) + '_has_finish_time',
                      type(part['finished_ms']) is int and part['finished_ms'] > 0)
                metadata = {'seq': part['seq'], 'kind': part['kind'], 'sent': True,
                            'platform_message_id_present': True, 'error_is_null': True,
                            'finished_ms': part['finished_ms']}
                if index:
                    gap = part['finished_ms'] - parts[index - 1]['finished_ms']
                    required = max(minimum, items[index]['delay_ms'])
                    metadata.update(gap_ms=gap, required_minimum_ms=required)
                    check(report, 'part_' + str(part['seq']) + '_paced', gap >= required)
                report['parts'].append(metadata)
            report['reply_item_count'] = len(items)
            report['image_item_count'] = kinds.count('image')
            report['real_qq_tested'] = True
            report['status'] = 'platform_delivery_verified_client_pending'
        return report, 0
    except EvidenceFailure as failure:
        report['status'] = 'incomplete' if failure.incomplete else 'failed'
        report['failure_check'] = failure.check
        return report, 2 if failure.incomplete else 1
    except Exception as failure:
        report['status'] = 'failed'
        report['failure_check'] = 'bounded_read_or_schema_validation_failed'
        report['exception_type'] = type(failure).__name__
        return report, 1


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def verify_health(report, expected_commit):
    """Optional fixed local readiness only; no credentials, proxies or redirects."""
    try:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        with opener.open(READY_URL, timeout=5) as response:
            data = response.read(16385)
            value = json.loads(data) if len(data) <= 16384 else {}
            passed = response.status == 200 and value.get('status') == 'ready' and value.get('commit') == expected_commit
        report['checks'].append({'check': 'fixed_loopback_ready_commit_matches', 'passed': passed})
        report['binary_commit_verified_by_script'] = bool(passed)
        if passed:
            report['observed_binary_commit'] = expected_commit
        return passed
    except Exception:
        report['checks'].append({'check': 'fixed_loopback_ready_commit_matches', 'passed': False})
        return False


def infer_script_commit():
    try:
        value = subprocess.run(['git', '-C', str(pathlib.Path(__file__).resolve().parents[1]),
                                'rev-parse', 'HEAD'], check=True, capture_output=True, text=True,
                               timeout=5).stdout.strip()
        return value if COMMIT.fullmatch(value) else None
    except (OSError, subprocess.SubprocessError):
        return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-data-dir', required=True)
    parser.add_argument('--run-id', required=True)
    parser.add_argument('--expected-input', required=True)
    parser.add_argument('--expected-image-sha256', required=True)
    parser.add_argument('--expected-commit', required=True)
    parser.add_argument('--script-commit')
    parser.add_argument('--verify-local-health', action='store_true')
    parser.add_argument('--report', required=True)
    args = parser.parse_args()
    inferred = infer_script_commit()
    if args.script_commit and inferred and args.script_commit != inferred:
        print('FAIL script_commit_does_not_match_source_checkout')
        return 1
    report, result = collect(args.source_data_dir, args.run_id, args.expected_input,
        args.expected_image_sha256, args.expected_commit, args.script_commit or inferred)
    report['script_commit_verified_from_checkout'] = bool(inferred)
    if args.verify_local_health and not verify_health(report, args.expected_commit):
        report['status'] = 'failed'
        report['failure_check'] = 'fixed_loopback_ready_commit_matches'
        result = 1
    destination = pathlib.Path(args.report).resolve()
    destination.parent.mkdir(parents=True, exist_ok=True)
    try:
        fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, 'w', encoding='utf-8') as output:
            json.dump(report, output, ensure_ascii=True, indent=2)
            output.write('\n')
    except FileExistsError:
        print('FAIL evidence_report_already_exists')
        return 1
    print(('PASS' if result == 0 else 'INCOMPLETE' if result == 2 else 'FAIL') +
          ' read_only_current_image_roundtrip')
    return result


if __name__ == '__main__':
    raise SystemExit(main())
