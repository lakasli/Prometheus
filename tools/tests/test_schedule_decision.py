import importlib.util
import pathlib
import unittest

spec=importlib.util.spec_from_file_location('decision',pathlib.Path(__file__).resolve().parents[1]/'actions/schedule_decision.py')
decision=importlib.util.module_from_spec(spec)
spec.loader.exec_module(decision)

class DecisionTests(unittest.TestCase):
    def test_only_current_bounded_choices_have_script_capability(self):
        self.assertEqual(decision.normalize({'decision':'execute'}),{'decision':'execute','delay_minutes':0})
        self.assertEqual(decision.normalize({'decision':'postpone','delay_minutes':30})['delay_minutes'],30)
        for value in ({'decision':'postpone','delay_minutes':True},{'decision':'postpone','delay_minutes':31},{'decision':'execute','delay_minutes':5},{'decision':'execute','url':'https://example.com'},{'decision':'shell'},[]):
            with self.subTest(value=value), self.assertRaises(ValueError):
                decision.normalize(value)
