"""run the fetchloom program this package installs, as ``fl`` and ``python -m fetchloom`` do."""

import os
import signal
import subprocess
import sys

from fetchloom import _program


def main() -> int:
    """run fetchloom with this command line's arguments, stdin, stdout and stderr.

    on unix fetchloom takes over this process. on windows it runs as a child, and its exit
    code is returned. as in a shell, 127 means the program was not found and 126 that it
    could not start.
    """
    try:
        program = _program()
    except FileNotFoundError as error:
        print(f"error: {error}", file=sys.stderr)
        return 127
    arguments = [program, *sys.argv[1:]]
    try:
        if sys.platform == "win32":
            # windows has no exec. ctrl-c reaches fetchloom too, which decides how to stop
            signal.signal(signal.SIGINT, signal.SIG_IGN)
            return subprocess.run(arguments, check=False).returncode
        # python starts with these ignored, and fetchloom would keep that through exec
        for name in ("SIGPIPE", "SIGXFSZ"):
            if hasattr(signal, name):
                signal.signal(getattr(signal, name), signal.SIG_DFL)
        os.execv(program, arguments)
    except OSError as error:
        print(f"error: could not run {program}: {error.strerror or error}", file=sys.stderr)
        return 126


if __name__ == "__main__":
    sys.exit(main())
