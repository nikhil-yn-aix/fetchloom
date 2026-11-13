"""deep checks reject absent subprocess coverage and unbounded mutation selection."""

import contextlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import check_coverage
from check_coverage import summary
from ci_journey import ROOT
import run_mutants
from run_mutants import arguments, mutation_run, require_selection, selection, source_file


def coverage(root, main=1, lib=1):
    """an LLVM export whose src/main.rs and src/lib.rs under ``root`` ran ``main`` and ``lib``
    functions."""
    totals = {'lines': {'count': 8, 'covered': 4, 'percent': 50.0},
              'functions': {'count': 3, 'covered': 1, 'percent': 100 / 3},
              'regions': {'count': 3, 'covered': 2, 'percent': 200 / 3}}
    return {'type': 'llvm.coverage.json.export', 'data': [{
        'totals': totals,
        'files': [{'filename': str(root / name), 'summary': {'functions': {'covered': covered}}}
                  for name, covered in [('src/main.rs', main), ('src/lib.rs', lib)]],
    }]}


class CoverageTests(unittest.TestCase):
    def test_the_summary_tables_the_totals(self):
        root = Path.cwd()
        self.assertEqual(summary(coverage(root), root).splitlines(), [
            '## linux coverage', '', 'CLI subprocess profiles include src/main.rs and src/lib.rs.', '',
            '| measure | covered | total | percent |', '|---|---:|---:|---:|',
            '| lines | 4 | 8 | 50.00% |', '| functions | 1 | 3 | 33.33% |', '| regions | 2 | 3 | 66.67% |',
            '', 'coverage identifies gaps; no percentage threshold is enforced.',
        ])

    def test_child_executable_must_contribute_coverage(self):
        root = Path.cwd()

        def change(edit):
            document = coverage(root)
            edit(document, document['data'][0]['files'])
            return document

        for case, document, said in [
            ('another export', change(lambda document, files: document.update(type='llvm.profdata')),
             'not an LLVM coverage export'),
            ('two reports', change(lambda document, files: document['data'].append(document['data'][0])),
             'expected one coverage report'),
            ('main.rs unrun', coverage(root, main=0), 'src/main.rs was not executed'),
            ('lib.rs unrun', coverage(root, lib=0), 'src/lib.rs was not executed'),
            ('main.rs of another checkout',
             change(lambda document, files: files[0].update(filename=str(root / 'other/src/main.rs'))),
             'src/main.rs was not executed'),
            ('main.rs twice', change(lambda document, files: files.append(files[0])), 'src/main.rs was not executed'),
        ]:
            with self.subTest(case), self.assertRaisesRegex(AssertionError, said):
                summary(document, root)

    def test_the_summary_is_added_to_the_step_summary(self):
        with tempfile.TemporaryDirectory() as folder:
            report, steps = Path(folder, 'coverage.json'), Path(folder, 'summary.md')
            report.write_text(json.dumps(coverage(ROOT)), encoding='utf-8')
            steps.write_text('earlier steps\n', encoding='utf-8')
            printed = io.StringIO()
            with patch.object(sys, 'argv', ['check_coverage.py', str(report), '--summary', str(steps)]), \
                 contextlib.redirect_stdout(printed):
                check_coverage.main()
            table = summary(coverage(ROOT), ROOT)
            self.assertEqual(steps.read_text(encoding='utf-8'), 'earlier steps\n' + table)
        self.assertEqual(printed.getvalue(), table + '\n')

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


class MutationRun(unittest.TestCase):
    """run_mutants.py against a stand-in for cargo-mutants 27.1, which deep.yml pins: its slice k
    of n is the k-th run of the listing's first ceil(total / n) mutants, then the next."""

    FILE = 'src/place.rs'

    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.output = Path(folder.name) / 'mutants'
        self.names = [f'{self.FILE}:{line}:5: replace place -> bool with false' for line in range(12)]
        self.files = {}
        self.slices = None
        self.code = 0
        self.planned = None
        self.executed = None
        self.ran = []

    def mutants(self, command, **options):
        """answer ``command`` as cargo-mutants would."""
        if '--list' in command:
            names = self.names
            if '--shard' in command:
                k, n = map(int, command[command.index('--shard') + 1].split('/'))
                size = self.slices or -(-len(names) // n)
                names = names[k * size:(k + 1) * size]
            listing = [{'file': self.files.get(name, self.FILE), 'name': name} for name in names]
            return subprocess.CompletedProcess(command, 0, json.dumps(listing), '')
        self.ran.append(command)
        given = self.names[:6] if self.planned is None else self.planned
        out = Path(command[command.index('--output') + 1]) / 'mutants.out'
        out.mkdir(parents=True, exist_ok=True)
        (out / 'mutants.json').write_text(json.dumps([{'name': name} for name in given]), encoding='utf-8')
        executed = given if self.executed is None else self.executed
        outcomes = [{'scenario': 'Baseline', 'summary': 'Success'},
                    *({'scenario': {'Mutant': {'name': name}}, 'summary': 'CaughtMutant'} for name in executed)]
        (out / 'outcomes.json').write_text(json.dumps({'outcomes': outcomes}), encoding='utf-8')
        return subprocess.CompletedProcess(command, self.code)

    def main(self):
        """run_mutants.py's exit code and what it printed."""
        printed = io.StringIO()
        with patch.object(sys, 'argv', ['run_mutants.py', self.FILE, '--output', str(self.output)]), \
             patch.object(run_mutants.subprocess, 'run', side_effect=self.mutants), \
             contextlib.redirect_stdout(printed), self.assertRaises(SystemExit) as stopped:
            run_mutants.main()
        return stopped.exception.code, printed.getvalue()

    def test_the_first_slice_is_announced_run_and_checked(self):
        for code in [0, 2]:
            with self.subTest(code=code):
                self.code, self.ran = code, []
                exit_code, printed = self.main()
                self.assertEqual(exit_code, code)
                self.assertEqual(printed.splitlines(), [
                    f'6 selected mutants in {self.FILE}, slice 0/2, judged by the lib tests; '
                    'the unmutated baseline runs first', *self.names[:6]])
                self.assertEqual(json.loads((self.output / 'selected.json').read_text(encoding='utf-8')), self.names[:6])
                self.assertEqual(self.ran[0][:11], ['cargo', 'mutants', '--file', self.FILE, '--colors', 'never',
                                                    '--shard', '0/2', '--sharding', 'slice', '--baseline'])

    def test_a_run_that_tested_other_mutants_fails(self):
        # the plan is checked whatever the run's end, the outcomes when it judged every mutant:
        # 0 when all were caught, 2 when some were missed
        for change, code in [('planned', 4), ('executed', 0), ('executed', 2)]:
            with self.subTest(change=change, code=code):
                self.setUp()
                self.code = code
                setattr(self, change, self.names[:5])
                with self.assertRaisesRegex(AssertionError, 'differs from the announced selection'):
                    self.main()

    def test_a_run_without_outcomes_to_check_keeps_its_exit_code(self):
        # cargo-mutants exits 4 when the unmutated tree fails its tests, and judges no mutant
        self.code, self.executed = 4, []
        self.assertEqual(self.main()[0], 4)

    def test_a_listing_outside_the_file_or_an_uneven_slice_fails(self):
        for case, said in [('escaped', 'escaped the requested file'), ('slice', 'differs from the announced selection')]:
            with self.subTest(case):
                self.setUp()
                if case == 'escaped':
                    self.files[self.names[3]] = 'src/other.rs'
                else:
                    self.slices = 5
                with self.assertRaisesRegex(AssertionError, said):
                    self.main()
                self.assertEqual(self.ran, [])
