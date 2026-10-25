import importlib.util
import pathlib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]


def module(name, file):
    spec = importlib.util.spec_from_file_location(name, ROOT / file)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


PROBE = module('qq_discovery_tests', 'qq-api-live.py')
RESOURCE = module('qq_resource_scope_tests', 'qq-resource-live.py')


class DiscoveryTests(unittest.TestCase):
    def test_platform_omitted_empty_panel_records_require_terminal_page_evidence(self):
        self.assertEqual(PROBE.panel_records({'is_end': True}), [])
        self.assertEqual(PROBE.panel_records({'is_end': True, 'records': None}), [])
        for malformed in [{}, {'is_end': False}, {'is_end': True, 'next_cursor': 'next'}, {'is_end': True, 'records': 'not-a-list'}]:
            with self.assertRaises(ValueError):
                PROBE.panel_records(malformed)

    def test_actual_empty_panel_response_records_a_count_of_zero(self):
        resources, counts = {}, {}
        PROBE.discover_resources('get_v2_panels', {'is_end': True}, resources, counts)
        self.assertEqual(counts['c2c_panels_returned_on_first_page'], 0)
        self.assertNotIn('panel_id', resources)
    def test_resource_discovery_precedes_dependent_reads(self):
        items = [
            {'id': 'get_channels_channel_id_threads_thread_id', 'read_only': True, 'parameters': ['channel_id', 'thread_id']},
            {'id': 'get_guilds_guild_id_channels', 'read_only': True, 'parameters': ['guild_id']},
            {'id': 'get_users_me_guilds', 'read_only': True, 'parameters': []},
            {'id': 'get_guilds_guild_id', 'read_only': True, 'parameters': ['guild_id']},
            {'id': 'get_channels_channel_id_threads', 'read_only': True, 'parameters': ['channel_id']},
        ]
        ids = [item['id'] for item in PROBE.ordered_operations(items)]
        self.assertLess(ids.index('get_users_me_guilds'), ids.index('get_guilds_guild_id_channels'))
        self.assertLess(ids.index('get_guilds_guild_id_channels'), ids.index('get_channels_channel_id_threads'))
        self.assertLess(ids.index('get_channels_channel_id_threads'), ids.index('get_channels_channel_id_threads_thread_id'))

    def test_discovery_counts_do_not_include_resource_ids(self):
        resources, counts = {}, {}
        PROBE.discover_resources('get_users_me_guilds', [{'id': 'private-guild'}], resources, counts)
        PROBE.discover_resources('get_guilds_guild_id_channels', [{'id': 'private-audio', 'type': 2}, {'id': 'private-text', 'type': 0}], resources, counts)
        self.assertEqual(resources['guild_id'], 'private-guild')
        self.assertEqual(counts['guilds_returned_on_first_page'], 1)
        self.assertNotIn('private-', str(counts))

    def test_audio_and_forum_calls_do_not_substitute_a_text_channel(self):
        resources = {'channel_id': 'text-only'}
        op = {'parameters': ['channel_id'], 'path': '/channels/{channel_id}/online_nums'}
        parameters, missing = PROBE.parameters_for(op, resources, set())
        self.assertIsNone(parameters)
        self.assertEqual(missing, ['audio_channel_id'])
        resources['_audio_channel_id'] = 'audio-channel'
        parameters, missing = PROBE.parameters_for(op, resources, set())
        self.assertEqual(parameters['channel_id'], 'audio-channel')
        self.assertEqual(missing, [])

    def test_empty_discovery_is_a_count_not_a_permission_guess(self):
        resources, counts = {}, {}
        PROBE.discover_resources('get_users_me_guilds', [], resources, counts)
        self.assertEqual(counts['guilds_returned_on_first_page'], 0)
        self.assertNotIn('guild_id', resources)


class OwnershipTests(unittest.TestCase):
    def test_fresh_panel_is_specific_and_only_the_explicit_approved_user_is_bound(self):
        self.assertEqual(RESOURCE.panel_request('marker')['user_openids'], [])
        body = RESOURCE.panel_request('marker', 'only-approved-test-user')
        self.assertEqual(body['scope'], 'c2c')
        self.assertEqual(body['target_type'], 'specific')
        self.assertEqual(body['user_openids'], ['only-approved-test-user'])
        self.assertNotIn('group_openids', body)
    def test_cleanup_rejects_existing_ids_and_other_accounts_or_runs(self):
        journal = {'account_sha256': 'account-a', 'panel': {'id': 'fresh-id', 'created_by_this_invocation': True}}
        self.assertTrue(RESOURCE.owned_resource('panel', 'fresh-id', {'old-id'}, journal, 'account-a'))
        self.assertFalse(RESOURCE.owned_resource('panel', 'old-id', {'old-id'}, journal, 'account-a'))
        self.assertFalse(RESOURCE.owned_resource('panel', 'fresh-id', set(), journal, 'account-b'))
        self.assertFalse(RESOURCE.owned_resource('panel', 'other-run-id', set(), journal, 'account-a'))

    def test_an_uncertain_delete_is_never_automatically_submitted_again(self):
        journal = {'panel': {'id': 'new-panel', 'created_by_this_invocation': True}}
        self.assertTrue(RESOURCE.cleanup_needed(journal, 'panel'))
        journal['panel']['delete_intent_recorded'] = True
        self.assertFalse(RESOURCE.cleanup_needed(journal, 'panel'))


if __name__ == '__main__':
    unittest.main()
