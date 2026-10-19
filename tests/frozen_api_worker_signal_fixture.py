# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Disposable, explicitly selected SIGINT schedules in the frozen Python CLI.

This subprocess helper imports and executes the actual reference files without
rewriting them. A one-shot main-thread trace pauses at either the worker's idle
loop or its tick Store.close writer-release line. The parent must send a real
SIGINT. These selected schedules are not general cancellation qualification.
"""

import inspect
import json
import os
import signal
import sys
from pathlib import Path


def main():
    if not __debug__:
        raise RuntimeError("selected signal fixtures require assertions")
    source = Path(sys.argv.pop(1)).resolve()
    boundary = sys.argv.pop(1)
    ready_fd = int(sys.argv.pop(1))
    sys.path.insert(0, str(source))
    from catabolic import cli, http_worker_local, store

    for module in (cli, http_worker_local, store):
        assert Path(module.__file__).resolve() == (
            source / "catabolic" / (module.__name__.rsplit(".", 1)[1] + ".py")
        )
    if boundary == "idle":
        function = http_worker_local.run
        selected = "time.sleep(interval)"
    elif boundary == "tick-close":
        function = store.Store.close
        selected = "os.close(self.lock_fd)"
    else:
        raise AssertionError("unknown selected signal boundary: " + boundary)
    lines, first = inspect.getsourcelines(function)
    matching = [first + i for i, line in enumerate(lines) if line.strip() == selected]
    assert len(matching) == 1, "frozen selected boundary changed"

    def trace(frame, event, arg):
        if frame.f_code is not function.__code__:
            return None
        if event == "line" and frame.f_lineno == matching[0]:
            if boundary == "tick-close":
                # Ignore recovery, notifier and final cleanup stores. The
                # selected close must belong to the actual heartbeat tick.
                caller = frame.f_back
                while caller and caller.f_code is not http_worker_local.tick.__code__:
                    caller = caller.f_back
                if caller is None:
                    return trace
                owner = frame.f_locals["self"]
                assert owner.db is None and owner.lock_fd is not None
                os.fstat(owner.lock_fd)
                profile = caller.f_locals["profile"]
            else:
                profile = frame.f_locals["profile"]
            sys.settrace(None)
            os.write(
                ready_fd,
                (
                    json.dumps(
                        {"boundary": boundary, "pid": os.getpid(), "profile": profile}
                    )
                    + "\n"
                ).encode(),
            )
            os.close(ready_fd)
            # No fixed delay, synthetic exception, substituted worker or
            # stdout marker: SIGINT interrupts this selected trace boundary.
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
