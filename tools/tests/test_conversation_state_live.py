"""Offline selection/provenance regression; no daemon, key or network access."""
import contextlib
import hashlib
import importlib.util
import io
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True
SCRIPT = pathlib.Path(__file__).resolve().parents[1] / 'conversation-state-live.py'
SPEC = importlib.util.spec_from_file_location('conversation_state_live_selection_test', SCRIPT)
live = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(live)
PROGRAM_SHA = '1' * 40
SCRIPT_SHA = '2' * 40


class SelectionTests(unittest.TestCase):
    def test_default_preserves_all_twelve_and_24_planned_requests(self):
        selected = live.select_scenarios(None, 24)
        self.assertEqual(selected, live.SCENARIOS)
        self.assertEqual(len(selected), 12)
        self.assertEqual(sum(item[1] for item in selected), 24)

    def test_default_lower_limit_still_supports_original_budget_limited_mode(self):
        self.assertEqual(live.select_scenarios(None, 1), live.SCENARIOS)

    def test_targeted_selection_is_stable_and_does_not_fill_the_spare_request(self):
        for raw in ['S05,GROUP', 'GROUP,S05', ' S05 , GROUP ']:
            selected = live.select_scenarios(raw, 6)
            self.assertEqual([case[0] for case in selected], ['GROUP', 'S05'])
            self.assertEqual([case[1] for case in selected], [3, 2])
            self.assertEqual(sum(case[1] for case in selected), 5)
        self.assertEqual(live.select_scenarios('S05,GROUP', 5), selected)

    def test_empty_unknown_and_duplicate_selection_are_rejected(self):
        for raw in ['', ' ', ',S05', 'GROUP,', 'S05,,GROUP', 'S05,S05',
                    'GROUP, GROUP', 's05', 'UNKNOWN', 'S01,UNKNOWN']:
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                live.select_scenarios(raw, 24)

    def test_invalid_limits_and_explicit_cohort_over_budget_are_rejected(self):
        for raw, limit in [(None, 0), (None, 25), (None, -1), (None, True),
                           ('GROUP,S05', 4), ('S05', 1)]:
            with self.subTest(raw=raw, limit=limit), self.assertRaises(ValueError):
                live.select_scenarios(raw, limit)

    def assert_early_rejected(self, options, label):
        argv = ['conversation-state-live.py', '--binary', 'must-not-open',
                '--web-dir', 'must-not-open', '--source-data-dir', 'must-not-open',
                '--expected-commit', PROGRAM_SHA, '--report', 'must-not-create', *options]
        with mock.patch.object(sys, 'argv', argv), \
                mock.patch.object(live.sqlite3, 'connect') as connect, \
                mock.patch.object(live, 'script_source_metadata') as provenance, \
                mock.patch.object(live.pathlib.Path, 'resolve',
                                  side_effect=AssertionError('invalid_arguments_touched_paths')), \
                contextlib.redirect_stderr(io.StringIO()) as stderr:
            with self.assertRaises(SystemExit) as failure:
                live.main()
            self.assertEqual(failure.exception.code, 2)
            self.assertIn(label, stderr.getvalue())
            connect.assert_not_called()
            provenance.assert_not_called()

    def test_cli_invalid_selection_is_rejected_before_any_source_path_or_git_read(self):
        for raw, label in [('', 'must_not_be_empty'), ('S05,S05', 'must_not_repeat'),
                           ('UNKNOWN', 'unknown_ID')]:
            with self.subTest(raw=raw):
                self.assert_early_rejected(['--cases', raw], label)

    def test_cli_targeted_limit_is_rejected_before_source_or_credential_read(self):
        self.assert_early_rejected(['--cases', 'GROUP,S05', '--max-real-requests', '4'],
                                   'selected_planned_requests_exceed_hard_limit')
        self.assert_early_rejected(['--max-real-requests', '25'], 'must_be_1_to_24')

    def test_cli_script_sha_format_is_rejected_before_source_read(self):
        self.assert_early_rejected(['--expected-script-commit', 'short'],
                                   'full lowercase commit SHA')

    def test_script_commit_failure_cannot_open_production_or_start_transport(self):
        argv = ['conversation-state-live.py', '--binary', 'must-not-open',
                '--web-dir', 'must-not-open', '--source-data-dir', 'must-not-open',
                '--expected-commit', PROGRAM_SHA, '--report', 'must-not-create',
                '--cases', 'GROUP,S05', '--max-real-requests', '6',
                '--expected-script-commit', SCRIPT_SHA]
        with mock.patch.object(sys, 'argv', argv), \
                mock.patch.object(live, 'script_source_metadata',
                                  side_effect=ValueError('script_source_worktree_is_not_clean')), \
                mock.patch.object(live.sqlite3, 'connect') as connect, \
                mock.patch.object(live, 'ForwardState') as forward, \
                contextlib.redirect_stderr(io.StringIO()) as stderr:
            with self.assertRaises(SystemExit) as failure:
                live.main()
            self.assertEqual(failure.exception.code, 2)
            self.assertIn('script_source_worktree_is_not_clean', stderr.getvalue())
            connect.assert_not_called()
            forward.assert_not_called()

    def test_targeted_report_separates_script_sha_program_sha_and_planned_limit(self):
        with tempfile.TemporaryDirectory(prefix='dsh-live-offline-selection-') as temporary:
            root = pathlib.Path(temporary)
            binary = root / 'synthetic-not-executable'
            binary.write_bytes(b'offline synthetic fixture only')
            source = root / 'empty-source'
            source.mkdir()
            output = root / 'offline-report.json'
            argv = ['conversation-state-live.py', '--binary', str(binary), '--web-dir', str(root),
                    '--source-data-dir', str(source), '--expected-commit', PROGRAM_SHA,
                    '--report', str(output), '--cases', 'S05,GROUP', '--max-real-requests', '6',
                    '--expected-script-commit', SCRIPT_SHA]
            with mock.patch.object(sys, 'argv', argv), \
                    mock.patch.object(live, 'script_source_metadata', return_value={
                        'commit': SCRIPT_SHA, 'clean': True, 'verification': 'exact_expected_clean_commit'}), \
                    mock.patch.object(live.sqlite3, 'connect',
                                      side_effect=live.sqlite3.OperationalError('synthetic_source_unavailable')), \
                    mock.patch.object(live, 'ForwardState') as forward, \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(live.main(), 1)  # deliberate offline source failure
                forward.assert_not_called()
            data = json.loads(output.read_text(encoding='utf-8'))
            self.assertEqual(data['status'], 'setup_or_safety_failed')
            self.assertEqual(data['selected_case_ids'], ['GROUP', 'S05'])
            self.assertEqual(data['planned_requests'], 5)
            self.assertEqual(data['request_budget']['limit'], 6)
            self.assertEqual(data['request_budget']['reserved'], 0)
            self.assertEqual(data['case_selection']['requested_ids'], ['S05', 'GROUP'])
            self.assertEqual(data['case_selection']['execution_order'], 'stable_SCENARIOS_manifest_order')
            self.assertEqual(len(data['case_selection']['excluded_case_ids']), 10)
            self.assertEqual(data['program_expected_commit'], PROGRAM_SHA)
            self.assertEqual(data['script_source_commit'], SCRIPT_SHA)
            self.assertEqual(data['expected_script_commit'], SCRIPT_SHA)
            self.assertTrue(data['script_source_clean'])
            self.assertEqual(data['runner_sha256'], hashlib.sha256(SCRIPT.read_bytes()).hexdigest())
            self.assertFalse(data['production_model_configuration_read'])
            self.assertFalse(data['production_model_credential_read'])
            self.assertEqual(data['cases'], [])


