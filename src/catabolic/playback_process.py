# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Retire the encoder process group if its API worker disappears."""

import os
import signal
import subprocess
import sys
import time


def main():
    parent = int(sys.argv[1])
    # The outer runner creates this private process group. Keep inherited media
    # descriptors open in FFmpeg, and inherit bounded stdout/stderr pipes.
    descriptors = [int(v) for v in sys.argv[2].split(",")]
    # macOS /dev/fd directory paths cannot create child files. A descriptor-pinned
    # cwd lets the muxer create/rename relative cache files without resolving the
    # owner's pathname again or changing the supervisor's working directory.
    os.fchdir(descriptors[1])
    proc = subprocess.Popen(sys.argv[3:], pass_fds=descriptors)
    while proc.poll() is None:
        if os.getppid() != parent:
            os.killpg(os.getpgrp(), signal.SIGKILL)
        time.sleep(0.1)
    return proc.wait()


if __name__ == "__main__":
    raise SystemExit(main())
