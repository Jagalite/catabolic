# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Portable process-table witnesses for Linux and macOS test harnesses.

The kernel's own process table is read through ``ps -A``, which both procps
(Linux) and BSD ps (macOS) provide with these exact keywords. Zombies are
reported as present, so "gone" means the process has also been reaped.
"""

import subprocess


def table():
    """Return {pid: (ppid, stat, args)} for every process, zombies included."""
    output = subprocess.run(
        ["ps", "-A", "-o", "pid=,ppid=,stat=,args="],
        capture_output=True,
        text=True,
        check=True,
        timeout=10,
    ).stdout
    processes = {}
    for line in output.splitlines():
        fields = line.split(None, 3)
        if len(fields) < 3:
            continue
        pid, ppid, stat = int(fields[0]), int(fields[1]), fields[2]
        processes[pid] = (ppid, stat, fields[3] if len(fields) == 4 else "")
    return processes


def children(pid):
    """Direct children of ``pid`` as [(child_pid, args)], sorted by PID."""
    return sorted(
        (child, args) for child, (ppid, _, args) in table().items() if ppid == pid
    )


def descendants(pid):
    """Every transitive descendant PID of ``pid``."""
    processes = table()
    found = set()
    pending = [pid]
    while pending:
        parent = pending.pop()
        for child, (ppid, _, _) in processes.items():
            if ppid == parent and child not in found:
                found.add(child)
                pending.append(child)
    return found


def present(pids):
    """The subset of ``pids`` still in the process table (zombies included)."""
    processes = table()
    return sorted(pid for pid in pids if pid in processes)


def group(pgid, live=False):
    """Members of process group ``pgid`` (zombies excluded when ``live``)."""
    output = subprocess.run(
        ["ps", "-A", "-o", "pid=,pgid=,stat="],
        capture_output=True,
        text=True,
        check=True,
        timeout=10,
    ).stdout
    members = []
    for line in output.splitlines():
        fields = line.split()
        if len(fields) < 3 or int(fields[1]) != pgid:
            continue
        if live and fields[2].startswith("Z"):
            continue
        members.append(int(fields[0]))
    return sorted(members)
