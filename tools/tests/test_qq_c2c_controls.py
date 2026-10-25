import importlib.util
import pathlib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('qq_control_probe_tests', ROOT / 'qq-c2c-controls-live.py')
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class StreamAndOwnershipTests(unittest.TestCase):
    def test_local_validation_rejection_is_distinct_from_uncertain_network_result(self):
        self.assertTrue(MODULE.is_known_local_rejection(MODULE.ControlsApiFailure(400, 'qq_parameters')))
        self.assertFalse(MODULE.is_known_local_rejection(MODULE.ControlsApiFailure(500, 'internal_error')))
        self.assertFalse(MODULE.is_known_local_rejection(TimeoutError()))
    def test_one_stream_reuses_seq4_and_increments_only_the_index(self):
        first = MODULE.stream_fragment('synthetic-source', 0, 'prefix')
        last = MODULE.stream_fragment('synthetic-source', 1, 'prefix end', 'new-stream-id')
        self.assertEqual(first['msg_seq'], 4)
        self.assertEqual(last['msg_seq'], 4)
        self.assertEqual((first['index'], last['index']), (0, 1))
        self.assertEqual((first['input_state'], last['input_state']), (1, 10))
        self.assertNotIn('stream_msg_id', first)
        self.assertEqual(last['stream_msg_id'], 'new-stream-id')
        self.assertTrue(last['content_raw'].startswith(first['content_raw']))

    def test_stream_cannot_skip_the_first_fragment_or_reuse_an_existing_id(self):
        for args in [('source', 0, 'prefix', 'some-existing-id'), ('source', 1, 'prefix end'), ('source', 2, 'extra', 'own-id')]:
            with self.assertRaises(ValueError):
                MODULE.stream_fragment(*args)

    def test_retraction_requires_a_new_owned_markdown_or_stream_id(self):
        journal = {'account_sha256': 'account-a', 'messages': {'markdown': {'id': 'our-fresh-id', 'seq': 3, 'created_by_this_invocation': True}}}
        self.assertTrue(MODULE.retractable(journal, 'markdown', 'our-fresh-id', 'account-a'))
        self.assertFalse(MODULE.retractable(journal, 'markdown', 'somebody-elses-id', 'account-a'))
        self.assertFalse(MODULE.retractable(journal, 'markdown', 'our-fresh-id', 'account-b'))
        self.assertFalse(MODULE.retractable(journal, 'native', 'our-fresh-id', 'account-a'))
        journal['messages']['markdown']['seq'] = 1
        self.assertFalse(MODULE.retractable(journal, 'markdown', 'our-fresh-id', 'account-a'))

    def test_unknown_or_completed_retraction_is_not_repeated(self):
        journal = {'account_sha256': 'account-a', 'messages': {'stream': {'id': 'our-stream', 'seq': 4, 'created_by_this_invocation': True, 'retract_intent_recorded': True}}}
        self.assertFalse(MODULE.retractable(journal, 'stream', 'our-stream', 'account-a'))


if __name__ == '__main__':
    unittest.main()
