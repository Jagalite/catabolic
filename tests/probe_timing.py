# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Controlled startup delay for a real frozen-reference probe subprocess."""

import sys
from contextlib import contextmanager
from unittest.mock import patch

from catabolic import fallback_probe, fallback_resolution


@contextmanager
def delay_first_probe_start(path, seconds=1.25):
    """Delay one matching child, without changing its real result or deadline."""
    probe = fallback_resolution.probe
    popen = fallback_probe.subprocess.Popen
    observations = []

    def controlled_probe(database, snapshots, timeout_ms, **kwargs):
        if observations or [snapshot["path"] for snapshot in snapshots] != [path]:
            return probe(database, snapshots, timeout_ms, **kwargs)
        observed = {"timeout_ms": timeout_ms, "delayed_children": 0}
        observations.append(observed)

        def delayed_popen(command, *args, **options):
            if command == [sys.executable, "-m", "catabolic.fallback_probe"]:
                observed["delayed_children"] += 1
                command = [
                    sys.executable,
                    "-c",
                    f"import runpy, time; time.sleep({seconds!r}); "
                    'runpy.run_module("catabolic.fallback_probe", run_name="__main__")',
                ]
            return popen(command, *args, **options)

        with patch.object(fallback_probe.subprocess, "Popen", delayed_popen):
            observed["result"] = probe(database, snapshots, timeout_ms, **kwargs)
        return observed["result"]

    with patch.object(fallback_resolution, "probe", controlled_probe):
        yield observations
