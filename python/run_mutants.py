"""run a bounded, explicitly listed mutation pilot in one Rust source file."""

import argparse
import itertools
import json
from pathlib import Path
import re
import subprocess

from ci_journey import ROOT, require

TESTS = {'lib': '--lib', 'all': '--all-targets'}
# the characters the regex crate behind cargo-mutants' --re gives a meaning
META = frozenset('\\.+*?()|[]{}^$#&-~')


def source_file(root, requested):
    """``requested`` as a path from ``root``, refused unless it is one rust file under src."""
    path = (root / requested).resolve()
    require(path.is_relative_to((root / 'src').resolve()) and path.suffix == '.rs' and path.is_file(),
            'mutation scope must name one existing Rust file under src/')
    return path.relative_to(root.resolve()).as_posix()


def listed_names(file, text):
    """the mutant names ``text`` lists, refused unless each is ``file``'s own, once and plain."""
    names = [name for name in text.splitlines() if name.strip()]
    require(names and len(names) == len(set(names))
            and all(name.startswith(file + ':') and '\x1b' not in name for name in names),
            'mutation listing is empty, duplicated, colored or escaped the requested file')
    return names


def selection(file, text, slice=0):
    """slice ``slice`` of the mutants ``text`` lists, cut into slices of at most ten as cargo-mutants
    cuts them, and that slice as its ``--shard`` names it; refused unless each mutant is ``file``'s
    own, once and plain, and the file has that slice."""
    names = listed_names(file, text)
    shards = (len(names) + 9) // 10
    size = (len(names) + shards - 1) // shards
    require(0 <= slice < shards, f'{file} has {shards} slices of mutants: choose a slice from 0 to {shards - 1}')
    return names[slice * size:(slice + 1) * size], f'{slice}/{shards}'


def named(file, text, pattern):
    """the mutants ``text`` lists whose names the python regex ``pattern`` finds; refused unless
    each mutant is ``file``'s own, once and plain, and one to ten are found."""
    names = listed_names(file, text)
    found = [name for name in names if re.search(pattern, name)]
    require(0 < len(found) <= 10,
            f'{pattern} finds {len(found)} of the {len(names)} mutants in {file}: name one to ten')
    return found


def exactly(names):
    """a regex cargo-mutants' ``--re`` matches against each of ``names`` and no other name."""
    escaped = (''.join('\\' + character if character in META else character for character in name)
               for name in names)
    return '^(?:' + '|'.join(escaped) + ')$'


def marked(file, source, names):
    """a diff that adds, to the text ``source`` of ``file``, the line each of ``names`` starts on.
    cargo-mutants' ``--re`` lets through struct-field mutants it does not name, and its
    ``--in-diff`` keeps out those on other lines."""
    lines = source.split('\n')
    numbers = sorted({int(name[len(file) + 1:].split(':', 1)[0]) for name in names})
    diff = [f'--- a/{file}', f'+++ b/{file}']
    added = 0
    for _, run in itertools.groupby(enumerate(numbers), lambda pair: pair[1] - pair[0]):
        run = [number for _, number in run]
        diff.append(f'@@ -{run[0] - 1 - added},0 +{run[0]},{len(run)} @@')
        diff += ['+' + lines[number - 1] for number in run]
        added += len(run)
    return '\n'.join(diff) + '\n'


def require_selection(expected, actual):
    """refuse ``actual`` unless it names each mutant ``expected`` names, once."""
    require(len(actual) == len(set(actual)) and set(actual) == set(expected),
            'the mutation inventory differs from the announced selection')


def mutation_run(command, tests, output):
    """the cargo-mutants run that judges each mutant by ``tests``, after the unmutated baseline."""
    # no fixed --timeout, which would cap the unmutated baseline too; the multiplier scales from it
    return [*command, '--baseline', 'run', '--jobs', '2', '--timeout-multiplier', '3',
            '--build-timeout', '600', '--no-shuffle', '--test-tool', 'cargo',
            '--output', str(output), '--', '--locked', TESTS[tests]]


def arguments(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('file')
    parser.add_argument('--tests', choices=sorted(TESTS), default='lib',
                        help='the tests that judge each mutant: lib, or all for a file only the binary reaches')
    parser.add_argument('--slice', type=int, default=0,
                        help="which slice of at most ten of the file's mutants to test, from 0")
    parser.add_argument('--mutants', default='',
                        help="a regex finding one to ten of the file's mutants by name, tested instead of a slice")
    parser.add_argument('--output', type=Path, default=Path('target/ci-mutants'))
    return parser.parse_args(argv)


def main():
    args = arguments()
    file = source_file(ROOT, args.file)
    require(not (args.mutants and args.slice), 'name mutants or choose a slice, not both')
    args.output.mkdir(parents=True, exist_ok=True)
    command = ['cargo', 'mutants', '--file', file, '--colors', 'never']

    def listing(arguments):
        listed = subprocess.run([*arguments, '--list', '--json'], cwd=ROOT,
                                capture_output=True, text=True, check=True, timeout=120)
        document = json.loads(listed.stdout)
        require(all(mutant['file'] == file for mutant in document), 'mutation listing escaped the requested file')
        return [mutant['name'] for mutant in document]

    text = '\n'.join(listing(command))
    if args.mutants:
        names = named(file, text, args.mutants)
        diff = args.output.resolve() / 'selected.diff'
        diff.write_text(marked(file, (ROOT / file).read_text(encoding='utf-8'), names), encoding='utf-8')
        command += ['--in-diff', str(diff), '--re', exactly(names)]
        scope = f'found by {args.mutants}'
    else:
        names, shard = selection(file, text, args.slice)
        command += ['--shard', shard, '--sharding', 'slice']
        scope = f'slice {shard.replace("/", " of ")}'
    require_selection(names, listing(command))
    (args.output / 'selected.json').write_text(json.dumps(names, indent=2) + '\n', encoding='utf-8')
    print(f'{len(names)} selected mutants in {file}, {scope}, judged by the {args.tests} '
          'tests; the unmutated baseline runs first', flush=True)
    print('\n'.join(names), flush=True)
    result = subprocess.run(mutation_run(command, args.tests, args.output.resolve()), cwd=ROOT)
    planned = json.loads((args.output / 'mutants.out/mutants.json').read_text(encoding='utf-8'))
    require_selection(names, [mutant['name'] for mutant in planned])
    if result.returncode in (0, 2):
        outcomes = json.loads((args.output / 'mutants.out/outcomes.json').read_text(encoding='utf-8'))
        executed = [outcome['scenario']['Mutant']['name'] for outcome in outcomes['outcomes']
                    if isinstance(outcome['scenario'], dict) and 'Mutant' in outcome['scenario']]
        require_selection(names, executed)
    raise SystemExit(result.returncode)


if __name__ == '__main__':
    main()
