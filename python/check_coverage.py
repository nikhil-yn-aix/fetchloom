"""summarize LLVM coverage only when the CLI's subprocess profiles are present."""

import argparse
import json
from pathlib import Path

from ci_journey import ROOT, require


def summary(document, root):
    require(document['type'] == 'llvm.coverage.json.export', 'not an LLVM coverage export')
    reports = document['data']
    require(len(reports) == 1, 'expected one coverage report')
    report = reports[0]
    main = (root / 'src/main.rs').resolve()
    entrypoints = [file for file in report['files'] if Path(file['filename']).resolve() == main]
    require(len(entrypoints) == 1 and entrypoints[0]['summary']['functions']['covered'] > 0,
            'CLI child-process coverage is missing: src/main.rs was not executed')
    lines = ['## linux coverage', '', 'CLI subprocess profiles include src/main.rs.', '',
             '| measure | covered | total | percent |', '|---|---:|---:|---:|']
    for name in ['lines', 'functions', 'regions']:
        counts = report['totals'][name]
        lines.append(f"| {name} | {counts['covered']} | {counts['count']} | {counts['percent']:.2f}% |")
    lines.extend(['', 'coverage identifies gaps; no percentage threshold is enforced.', ''])
    return '\n'.join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('report', type=Path)
    parser.add_argument('--summary', type=Path, required=True)
    args = parser.parse_args()
    text = summary(json.loads(args.report.read_text(encoding='utf-8')), ROOT)
    with args.summary.open('a', encoding='utf-8') as stream:
        stream.write(text)
    print(text)


if __name__ == '__main__':
    main()
