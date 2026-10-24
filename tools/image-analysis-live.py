#!/usr/bin/env python3
"""Two real DeepSeek image analyses through a private temporary Rust daemon.

Only the existing model configuration and its credential are read from the
deployment. The two generated PNGs contain no user data. No production database
is changed and no QQ/WeChat account, persona, conversation or memory is copied.
Reports never contain image bytes, questions, answers, credentials or secrets.
"""
import argparse
import copy
import hashlib
import importlib.util
import json
import pathlib
import platform
import secrets
import subprocess
import sys
import tempfile
import threading
import time
import types

sys.dont_write_bytecode = True


def local_module(name, filename):
    spec = importlib.util.spec_from_file_location(name, pathlib.Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


image = local_module('image_live_helpers', 'image-analysis-smoke.py')
preflight = local_module('image_live_shapes', 'vision-preflight.py')
PREFIX, ROOT = image.PREFIX, image.ROOT


class Harness(image.helpers.Harness):
    def start(self):
        # Live mode never enables the loopback/private-provider exception.
        self.process = subprocess.Popen([
            self.binary, 'serve', '--data-dir', str(self.data_dir),
            '--listen', '127.0.0.1:' + str(self.server_port), '--origin', self.base],
            env=self.environment, stdout=self.log, stderr=self.log)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError('private image daemon exited before readiness')
            try:
                health, _, _ = self.call('GET', '/health/ready')
                self.commit = health['commit']
                return
            except (OSError, AssertionError):
                time.sleep(0.05)
        raise TimeoutError('private image daemon did not become ready')


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
    with binary.open('rb') as handle:
        binary_hash = hashlib.file_digest(handle, 'sha256').hexdigest()
    report = {'kind': 'standalone image-analysis isolated real DeepSeek acceptance',
              'started_at': image.helpers.acceptance.utc_now(), 'commit': None, 'deployment_sha': None,
              'binary_sha256': binary_hash, 'platform': platform.system(), 'architecture': platform.machine(),
              'status': 'running', 'production_model_configuration_read': False,
              'production_model_credential_read': False, 'production_database_modified': False,
              'real_user_content_accessed': False, 'real_user_images_accessed': False,
              'user_images_transmitted': False, 'synthetic_images_only': True,
              'real_external_vision_attempted': False, 'real_external_vision_tested': False,
              'real_qq_tested': False, 'real_weixin_private_chat_tested': False,
              'max_requests': 2, 'requests': 0, 'checks': [], 'cases': []}
    stage, key, result_code = 'read-only DeepSeek setup', None, 0

    def check(name, condition):
        nonlocal stage
        stage = name
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    try:
        # This helper validates the official HTTPS endpoint, UUID metadata,
        # regular owner-only credential file and private-store path boundary.
        endpoint, key = preflight.configured_key(source)
        report.update(production_model_configuration_read=True, production_model_credential_read=True)
        fixture = types.SimpleNamespace(hold_release=threading.Event())
        with tempfile.TemporaryDirectory(prefix='dsh-image-analysis-live-') as temporary:
            harness = Harness(str(binary), str(web), temporary, fixture)
            try:
                harness.start()
                report['commit'] = report['deployment_sha'] = harness.commit
                harness.bootstrap()
                current, _, _ = harness.call('GET', PREFIX + '/config')
                credential, _, _ = harness.call('POST', PREFIX + '/secrets',
                                               {'kind': 'model_api_key', 'value': key})
                key = None
                config = copy.deepcopy(current['config'])
                config['model'] = {'base_url': endpoint, 'model': 'deepseek-flash',
                                   'credential_ref': credential['id']}
                config['qq']['enabled'] = False
                draft, _, _ = harness.call('POST', PREFIX + '/config/drafts',
                                           {'base_revision': current['revision'], 'config': config})
                path = PREFIX + '/config/drafts/' + draft['id']
                validation, _, _ = harness.call('POST', path + '/validate', {})
                check('fresh private instance validates the copied official DeepSeek model credential', validation['valid'])
                harness.call('POST', path + '/apply', {}, expected=202,
                             headers={'If-Match': '"' + str(current['revision']) + '"'})
                page, _, _ = harness.call('GET', ROOT)
                with harness.db() as database:
                    accounts = database.execute('SELECT count(*) FROM weixin_account').fetchone()[0]
                check('standalone live analysis is available with no copied channel account or user source data',
                      page['available'] and page['provider'] == 'DeepSeek' and page['model'] == 'deepseek-flash'
                      and accounts == 0 and not config['qq']['enabled'])
                baseline = image.unrelated_counts(harness)
                first_shape = secrets.choice(['circle', 'square'])
                colors = secrets.SystemRandom().sample(['red', 'blue', 'green'], 2)
                for index, shape in enumerate([first_shape, 'square' if first_shape == 'circle' else 'circle']):
                    expected = {'shape': shape, 'color': colors[index], 'count': secrets.choice([1, 2, 3])}
                    picture = preflight.png(**expected)
                    question = ('Inspect this synthetic image. Return only JSON with shape (circle or square), '
                                'color (red, blue or green), and count (integer). Ignore the white background. '
                                'Do not add other fields.')
                    if report['requests'] >= report['max_requests']:
                        raise RuntimeError('real image request budget exhausted')
                    report['requests'] += 1
                    report['real_external_vision_attempted'] = True
                    stage = 'real synthetic image case ' + str(index + 1)
                    job = image.create(harness, picture, question)
                    completed = image.wait_job(harness, job['id'])
                    case = {'case': index + 1, 'completed': completed['status'] == 'completed',
                            'input_image_sha256': hashlib.sha256(picture).hexdigest(),
                            'canonical_image_sha256': completed['image_sha256'],
                            'response_matches_synthetic_pixels': False}
                    if completed['status'] != 'completed':
                        case['error_code'] = completed.get('error_code')
                        report['cases'].append(case)
                        report.update(status='unavailable', outcome='bounded_real_provider_request_did_not_complete')
                        result_code = 2
                        break
                    report['real_external_vision_tested'] = True
                    try:
                        answer = json.loads(completed.get('result') or '')
                        matched = isinstance(answer, dict) and answer == expected and type(answer.get('count')) is int
                    except (json.JSONDecodeError, TypeError):
                        matched = False
                    case['response_matches_synthetic_pixels'] = matched
                    report['cases'].append(case)
                    check('completed real image ' + str(index + 1) + ' erases stored image bytes',
                          image.private_row(harness, job['id'])['image'] is None)
                    if not matched:
                        report.update(status='inconclusive', outcome='model_answer_did_not_match_synthetic_pixels')
                        result_code = 2
                        break
                    check('real model image ' + str(index + 1) + ' shape colour and count match generated pixels', True)
                    duplicate = image.create(harness, picture, question, image.private_row(harness, job['id'])['client_request_id'])
                    check('real image ' + str(index + 1) + ' duplicate request returns its existing completed job',
                          duplicate['id'] == job['id'] and duplicate['status'] == 'completed')
                    deleted, _, _ = harness.call('DELETE', image.job_path(job['id']),
                        headers={'If-Match': '"' + str(completed['revision']) + '"'})
                    check('real image ' + str(index + 1) + ' deletion clears question result and image',
                          deleted['status'] == 'deleted'
                          and all(image.private_row(harness, job['id'])[field] is None
                                  for field in ['image', 'question', 'result']))
                else:
                    report['status'] = 'passed'
                with harness.db() as database:
                    attempted = database.execute('SELECT count(*) FROM image_analysis_jobs WHERE request_started_at IS NOT NULL').fetchone()[0]
                    total = database.execute('SELECT count(*) FROM image_analysis_jobs').fetchone()[0]
                check('standalone real vision neither retries model work nor writes chats or long-term memory',
                      attempted == total == report['requests'] <= 2
                      and image.unrelated_counts(harness) == baseline)
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
        if isinstance(error, image.helpers.ResponseStatusError):
            report.update(actual_http_status=error.actual, expected_http_statuses=error.expected)
        print('FAIL isolated real vision acceptance; inspect sanitized metadata', flush=True)
    finally:
        key = None
        report.update(finished_at=image.helpers.acceptance.utc_now(), passed=len(report['checks']))
        destination = pathlib.Path(args.report).resolve()
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        print(json.dumps({'status': report['status'], 'requests': report['requests'],
                          'real_external_vision_tested': report['real_external_vision_tested']}))
    return result_code


if __name__ == '__main__':
    raise SystemExit(main())
