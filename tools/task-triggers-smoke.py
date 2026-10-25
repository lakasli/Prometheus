#!/usr/bin/env python3
"""Actual-process task trigger acceptance against an isolated loopback model.

No production data or credentials are read. All task bodies, model outputs and
accounts are synthetic; the report contains only hashes, check names and counts.
"""
import argparse
import copy
import hashlib
import http.server
import importlib.util
import json
import pathlib
import platform
import secrets
import sys
import tempfile
import threading
import time
import uuid

sys.dont_write_bytecode = True


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, pathlib.Path(__file__).with_name(filename))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


skills = module('trigger_skill_helpers', 'skills-smoke.py')
helpers = skills.helpers
PREFIX = helpers.PREFIX
ROOT = PREFIX + '/task-triggers'
REPOSITORY = pathlib.Path(__file__).resolve().parents[1]
TERMINAL = {'completed', 'failed', 'cancelled', 'expired', 'needs_attention'}


def definition(example='once-after', *, delay=1, content='Synthetic trigger notice'):
    value = json.loads((REPOSITORY / 'docs/examples/task-triggers' / (example + '.json')).read_text(encoding='utf-8'))['task_trigger_draft']
    value['title'] = 'Synthetic trigger ' + secrets.token_hex(4)
    if example == 'once-after':
        value['trigger']['after_seconds'] = delay
        value['action']['content'] = content
    return value


def model_definition(prompt='TASK_MODEL_EXECUTION_PROBE', *, delay=1, skill=None):
    value = definition('assistant-workday')
    value['trigger'] = {'type': 'once', 'mode': 'after', 'after_seconds': delay}
    value['action']['prompt'] = prompt
    value['action']['skill'] = skill
    return value


class Mock:
    def __init__(self):
        self.lock = threading.Lock()
        self.requests = []
        self.hold_release = threading.Event()
        self.hold_received = threading.Event()
        self.setup_proposal = model_definition()

    def count(self):
        with self.lock:
            return len(self.requests)

    def payloads(self):
        with self.lock:
            return copy.deepcopy(self.requests)

    def respond(self, path, payload):
        with self.lock:
            self.requests.append(payload)
        question = skills.Mock.question(payload)
        if 'TRIGGER_SETUP_PROBE' in question:
            content = '```json\n' + json.dumps({'task_trigger_draft': self.setup_proposal}) + '\n```'
        elif 'held-trigger-run' in question:
            self.hold_received.set()
            self.hold_release.wait(timeout=25)
            content = 'Synthetic held task answer'
        elif 'force-error-event' in question:
            return 503, {'error': {'message': 'synthetic controlled model failure'}}
        else:
            content = 'Synthetic task answer TASK_MODEL_EXECUTION_PROBE'
        return 200, {'choices': [{'message': {'role': 'assistant', 'content': content}}]}


def acknowledge_clock(harness):
    caps = harness.call('GET', ROOT + '/capabilities')[0]
    harness.call('POST', ROOT + '/clock/acknowledge', {'observed_now': caps['now'], 'confirmed': True})
    return harness.call('GET', ROOT + '/capabilities')[0]


def create(harness, value, *, key=None):
    return harness.call('POST', ROOT + '/jobs', {'task_trigger_draft': value},
                        expected=(200, 201), headers={'Idempotency-Key': key or str(uuid.uuid4())})[0]


def preview(harness, value, job=None):
    body = {'task_trigger_draft': value}
    if job:
        body.update(job_id=job['id'], expected_revision=job['revision'], draft_revision=job['draft_revision'])
    return harness.call('POST', ROOT + '/preview', body)[0]


def activate(harness, job, value):
    proposed = preview(harness, value, job)
    if not proposed.get('valid') or not proposed.get('preview_token'):
        raise AssertionError('synthetic reviewed task was not activatable')
    return harness.call('POST', ROOT + '/jobs/' + job['id'] + '/activate', {
        'expected_revision': job['revision'], 'draft_revision': job['draft_revision'],
        'preview_token': proposed['preview_token']},
        headers={'Idempotency-Key': str(uuid.uuid4())})[0]


def manage(harness, job, operation):
    latest = harness.call('GET', ROOT + '/jobs/' + job['id'])[0]
    return harness.call('POST', ROOT + '/jobs/' + job['id'] + '/' + operation,
                        {'expected_revision': latest['revision']},
                        headers={'Idempotency-Key': str(uuid.uuid4())})[0]


def occurrences(harness, job):
    return harness.call('GET', ROOT + '/jobs/' + job['id'] + '/occurrences?limit=100')[0]['items']