class ScriptSourceTests(unittest.TestCase):
    @staticmethod
    def git_results(commit=SCRIPT_SHA, dirty=''):
        return [subprocess.CompletedProcess([], 0, SCRIPT.name + '\n', ''),
                subprocess.CompletedProcess([], 0, commit + '\n', ''),
                subprocess.CompletedProcess([], 0, dirty, '')]

    def test_expected_script_commit_requires_matching_clean_checkout(self):
        with mock.patch.object(live.subprocess, 'run', side_effect=self.git_results()) as git:
            result = live.script_source_metadata(SCRIPT_SHA, SCRIPT)
            self.assertEqual(result, {'commit': SCRIPT_SHA, 'clean': True,
                                      'verification': 'exact_expected_clean_commit'})
            self.assertEqual(git.call_count, 3)
            self.assertTrue(all(call.kwargs['timeout'] == 10 for call in git.call_args_list))

    def test_expected_wrong_or_dirty_script_is_not_accepted(self):
        for values, message in [(self.git_results(commit=PROGRAM_SHA), 'does_not_match_expected'),
                                (self.git_results(dirty='?? untracked-validation.py\n'), 'is_not_clean')]:
            with self.subTest(message=message), \
                    mock.patch.object(live.subprocess, 'run', side_effect=values), \
                    self.assertRaisesRegex(ValueError, message):
                live.script_source_metadata(SCRIPT_SHA, SCRIPT)

    def test_unavailable_git_fails_formal_verification_without_inventing_a_commit(self):
        with mock.patch.object(live.subprocess, 'run', side_effect=FileNotFoundError):
            with self.assertRaisesRegex(ValueError, 'commit_unavailable'):
                live.script_source_metadata(SCRIPT_SHA, SCRIPT)
            self.assertEqual(live.script_source_metadata(None, SCRIPT),
                             {'commit': None, 'clean': None,
                              'verification': 'runner_hash_only_git_unavailable'})

    def test_untracked_script_inside_a_git_directory_is_not_commit_provenance(self):
        with mock.patch.object(live.subprocess, 'run', side_effect=[
                subprocess.CompletedProcess([], 0, '', '')]):
            with self.assertRaisesRegex(ValueError, 'commit_unavailable'):
                live.script_source_metadata(SCRIPT_SHA, SCRIPT)

    def test_original_default_can_record_dirty_git_without_claiming_formal_verification(self):
        with mock.patch.object(live.subprocess, 'run', side_effect=self.git_results(dirty=' M tools/script.py\n')):
            result = live.script_source_metadata(None, SCRIPT)
            self.assertFalse(result['clean'])
            self.assertEqual(result['verification'], 'observed_git_commit_only')


if __name__ == '__main__':
    unittest.main()
