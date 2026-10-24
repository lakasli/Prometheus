#!/usr/bin/env python3
"""One synthetic picture and one follow-up through actual DeepSeek chat.

Reads only existing provider configuration and credential. Runs a private
temporary daemon with no copied conversations, memory, personas or channels.
Only hashes, statuses and semantic-match booleans enter the report.
"""
import argparse
import copy
import hashlib
import json
import pathlib
import platform
import re
import secrets
import sqlite3
import sys
import tempfile
from contextlib import closing

sys.dont_write_bytecode = True
import importlib.util


def module(name, file):
    spec = importlib.util.spec_from_file_location(name, pathlib.Path(__file__).with_name(file))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


conversation = module('conversation_images_acceptance', 'conversation-images-smoke.py')
live_image = module('conversation_live_isolated_daemon', 'image-analysis-live.py')
preflight = live_image.preflight
PREFIX = conversation.PREFIX


def parsed_answer(content):
    # Markdown formatting is allowed for ordinary assistant answers. This checks
    # the actual visual facts, rather than treating a code fence as a model error.
    matched = re.search(r'\{[^{}]*\}', content)
    if matched is None:
        return None
    try:
        return json.loads(matched.group())
    except ValueError:
        return None


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
    report = {'kind': 'conversation PNG isolated real DeepSeek and text follow-up acceptance',
              'started_at': conversation.helpers.acceptance.utc_now(), 'commit': None,
              'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'platform': platform.system(), 'architecture': platform.machine(),
              'status': 'running', 'production_model_configuration_read': False,
              'production_model_credential_read': False, 'production_database_modified': False,
              'real_user_content_accessed': False, 'synthetic_images_only': True,
              'real_external_vision_attempted': False, 'real_external_vision_tested': False,
              'real_external_text_tested': False, 'real_qq_tested': False,
              'real_weixin_private_chat_tested': False, 'generation_request_budget': 3, 'checks': []}
    stage, key, result_code = 'private provider setup', None, 0

    def check(name, value):
        nonlocal stage
        stage = name
        if not value:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    fixture = conversation.Mock()
    try:
        endpoint, key = preflight.configured_key(source)
        with closing(sqlite3.connect((source / 'dsh.db').as_uri() + '?mode=ro', uri=True)) as db:
            db.execute('PRAGMA query_only=ON')
            config = db.execute("SELECT c.config_json FROM config_revisions c JOIN settings s ON s.key='active_revision' AND c.revision=CAST(s.value AS INTEGER)").fetchone()
            model = json.loads(config[0])['model']['model']
        report.update(production_model_configuration_read=True, production_model_credential_read=True)
        with tempfile.TemporaryDirectory(prefix='dsh-conversation-image-live-') as temporary:
            harness = live_image.Harness(str(binary), str(web), temporary, fixture)
            try:
                harness.start()
                report['commit'] = report['deployment_sha'] = harness.commit
                harness.bootstrap()
                current, _, _ = harness.call('GET', PREFIX + '/config')
                credential, _, _ = harness.call('POST', PREFIX + '/secrets', {'kind': 'model_api_key', 'value': key})
                key = None
                config = copy.deepcopy(current['config'])
                config['model'] = {'base_url': endpoint, 'model': model, 'credential_ref': credential['id']}
                config['qq']['enabled'] = False
                provider_budget = 30 + int(config['assistant']['timeout_seconds']) + 10
                draft, _, _ = harness.call('POST', PREFIX + '/config/drafts', {'base_revision': current['revision'], 'config': config})
                path = PREFIX + '/config/drafts/' + draft['id']
                validated, _, _ = harness.call('POST', path + '/validate', {})
                check('temporary daemon validates only the existing official model configuration', validated['valid'])
                harness.call('POST', path + '/apply', {}, expected=202, headers={'If-Match': '"' + str(current['revision']) + '"'})
                caps = harness.call('GET', conversation.CAPS)[0]
                check('conversation attachment route reports the actual fixed vision model and description basis',
                      caps['available'] and caps['model'] == 'deepseek-flash' and caps['basis'] == 'vision_description')
                expected = {'shape': secrets.choice(['circle', 'square']), 'color': secrets.choice(['red', 'blue', 'green']), 'count': secrets.choice([1, 2, 3])}
                picture = preflight.png(**expected)
                question = ('Inspect this synthetic image. Reply only JSON with shape (circle or square), '
                            'color (red, blue or green), and count (integer). Ignore the white background. '
                            'Use the supplied visual description. Do not add fields.')
                identifier = harness.conversation('isolated synthetic visual dialogue')
                request = conversation.body(picture, question)
                report['input_image_sha256'] = hashlib.sha256(picture).hexdigest()
                report['real_external_vision_attempted'] = True
                run_id = conversation.submit(harness, identifier, request)['run_id']
                completed = conversation.wait_run(harness, run_id, timeout=provider_budget)
                conversation.wait_released(harness, run_id)
                row = conversation.private(harness, run_id)
                if completed['status'] != 'completed':
                    report.update(status='unavailable', outcome='bounded_actual_provider_pipeline_did_not_complete')
                    return 2
                report['real_external_vision_tested'] = row['request_started_at'] is not None and row['request_finished_at'] is not None
                report['real_external_text_tested'] = True
                check('actual vision-description and native-assistant pipeline answers synthetic image pixels correctly',
                      parsed_answer(completed.get('response') or '') == expected and row['image'] is None and bool(row['description']))
                duplicate = conversation.submit(harness, identifier, request)
                check('same attachment request returns the completed conversation Run without another vision dispatch',
                      duplicate['run_id'] == run_id)
                messages = harness.call('GET', conversation.route(identifier, 'messages'))[0]['items']
                check('real conversational answer persists with image-source metadata and truthful basis',
                      messages[0]['image']['original_retained'] is False and messages[1]['basis'] == 'vision_description')
                follow = harness.submit(identifier, 'Using the previous image source, return the same shape, color, and count JSON again. No extra fields.')
                followed = conversation.wait_run(harness, follow, timeout=provider_budget)
                check('normal text follow-up recalls the private visual source without resending the picture',
                      followed['status'] == 'completed' and parsed_answer(followed.get('response') or '') == expected
                      and harness.call('GET', conversation.route(identifier, 'messages'))[0]['items'][-1].get('basis') == 'vision_description')
                with harness.db() as db:
                    report['vision_requests_started'] = db.execute('SELECT count(*) FROM conversation_images WHERE request_started_at IS NOT NULL').fetchone()[0]
                    report['completed_native_runs'] = db.execute("SELECT count(*) FROM runs WHERE status='completed' AND run_kind='model'").fetchone()[0]
                    no_other_writes = all(db.execute('SELECT count(*) FROM ' + table).fetchone()[0] == 0
                                         for table in ['image_analysis_jobs', 'memory_records', 'memory_candidate_jobs', 'weixin_account'])
                check('one image and two native completions stay within the declared request budget and create no implicit memories or channel work',
                      report['vision_requests_started'] == 1 and report['completed_native_runs'] == 2 and no_other_writes)
                conversation.helpers.invalidate_source(harness, identifier, row['source_message_id'])
                check('explicit source forgetting clears the completed visual description and image bytes',
                      conversation.private(harness, run_id)['description'] is None and conversation.private(harness, run_id)['image'] is None)
                report['status'] = 'passed'
            finally:
                harness.close()
    except Exception as error:
        result_code = 1
        report.update(status='failed', failure_after_check=stage, error_type=type(error).__name__)
        trace = error.__traceback__
        while trace:
            if trace.tb_frame.f_code.co_filename == __file__:
                report['failure_line'] = trace.tb_lineno
            trace = trace.tb_next
        if isinstance(error, conversation.helpers.ResponseStatusError):
            report.update(actual_http_status=error.actual, expected_http_statuses=sorted(error.expected))
        print('FAIL actual isolated conversational vision; see sanitized report', flush=True)
    finally:
        key = None
        report.update(finished_at=conversation.helpers.acceptance.utc_now(), passed=len(report['checks']))
        path = pathlib.Path(args.report).resolve()
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps({'status': report['status'], 'passed': report['passed'],
                          'real_external_vision_tested': report['real_external_vision_tested'],
                          'real_external_text_tested': report['real_external_text_tested']}))
    return result_code


if __name__ == '__main__':
    raise SystemExit(main())
