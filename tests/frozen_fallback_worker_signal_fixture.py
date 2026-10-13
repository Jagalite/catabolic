# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Select the actual frozen fallback worker's idle SIGINT boundary.

No business function is replaced. A one-shot trace reports readiness privately,
then the parent sends a real signal after checking writer release.
"""

import inspect
import json
import os
import signal
import sys
from pathlib import Path


def main():
    source = Path(sys.argv.pop(1)).resolve()
    ready_fd = int(sys.argv.pop(1))
    sys.path.insert(0, str(source))
    from catabolic import cli, fallback_worker

    for module in (cli, fallback_worker):
        assert Path(module.__file__).resolve() == (
            source / "catabolic" / (module.__name__.rsplit(".", 1)[1] + ".py")
        )
    lines, first = inspect.getsourcelines(fallback_worker.run)
    selected = [
        first + index
        for index, line in enumerate(lines)
        if line.strip() == "time.sleep(interval)"
    ]
    assert len(selected) == 1, "frozen idle boundary changed"

    def trace(frame, event, arg):
        if frame.f_code is not fallback_worker.run.__code__:
            return None
        if event == "line" and frame.f_lineno == selected[0]:
            sys.settrace(None)
            os.write(ready_fd, json.dumps({"pid": os.getpid()}).encode() + b"\n")
            os.close(ready_fd)
            while True:
                signal.pause()
        return trace

    sys.settrace(trace)
    try:
        return cli.main(sys.argv[1:])
    finally:
        sys.settrace(None)


if __name__ == "__main__":
    raise SystemExit(main())