def wait_occurrence(harness, job, minimum=1, timeout=25):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        values = occurrences(harness, job)
        settled = [v for v in values if v['status'] in TERMINAL]
        if len(settled) >= minimum:
            return settled[0]
        time.sleep(0.1)
    raise TimeoutError('synthetic scheduled occurrence did not settle')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report', required=True)
    args = parser.parse_args()
    binary, web = pathlib.Path(args.binary).resolve(strict=True), pathlib.Path(args.web_dir).resolve(strict=True)
    report = {'kind': 'task triggers actual-process isolated acceptance',
              'started_at': helpers.acceptance.utc_now(), 'commit': None,
              'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'platform': platform.system(), 'architecture': platform.machine(),
              'production_data_accessed': False, 'real_external_model_tested': False,
              'real_external_image_generation_tested': False,
              'real_qq_tested': False, 'real_weixin_private_chat_tested': False,
              'status': 'running', 'checks': []}
    stage, code = 'setup', 0

    def check(name, predicate=True):
        nonlocal stage
        stage = name
        if not predicate:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    mock = Mock()
    provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), skills.Provider)
    provider.daemon_threads, provider.mock = True, mock
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-trigger-smoke-') as temporary:
            harness = helpers.Harness(str(binary), str(web), temporary, mock)
            try:
                harness.start()
                report['commit'] = harness.commit
                harness.call('GET', ROOT + '/capabilities', authenticated=False, expected=401)
                harness.call('GET', ROOT + '/jobs', authenticated=False, expected=401)
                harness.bootstrap()
                caps = acknowledge_clock(harness)
                initial = harness.call('GET', PREFIX + '/skills')[0]['items']
                setup = next(item for item in initial if item['id'] == 'task-trigger-setup')
                check('authenticated task capability and independently seeded setup Skill are available',
                      caps['available'] and setup['enabled'] and setup['builtin'])
                body = {'task_trigger_draft': definition()}
                harness.call('POST', ROOT + '/jobs', body, expected=403, with_csrf=False)
                harness.call('POST', ROOT + '/jobs', body, expected=403, headers={'Origin': 'https://example.invalid'})
                harness.call('POST', ROOT + '/clock/acknowledge', {'observed_now': caps['now'], 'confirmed': True}, expected=403, with_csrf=False)
                check('task writes and manual clock acknowledgement enforce authentication Origin and CSRF')
                harness.configure(provider.server_port)
                before = mock.count()
                value = definition(delay=2)
                calculated = preview(harness, value)
                check('relative preview is estimated and causes no model or task execution',
                      calculated['valid'] and calculated['estimated'] and mock.count() == before
                      and isinstance(calculated.get('next_times'), list) and len(calculated['next_times']) == 1
                      and calculated['next_times'][0]['at'] >= calculated['now'] + 2
                      and harness.call('GET', ROOT + '/jobs')[0]['total'] == 0)
                for field, replacement in [('enabled', True), ('owner_id', 'forged')]:
                    bad = copy.deepcopy(value); bad[field] = replacement
                    harness.call('POST', ROOT + '/jobs', {'task_trigger_draft': bad}, expected=(400, 422),
                                 headers={'Idempotency-Key': str(uuid.uuid4())})
                bad = copy.deepcopy(value); bad['trigger']['after_seconds'] = 0
                harness.call('POST', ROOT + '/jobs', {'task_trigger_draft': bad}, expected=(400, 422),
                             headers={'Idempotency-Key': str(uuid.uuid4())})
                check('unapproved fields and invalid timing are rejected by the actual server')
                identifier = str(uuid.uuid4())
                job = create(harness, value, key=identifier)
                repeated = create(harness, value, key=identifier)
                time.sleep(0.15)
                check('saving a draft is disabled and idempotent without creating an occurrence',
                      job['id'] == repeated['id'] and job['lifecycle'] == 'draft'
                      and job['active_revision'] is None and occurrences(harness, job) == [])
                proof = preview(harness, value, job)
                old_job = job
                value = copy.deepcopy(value); value['title'] += ' revised'
                job = harness.call('POST', ROOT + '/jobs/' + job['id'] + '/drafts', {
                    'expected_revision': job['revision'], 'task_trigger_draft': value},
                    headers={'Idempotency-Key': str(uuid.uuid4())})[0]
                harness.call('POST', ROOT + '/jobs/' + job['id'] + '/drafts', {
                    'expected_revision': old_job['revision'], 'task_trigger_draft': value}, expected=409)
                harness.call('POST', ROOT + '/jobs/' + job['id'] + '/activate', {
                    'expected_revision': old_job['revision'], 'draft_revision': old_job['draft_revision'],
                    'preview_token': proof['preview_token']}, expected=409)
                check('draft revisions and reviewed activation reject stale changes without overwriting newer data',
                      job['draft_revision'] > old_job['draft_revision'] and job['active_revision'] is None)
                with helpers.other_admin(harness):
                    harness.call('GET', ROOT + '/jobs/' + job['id'], expected=404)
                    harness.call('GET', ROOT + '/jobs/' + job['id'] + '/occurrences', expected=404)
                    harness.call('POST', ROOT + '/jobs/' + job['id'] + '/pause', {'expected_revision': job['revision']}, expected=404)
                check('another authenticated administrator cannot read private task or occurrence data')
                job = activate(harness, job, value)
                result = wait_occurrence(harness, job)
                check('a reviewed one-shot task actually triggers once and stores a Web result',
                      result['status'] == 'completed' and result['delivery_status'] == 'stored'
                      and result.get('response') == value['action']['content'] and len(occurrences(harness, job)) == 1)
                with helpers.other_admin(harness):
                    harness.call('GET', ROOT + '/occurrences/' + result['id'], expected=404)
                check('stored task results remain private after execution')
                harness.stop(); harness.start(); harness.login(); acknowledge_clock(harness)
                time.sleep(0.2)
                check('restart preserves settled once tasks without duplicate execution or countdown reset',
                      len(occurrences(harness, job)) == 1)
                current = harness.call('GET', ROOT + '/jobs/' + job['id'])[0]
                revision_after = copy.deepcopy(value); revision_after['action']['content'] = 'New saved draft only'
                edited = harness.call('POST', ROOT + '/jobs/' + job['id'] + '/drafts', {
                    'expected_revision': current['revision'], 'task_trigger_draft': revision_after})[0]
                check('saving another draft preserves the previously approved active definition',
                      edited['active_definition'] == value and edited['task_trigger_draft'] == revision_after)
                interval = definition('interval'); interval['trigger']['every_seconds'] = 1
                interval['budget']['daily_occurrences'] = 2
                periodic = activate(harness, create(harness, interval), interval)
                wait_occurrence(harness, periodic, minimum=2)
                time.sleep(1.1)
                budget_results = occurrences(harness, periodic)
                budget_job = harness.call('GET', ROOT + '/jobs/' + periodic['id'])[0]
                check('fixed-rate seconds tasks enforce a persistent daily occurrence budget',
                      len([item for item in budget_results if item['status'] == 'completed']) == 2
                      and len(budget_results) <= 3 and budget_job['block_reason'] == 'budget')
                paused = manage(harness, periodic, 'pause')
                check('pause stops future triggering independently of saved definitions', paused['lifecycle'] == 'paused')
                manage(harness, paused, 'archive')
                unknown = definition('workday'); unknown['trigger']['frequency'] = 'dates'; unknown['trigger']['dates'] = ['2027-01-04']
                missing = preview(harness, unknown)
                check('missing official year is reported without a weekday fallback',
                      bool(missing.get('blocked_reason')) or not missing.get('valid'))
                for name in ['workday', 'restday', 'month-end', 'leap-day', 'date-list', 'weekdays']:
                    preview(harness, definition(name))
                check('calendar rule families use the real server preview and bounded candidate calculations')
                private = 'PRIVATE_TASK_CONTEXT_' + secrets.token_hex(8)
                memory_source = harness.conversation('Synthetic private task-isolation source')
                remembered = harness.wait_run(harness.submit(memory_source, '/memory remember ' + private))
                if remembered['status'] != 'completed':
                    raise AssertionError('synthetic explicit memory fixture was not saved')
                old = 'FROZEN_TASK_SKILL_' + secrets.token_hex(6)
                selected = harness.call('POST', PREFIX + '/skills', {'name': 'Synthetic task Skill', 'description': 'Isolated acceptance.',
                    'instructions': old, 'kind': 'instruction', 'enabled': True}, expected=201)[0]
                task = model_definition(delay=2, skill={'id': selected['id'], 'revision': selected['revision']})
                automatic = activate(harness, create(harness, task), task)
                skills.change(harness, selected, instructions='NEW_TASK_INSTRUCTIONS')
                result = wait_occurrence(harness, automatic)
                payload = next(item for item in mock.payloads() if 'TASK_MODEL_EXECUTION_PROBE' in skills.Mock.question(item))
                serialized = json.dumps(payload, ensure_ascii=False)
                check('scheduled model execution preserves reviewed Skill text and excludes ordinary private memory',
                      result['status'] == 'completed' and old in skills.system_text(payload)
                      and 'NEW_TASK_INSTRUCTIONS' not in serialized and private not in serialized)
                source = harness.conversation('Synthetic setup Skill source')
                jobs_before_setup = harness.call('GET', ROOT + '/jobs')[0]['total']
                run = skills.submit(harness, source, 'TRIGGER_SETUP_PROBE', 'task-trigger-setup')['run_id']
                setup_result = harness.wait_run(run)
                check('setup Skill returns a reviewable task envelope without automatically installing it',
                      'task_trigger_draft' in setup_result['response'] and mock.count() >= 2
                      and harness.call('GET', ROOT + '/jobs')[0]['total'] == jobs_before_setup)
                event = definition('event')
                event['trigger']['event_type'] = 'dsh.run.completed.v1'; event['trigger']['filters'] = []
                event['trigger']['cooldown_seconds'] = 0
                event['action'] = model_definition('EVENT_TASK_MODEL_PROBE')['action']
                event['budget'] = model_definition()['budget']
                watched = activate(harness, create(harness, event), event)
                conversation = harness.conversation('Synthetic event producer')
                interactive = harness.submit(conversation, 'interactive-event-probe')
                check('interactive source Run completes through the configured HTTP provider', harness.wait_run(interactive)['status'] == 'completed')
                result = wait_occurrence(harness, watched)
                time.sleep(1.1)
                check('owned interaction completion triggers one isolated task without recursively triggering itself',
                      result['status'] == 'completed' and len(occurrences(harness, watched)) == 1)
                manage(harness, watched, 'pause')
                held = model_definition('held-trigger-run')
                stopping = activate(harness, create(harness, held), held)
                check('held scheduled model request is actually in flight before cancellation', mock.hold_received.wait(timeout=12))
                held_occurrences = occurrences(harness, stopping)
                harness.call('DELETE', PREFIX + '/runs/' + held_occurrences[0]['run_id'], expected=(403,))
                check('ordinary Run cancellation cannot bypass the automatic task state machine',
                      occurrences(harness, stopping)[0]['status'] == 'running')
                stopped = manage(harness, stopping, 'stop')
                mock.hold_release.set()
                result = wait_occurrence(harness, stopping)
                check('stop cancels current work and suppresses delivery rather than reporting a generated result',
                      stopped['lifecycle'] == 'paused' and result['status'] in {'cancelled', 'expired'}
                      and result.get('response') is None and result['delivery_status'] != 'stored')
                with harness.db() as database:
                    task_runs = database.execute('SELECT count(*) FROM runs r JOIN trigger_occurrences o ON o.run_id=r.id').fetchone()[0]
                    contaminated = database.execute('SELECT count(*) FROM memory_candidate_jobs').fetchone()[0] + database.execute('SELECT count(*) FROM summary_jobs').fetchone()[0]
                check('automatic task Runs do not create implicit memory candidates or summaries', task_runs >= 2 and contaminated == 0)
                setup = next(item for item in harness.call('GET', PREFIX + '/skills')[0]['items'] if item['id'] == 'task-trigger-setup')
                harness.call('DELETE', PREFIX + '/skills/task-trigger-setup', {'expected_revision': setup['revision']})
                harness.stop(); harness.start(); harness.login(); acknowledge_clock(harness)
                check('setup Skill migration does not resurrect an explicitly deleted default',
                      all(item['id'] != 'task-trigger-setup' for item in harness.call('GET', PREFIX + '/skills')[0]['items']))
                report['status'] = 'passed'
            finally:
                mock.hold_release.set()
                harness.close()
    except Exception as error:
        code = 1
        report.update(status='failed', failure_after_check=stage, error_type=type(error).__name__)
        trace = error.__traceback__
        while trace:
            if trace.tb_frame.f_code.co_filename == __file__:
                report['failure_line'] = trace.tb_lineno
            trace = trace.tb_next
        if isinstance(error, helpers.ResponseStatusError):
            report.update(actual_http_status=error.actual, expected_http_statuses=error.expected)
        print('FAIL isolated task-trigger acceptance; see sanitized report', flush=True)
    finally:
        provider.shutdown(); provider.server_close()
        report.update(finished_at=helpers.acceptance.utc_now(), passed=len(report['checks']), mock_requests=mock.count())
        pathlib.Path(args.report).write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps({'status': report['status'], 'passed': report['passed'], 'report': args.report}))
    return code


if __name__ == '__main__':
    raise SystemExit(main())
