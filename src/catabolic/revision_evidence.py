# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Recorded revision comparisons shared by workflow and query evidence."""


def _snapshot_matches(snapshot, observation, binding):
    return (
        " AND ".join(
            f"json_extract({snapshot},'$.{field}') IS {observation}.{column}"
            for field, column in (
                ("size", "size"),
                ("mtime_ns", "mtime_ns"),
                ("device", "device"),
                ("inode", "inode"),
            )
        )
        + f" AND json_extract({snapshot},'$.root') IS {binding}.root AND json_extract({snapshot},'$.root_device') IS {binding}.device AND json_extract({snapshot},'$.root_inode') IS {binding}.inode"
    )
