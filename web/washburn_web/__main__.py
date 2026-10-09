"""``python -m washburn_web public|admin``: same as the console scripts."""

import sys

from washburn_web.cli import run_admin, run_public

_RUNNERS = {"public": run_public, "admin": run_admin}


def main() -> None:
    if len(sys.argv) != 2 or sys.argv[1] not in _RUNNERS:
        raise SystemExit("usage: python -m washburn_web {public|admin}")
    _RUNNERS[sys.argv[1]]()


if __name__ == "__main__":
    main()
