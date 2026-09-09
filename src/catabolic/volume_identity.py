# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Stable filesystem UUID evidence, obtained from an already-open root."""

import ctypes
import json
import os
import sys
from uuid import UUID

from .domain import CatabolicError
from .process_runner import CommandFailure, command_output


def volume_uuid(fd):
    if sys.platform == "darwin":
        # macOS SDK sys/attr.h: ATTR_VOL_INFO | ATTR_VOL_UUID, ATTR_BIT_MAP_COUNT=5.
        class Attributes(ctypes.Structure):
            _fields_ = [
                ("count", ctypes.c_uint16),
                ("reserved", ctypes.c_uint16),
                ("common", ctypes.c_uint32),
                ("volume", ctypes.c_uint32),
                ("directory", ctypes.c_uint32),
                ("file", ctypes.c_uint32),
                ("fork", ctypes.c_uint32),
            ]

        function = ctypes.CDLL(None, use_errno=True).fgetattrlist
        function.argtypes = [
            ctypes.c_int,
            ctypes.POINTER(Attributes),
            ctypes.c_void_p,
            ctypes.c_size_t,
            ctypes.c_uint32,
        ]
        function.restype = ctypes.c_int
        attributes = Attributes(5, 0, 0, 0x80040000, 0, 0, 0)
        result = ctypes.create_string_buffer(20)
        if function(fd, ctypes.byref(attributes), result, 20, 0) != 0:
            return None
        identifier = UUID(bytes=result.raw[4:20])
        return str(identifier) if identifier.int else None
    if sys.platform.startswith("linux"):
        try:
            root = os.readlink(f"/proc/self/fd/{fd}")
            value = json.loads(
                command_output(
                    ["findmnt", "--json", "--target", root, "--output", "UUID"],
                    timeout=5,
                    maximum=65536,
                )
            )
            rows = value.get("filesystems", [])
            if len(rows) == 1 and rows[0].get("uuid"):
                return str(rows[0]["uuid"]).lower()
        except (OSError, ValueError, CommandFailure, CatabolicError):
            pass
    return None
