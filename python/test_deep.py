"""deep checks reject absent subprocess coverage and unbounded mutation selection."""

import json
from pathlib import Path
import tempfile
import unittest

from check_coverage import summary
from ci_journey import ROOT
from run_mutants import arguments, mutation_run, require_selection, selection, source_file


class CoverageTests(unittest.TestCase):
    def document(self, root, covered):
        totals = {name: {'count': 8, 'covered': 4, 'percent': 50.0}
                  for name in ['lines', 'functions', 'regions']}
        return {'type': 'llvm.coverage.json.export', 'data': [{
            'totals': totals,
            'files': [{'filename': str(root / name), 'summary': {'functions': {'covered': covered}}}
                      for name in ['src/main.rs', 'src/lib.rs']],
        }]}

    def test_child_executable_must_contribute_coverage(self):
        root = Path.cwd()
        for count in [0, 1]:
            with self.subTest(covered=count):
                document = self.document(root, count)
                if count:
                    self.assertIn('50.00%', summary(document, root))
                else:
                    with self.assertRaises(AssertionError):
                        summary(document, root)
        document['data'][0]['files'][0]['filename'] = str(root / 'other/src/main.rs')
        with self.assertRaises(AssertionError):
            summary(document, root)

    def test_a_binary_that_linked_a_plain_library_fails(self):
        # run 43177722472's export, cut to its src/main.rs and src/lib.rs records: the binary ran,
        # but around a library built without instrumentation
        fixture = ROOT / 'tests/fixtures/coverage/plain-library.json'
        document = json.loads(fixture.read_text(encoding='utf-8'))
        with self.assertRaisesRegex(AssertionError, 'src/lib.rs'):
            summary(document, Path('/home/runner/work/fetchloom/fetchloom'))


class MutationTests(unittest.TestCase):
    def test_selection_is_exact_nonempty_and_capped(self):
        file = 'src/place.rs'
        names = [f'{file}:{number}: replace place -> bool with false' for number in range(12)]
        chosen, shard = selection(file, '\n'.join(names))
        self.assertEqual(chosen, names[:6])
        self.assertEqual(shard, '0/2')
        for text in ['', 'src/other.rs:1: replace function',
                     'src/place.rs:1: replace \x1b[33mfunction\x1b[0m with ()']:
            with self.subTest(text=text), self.assertRaises(AssertionError):
                selection(file, text)
        for count in [1, 10, 11, 99, 101]:
            chosen, shard = selection(file, '\n'.join(f'{file}:{n}: change' for n in range(count)))
            self.assertGreater(len(chosen), 0)
            self.assertLessEqual(len(chosen), 10)

    def test_tool_preview_and_executed_mutants_must_match_the_selection(self):
        expected = ['src/place.rs:1: replace function', 'src/place.rs:2: replace condition']
        require_selection(expected, list(reversed(expected)))
        for actual in [expected[:1], expected + ['src/place.rs:3: delete field'], expected + expected[:1]]:
            with self.subTest(actual=actual), self.assertRaises(AssertionError):
                require_selection(expected, actual)

    def test_lib_tests_judge_each_mutant_unless_every_target_is_asked_for(self):
        self.assertEqual(arguments(['src/place.rs']).tests, 'lib')
        for tests, wanted, unwanted in [('lib', '--lib', '--all-targets'), ('all', '--all-targets', '--lib')]:
            with self.subTest(tests=tests):
                command = mutation_run(['cargo', 'mutants'], tests, Path('out'))
                cargo = command[command.index('--') + 1:]
                self.assertIn(wanted, cargo)
                self.assertNotIn(unwanted, cargo)

    def test_each_mutant_is_timed_against_the_measured_baseline(self):
        command = mutation_run(['cargo', 'mutants'], 'lib', Path('out'))
        tool = command[:command.index('--')]
        # a fixed --timeout also caps the unmutated baseline, which then tests nothing
        self.assertNotIn('--timeout', tool)
        self.assertEqual(tool[tool.index('--timeout-multiplier') + 1], '3')

    def test_file_scope_stays_within_rust_source(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'src').mkdir()
            (root / 'src/place.rs').write_text('fn place() {}', encoding='utf-8')
            (root / 'Cargo.toml').write_text('', encoding='utf-8')
            self.assertEqual(source_file(root, 'src/place.rs'), 'src/place.rs')
            for path in ['src/missing.rs', 'Cargo.toml', 'src/*.rs', '../outside.rs']:
                with self.subTest(path=path), self.assertRaises(AssertionError):
                    source_file(root, path)
