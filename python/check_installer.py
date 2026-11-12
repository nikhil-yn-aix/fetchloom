"""run repository installers against the checked archive on loopback only."""

import argparse
import contextlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile

from check_artifacts import archive_name
from ci_journey import ROOT, Journey, Server, check_version, digest, environment, fixtures, require


@contextlib.contextmanager
def user_path():
    if sys.platform != 'win32':
        yield None
        return
    import winreg
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, 'Environment', 0, winreg.KEY_READ | winreg.KEY_WRITE) as key:
        saved = {}
        for name in ['Path', 'FETCHLOOM_INSTALL']:
            try:
                saved[name] = winreg.QueryValueEx(key, name)
            except FileNotFoundError:
                saved[name] = None
        try:
            yield (key, saved)
        finally:
            for name, value in saved.items():
                if value is None:
                    try:
                        winreg.DeleteValue(key, name)
                    except FileNotFoundError:
                        pass
                else:
                    winreg.SetValueEx(key, name, 0, value[1], value[0])


def invalid_archive(target, version):
    folder = f'fetchloom-{version}-{target}'
    data = io.BytesIO()
    if 'windows' in target:
        with zipfile.ZipFile(data, 'w') as archive:
            archive.writestr(folder + '/fetchloom.exe', b'not an executable\n')
    else:
        with tarfile.open(fileobj=data, mode='w:gz') as archive:
            entry = tarfile.TarInfo(folder + '/fetchloom')
            body = b'not an executable\n'
            entry.size, entry.mode = len(body), 0o755
            archive.addfile(entry, io.BytesIO(body))
    return data.getvalue()


def check(args):
    import hashlib
    archive = args.dist / archive_name(args.target, args.version)
    body = archive.read_bytes()
    sums = f'{hashlib.sha256(body).hexdigest()}  {archive.name}\n'.encode()
    with tempfile.TemporaryDirectory(prefix='fetchloom-install-', dir=os.environ.get('RUNNER_TEMP')) as directory, user_path() as registry:
        root = Path(directory)
        home = root / 'home with spaces'
        home.mkdir()
        profile = home / '.profile'
        profile.write_text('# existing profile\n', encoding='utf-8')
        with Server({'/SHA256SUMS': sums, '/' + archive.name: body}) as server:
            env = environment(home)
            env.update(FETCHLOOM_DOWNLOAD_URL=server.origin, FETCHLOOM_NO_MODIFY_PATH='1')
            if args.shell == 'sh':
                command = ['sh', str(ROOT / 'install.sh'), args.version]
                installed = home / '.local/bin/fetchloom'
            else:
                shell = shutil.which(args.shell)
                require(shell is not None, f'missing requested shell {args.shell}')
                actual = subprocess.check_output([shell, '-NoProfile', '-NonInteractive', '-Command', '$PSVersionTable.PSVersion.ToString()'], text=True, timeout=20).strip()
                require(actual.startswith('5.1.' if args.shell == 'powershell' else '7.'), f'unexpected {args.shell} version: {actual}')
                print(f'{args.shell}: {actual}')
                command = [shell, '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', str(ROOT / 'install.ps1'), '-Version', args.version]
                installed = home / 'Programs/fetchloom/bin/fetchloom.exe'
            def unchanged():
                require(installed.read_bytes() == previous, 'failed install replaced the installation')
                require(profile.read_text(encoding='utf-8') == '# existing profile\n', 'failed install changed a profile')
                if registry is not None:
                    key, saved = registry
                    for name, old in saved.items():
                        try:
                            value = winreg.QueryValueEx(key, name)
                        except FileNotFoundError:
                            value = None
                        require(value == old, f'failed install changed user {name}')

            def invoke(success):
                try:
                    result = subprocess.run(command, cwd=root, env=env, stdin=subprocess.DEVNULL,
                                            capture_output=True, text=True, timeout=90)
                except subprocess.TimeoutExpired as error:
                    told = [part.decode(errors='replace') if isinstance(part, bytes) else part or ''
                            for part in (error.stdout, error.stderr)]
                    raise RuntimeError('the installer ran past 90 s:\n' + ''.join(told)) from error
                require((result.returncode == 0) == success, result.stdout + result.stderr)
                return result
            invoke(True)
            require(digest(installed) == digest(args.binary), 'installer selected different bytes')
            check_version(installed.with_name('fl.exe' if sys.platform == 'win32' else 'fl'), args.version, env, cwd=root)
            invoke(True)
            require(profile.read_text(encoding='utf-8') == '# existing profile\n', 'no-modify-path changed a profile')
            if registry is not None:
                import winreg
                key, saved = registry
                try:
                    value = winreg.QueryValueEx(key, 'Path')
                except FileNotFoundError:
                    value = None
                require(value == saved['Path'], 'no-modify-path changed user PATH')
            journey_root = root / 'journey'
            journey_root.mkdir()
            with Server(fixtures()) as inputs:
                proof = Journey(installed, journey_root, inputs).check(args.version, args.commit)
            previous = installed.read_bytes()
            server.bodies['/SHA256SUMS'] = f'{"0" * 64}  {archive.name}\n'.encode()
            invoke(False)
            unchanged()
            server.bodies['/SHA256SUMS'] = b''
            invoke(False)
            unchanged()
            broken = invalid_archive(args.target, args.version)
            server.bodies['/' + archive.name] = broken
            server.bodies['/SHA256SUMS'] = f'{hashlib.sha256(broken).hexdigest()}  {archive.name}\n'.encode()
            invoke(False)
            unchanged()
            server.bodies['/' + archive.name] = body
            server.bodies['/SHA256SUMS'] = sums
            env.pop('FETCHLOOM_NO_MODIFY_PATH')
            invoke(True)
            invoke(True)
            if args.shell == 'sh':
                line = 'export PATH="$HOME/.local/bin:$PATH"'
                require(profile.read_text(encoding='utf-8').count(line) == 1, 'profile update was not idempotent')
                if sys.platform == 'darwin':
                    require((home / '.zshrc').read_text(encoding='utf-8').count(line) == 1, 'zsh PATH was not installed exactly once')
            else:
                key, saved = registry
                value, kind = winreg.QueryValueEx(key, 'Path')
                entries = [os.path.expandvars(entry).rstrip('\\').lower() for entry in value.split(';')]
                require(entries.count(str(installed.parent).lower()) == 1, 'user PATH was not installed exactly once')
                if saved['Path'] is not None:
                    require(kind == saved['Path'][1], 'user PATH registry type changed')
            proof['shell'] = args.shell
            return proof


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target', required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--dist', type=Path, default=Path('dist'))
    parser.add_argument('--shell', choices=['sh', 'powershell', 'pwsh'], required=True)
    parser.add_argument('--proofs', type=Path, required=True)
    args = parser.parse_args()
    proof = check(args)
    args.proofs.mkdir(parents=True, exist_ok=True)
    (args.proofs / f'installer-{args.shell}.json').write_text(json.dumps(proof, indent=2) + '\n', encoding='utf-8')
    print(json.dumps(proof))


if __name__ == '__main__':
    main()
