"""exercise public source and search services on the weekly mac runner."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

from ci_journey import environment, remove_tree, require


def check(binary):
    with tempfile.TemporaryDirectory(prefix='fetchloom-live-', dir=os.environ.get('RUNNER_TEMP')) as folder:
        root = Path(folder)
        home, project = root / 'home', root / 'project'
        home.mkdir()
        project.mkdir()
        env = environment(home, root / 'cache')

        def run(*arguments):
            result = subprocess.run([str(binary), '--json', '-C', str(project), *arguments],
                                    cwd=project, env=env, stdin=subprocess.DEVNULL,
                                    capture_output=True, text=True, timeout=180)
            print('+ fetchloom', *arguments, '=>', result.returncode, flush=True)
            print(result.stdout, result.stderr, flush=True)
            require(result.returncode == 0, 'live command failed; inspect the captured service error')
            document = json.loads(result.stdout)
            require(document['status'] == 'ok', 'live command returned an error')
            return document['result']

        run('add', 'zenodo:3608990', '--as', 'iris')
        require(any(path.is_file() for path in (project / 'data').rglob('*')), 'add placed no files')
        first = {path.relative_to(project / 'data'): path.read_bytes()
                 for path in (project / 'data').rglob('*') if path.is_file()}
        run('status')
        remove_tree(project / 'data')
        run('sync', '--locked')
        require(all((project / 'data' / path).read_bytes() == body for path, body in first.items()),
                'sync did not restore the public dataset bytes')
        require(run('get', 'zenodo:3608990', '--dir', str(root / 'got'))['files'], 'get placed no files')
        # every site, ENA and dandi among them, which a search asks only when named
        every = 'hf,kaggle,zenodo,ena,geo,dandi,openneuro'
        require(run('search', 'iris', '--source', every)['candidates'], 'search found no candidates')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    args = parser.parse_args()
    check(args.binary.resolve())


if __name__ == '__main__':
    main()
