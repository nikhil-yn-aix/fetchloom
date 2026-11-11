"""run a bounded, explicitly listed mutation pilot in one Rust source file."""

import argparse
import json
from pathlib import Path
import subprocess

from ci_journey import ROOT, require


def source_file(root, requested):
    path = (root / requested).resolve()
    require(path.is_relative_to((root / 'src').resolve()) and path.suffix == '.rs' and path.is_file(),
            'mutation scope must name one existing Rust file under src/')
    return path.relative_to(root.resolve()).as_posix()


def selection(file, text):
    names = [name for name in text.splitlines() if name.strip()]
    require(names and len(names) == len(set(names))
            and all(name.startswith(file + ':') and '\x1b' not in name for name in names),
            'mutation listing is empty, duplicated, colored or escaped the requested file')
    shards = (len(names) + 9) // 10
    size = (len(names) + shards - 1) // shards
    return names[:size], f'0/{shards}'


def require_selection(expected, actual):
    require(len(actual) == len(set(actual)) and set(actual) == set(expected),
            'the mutation inventory differs from the announced selection')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('file')
    parser.add_argument('--output', type=Path, default=Path('target/ci-mutants'))
    args = parser.parse_args()
    file = source_file(ROOT, args.file)
    args.output.mkdir(parents=True, exist_ok=True)
    command = ['cargo', 'mutants', '--file', file, '--colors', 'never']

    def listing(arguments):
        listed = subprocess.run([*arguments, '--list', '--json'], cwd=ROOT,
                                capture_output=True, text=True, check=True, timeout=120)
        document = json.loads(listed.stdout)
        require(all(mutant['file'] == file for mutant in document), 'mutation listing escaped the requested file')
        return [mutant['name'] for mutant in document]

    names, shard = selection(file, '\n'.join(listing(command)))
    command += ['--shard', shard, '--sharding', 'slice']
    require_selection(names, listing(command))
    (args.output / 'selected.json').write_text(json.dumps(names, indent=2) + '\n', encoding='utf-8')
    print(f'{len(names)} selected mutants in {file}, slice {shard}; the unmutated baseline runs first', flush=True)
    print('\n'.join(names), flush=True)
    result = subprocess.run([*command, '--baseline', 'run', '--jobs', '2', '--timeout', '120',
                             '--build-timeout', '600', '--no-shuffle', '--test-tool', 'cargo',
                             '--output', str(args.output.resolve()), '--', '--locked', '--all-targets'], cwd=ROOT)
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
