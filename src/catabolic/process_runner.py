# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Bounded subprocess I/O and process-group lifetime shared by adapters."""

import os
import selectors
import signal
import subprocess
import time


class CommandFailure(Exception):
    def __init__(self, state, message):
        self.state = state
        super().__init__(message)


def command_output(
    argv,
    *,
    pass_fds=(),
    timeout=20,
    maximum=2 * 1024 * 1024,
    cancel=None,
    env=None,
    input_bytes=None,
    capture=True,
    on_start=None,
    on_poll=None,
):
    """Own the whole process group, including descendants after the leader exits.

    Input uses a private pipe rather than argv/environment. Discarded output is
    still drained and bounded. on_start distinguishes failure to launch from an
    unknown external outcome. Kernel process creation/waits remain OS operations.
    """
    deadline = time.monotonic() + timeout
    proc = subprocess.Popen(
        argv,
        stdin=subprocess.PIPE if input_bytes is not None else subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        pass_fds=pass_fds,
        start_new_session=True,
        env=env,
    )
    buffers = {"stdout": bytearray(), "stderr": bytearray()}
    try:
        if on_start is not None:
            on_start()
        with selectors.DefaultSelector() as selector:
            for label, pipe in (("stdout", proc.stdout), ("stderr", proc.stderr)):
                os.set_blocking(pipe.fileno(), False)
                selector.register(pipe, selectors.EVENT_READ, label)
            pending_input = memoryview(input_bytes or b"")
            if proc.stdin is not None:
                os.set_blocking(proc.stdin.fileno(), False)
                selector.register(proc.stdin, selectors.EVENT_WRITE, "stdin")
            total = 0
            while selector.get_map() or proc.poll() is None:
                if on_poll is not None:
                    on_poll()
                if cancel is not None and cancel.is_set():
                    raise CommandFailure("cancelled", "command cancelled")
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise CommandFailure(
                        "timeout", "command exceeded the elapsed-time limit"
                    )
                for key, _ in selector.select(min(0.05, remaining)):
                    if key.data == "stdin":
                        try:
                            written = os.write(key.fd, pending_input[:4096])
                            pending_input = pending_input[written:]
                        except BrokenPipeError:
                            pending_input = pending_input[:0]
                        if not pending_input:
                            selector.unregister(key.fileobj)
                            proc.stdin.close()
                        continue
                    data = os.read(key.fd, 65536)
                    if not data:
                        selector.unregister(key.fileobj)
                        continue
                    total += len(data)
                    if total > maximum:
                        raise CommandFailure(
                            "partial", "command output exceeded the byte limit"
                        )
                    if capture:
                        buffers[key.data].extend(data)
            code = proc.wait()
            if code:
                detail = buffers["stderr"].decode("utf-8", "replace")[:1000]
                raise CommandFailure(
                    "failed", f"command exit {code}" + (f": {detail}" if detail else "")
                )
            return bytes(buffers["stdout"])
    finally:
        # poll() only describes the leader. Always retire our group, including
        # descendants that retained pipes or closed them and kept running.
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        proc.wait()
        for pipe in (proc.stdin, proc.stdout, proc.stderr):
            if pipe is not None:
                pipe.close()
