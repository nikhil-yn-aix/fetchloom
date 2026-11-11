"""run a bounded, explicitly listed mutation pilot in one Rust source file."""

import argparse
import json
from pathlib import Path
import re
import subprocess

from ci_journey import ROOT, require


def source_file(root, requested):
    path = (root / requested).resolve()
    require(path.is_relative_to((root / 'src').resolve()) and path.suffix == '.rs' and path.is_file(),
            'mutation scope must name one existing Rust file under src/')
    return path.relative_to(root.resolve()).as_posix()


def selection(file, text):
    names = [name for name in text.splitlines() if name.strip()]
    require(names and all(name.startswith(file + ':') for name in names),
            'mutation listing is empty or escaped the requested file')
    chosen = names[:10]
    expression = '^(?:' + '|'.join(re.escape(name) for name in chosen) + ')$'
    return chosen, expression


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('file')
    parser.add_argument('--output', type=Path, default=Path('target/ci-mutants'))
    args = parser.parse_args()
    file = source_file(ROOT, args.file)
    args.output.mkdir(parents=True, exist_ok=True)
    listed = subprocess.run(['cargo', 'mutants', '--list', '--file', file], cwd=ROOT,
                            capture_output=True, text=True, check=True, timeout=120)
    names, expression = selection(file, listed.stdout)
    (args.output / 'selected.json').write_text(json.dumps(names, indent=2) + '\n', encoding='utf-8')
    print(f'{len(names)} selected mutants in {file}; the unmutated baseline runs first', flush=True)
    print('\n'.join(names), flush=True)
    result = subprocess.run(['cargo', 'mutants', '--file', file, '--re', expression,
                             '--baseline', 'run', '--jobs', '2', '--timeout', '120',
                             '--build-timeout', '600', '--no-shuffle', '--test-tool', 'cargo',
                             '--output', str(args.output.resolve()), '--', '--locked', '--lib'], cwd=ROOT)
    raise SystemExit(result.returncode)


if __name__ == '__main__':
    main()
