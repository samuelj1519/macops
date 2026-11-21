import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('chezmoi_model_r3', ROOT / 'tools/verify_chezmoi_model.py')
MODEL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODEL)


def state(number, action, phase, runs):
    return f'State {number}: <{action} line 1>\n/\\ phase = "{phase}"\n/\\ runs = {runs}\n\n'


class NativeModelChecks(unittest.TestCase):
    def test_replacement_requires_exactly_one_match(self):
        self.assertEqual(MODEL.replace_once('abc', 'b', 'x'), 'axc')
        for source in ['ac', 'abbc']:
            with self.assertRaises(ValueError):
                MODEL.replace_once(source, 'b', 'x')

    def test_action_and_state_violations_have_distinct_classifications(self):
        self.assertEqual(MODEL.violation('Error: Action property NoRepeatRunAction is violated.\n'), ('Action property', 'NoRepeatRunAction'))
        self.assertEqual(MODEL.violation('Error: Invariant AtMostOnce is violated.\n'), ('Invariant', 'AtMostOnce'))
        for invalid in ['Temporal properties were violated', 'java.net.SocketException', 'Action property NoRepeatRunAction is violated.', 'Error: Invariant Other is violated without a trace']:
            self.assertIsNone(MODEL.violation(invalid))

    def test_real_replay_counts_actions_independently_of_counter(self):
        prefix = state(1, 'Initial', 'planned', 0) + state(2, 'RunOnce', 'running', 1) + state(3, 'Timeout', 'unknown', 1)
        cumulative = MODEL.literal_trace(prefix + state(4, 'RunOnce', 'running', 2))
        saturated = MODEL.literal_trace(prefix + state(4, 'RunOnce', 'running', 1))
        self.assertTrue(MODEL.replay_matches(cumulative, 2))
        self.assertTrue(MODEL.replay_matches(saturated, 1))
        self.assertFalse(MODEL.replay_matches(saturated, 2))
        self.assertFalse(MODEL.replay_matches(cumulative[:3], 2))
        forged = MODEL.literal_trace(prefix + state(4, 'RecoverQuery', 'unknown', 2))
        self.assertFalse(MODEL.replay_matches(forged, 2))

    def test_witness_oracle_rejects_missing_and_wrong_literal_state(self):
        self.assertFalse(MODEL.witness_matches('NoPreview', []))
        self.assertFalse(MODEL.witness_matches('NoPreview', MODEL.literal_trace(state(1, 'Initial', 'planned', 0))))
        output = state(1, 'Preview', 'previewed', 0) + '/\\ dry = TRUE\n/\\ queryRan = FALSE\n'
        self.assertTrue(MODEL.witness_matches('NoPreview', MODEL.literal_trace(output)))
        self.assertFalse(MODEL.witness_matches('NoPreview', MODEL.literal_trace(output.replace('runs = 0', 'runs = 1'))))

    def test_formal_candidate_checks_include_real_guard_and_counter_regressions(self):
        source = ROOT / 'docs/models/chezmoi_native/ChezmoiNative.tla'
        cases = MODEL.checks(source.read_text(), source.with_suffix('.cfg').read_text())
        by_name = {item[0]: item for item in cases}
        self.assertEqual(len(cases), 17)
        self.assertIn('/\\ runs \\in Nat', by_name['baseline'][1])
        self.assertIn('NoRepeatRunAction', by_name['baseline'][2])
        self.assertIn('RunsNeverDecrease', by_name['baseline'][2])
        guard = by_name['guard_only_replay']
        self.assertIn('phase \\in {"prepared", "unknown"}', guard[1])
        self.assertIn("runs' = runs + 1", guard[1])
        self.assertEqual(guard[3], ('Invariant', 'AtMostOnce'))
        self.assertEqual(by_name['guard_replay_action_oracle'][3], ('Action property', 'NoRepeatRunAction'))
        self.assertEqual(by_name['saturated_counter_real_replay'][3], ('Action property', 'NoRepeatRunAction'))
        self.assertEqual(by_name['reset_counter'][3], ('Action property', 'RunsNeverDecrease'))
        self.assertNotIn('TypeOK', [item[3][1] for item in cases if item[3] is not None])


if __name__ == '__main__':
    unittest.main()
