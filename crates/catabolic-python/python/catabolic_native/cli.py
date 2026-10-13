"""Opt-in returning callable for the native legacy CLI.

The packaged Rust executable owns parsing, dispatch, rendering and exit semantics.
Python only adapts argv and text streams, and owns the subprocess lifetime.
"""

import hashlib
import json
import os
import signal
import sqlite3
import stat
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

from . import CatabolicError, __version__

_MAX_FRAME = 1024 * 1024
_ARGUMENT = "__catabolic-python-cli-v1"


def legacy_binary():
    """Verify the packaged legacy helper; never search PATH or fall back."""
    root = Path(__file__).resolve().parent
    executable = root / "bin/catabolic-legacy-native"
    try:
        manifests = []
        for name, path in (
            ("legacy-helper.json", "bin/catabolic-legacy-native"),
            ("native-helper.json", "bin/catabolic-native"),
        ):
            file = root / name
            if file.is_symlink() or not file.is_file() or file.stat().st_size > 4096:
                raise ValueError("missing or invalid manifest")
            metadata = json.loads(file.read_text(encoding="utf-8"))
            if (
                not isinstance(metadata, dict)
                or metadata.get("format") != 1
                or metadata.get("version") != __version__
                or metadata.get("path") != path
                or not isinstance(metadata.get("build"), dict)
                or not all(
                    isinstance(metadata.get(key), str)
                    and len(metadata[key]) == 64
                    and all(c in "0123456789abcdef" for c in metadata[key])
                    for key in ("sha256", "source_sha256")
                )
            ):
                raise ValueError("invalid manifest")
            manifests.append(metadata)
        legacy, companion = manifests
        if any(legacy[key] != companion[key] for key in ("source_sha256", "build")):
            raise ValueError("legacy and companion source provenance differ")
        before = executable.lstat()
        if (
            not stat.S_ISREG(before.st_mode)
            or not before.st_mode & 0o111
            or not os.access(executable, os.X_OK)
            or before.st_size > 128 * 1024 * 1024
        ):
            raise ValueError("helper is not an executable regular file")
        with executable.open("rb") as stream:
            actual = hashlib.file_digest(stream, "sha256").hexdigest()

        def identity(info):
            return (
                info.st_dev,
                info.st_ino,
                info.st_mode,
                info.st_size,
                info.st_mtime_ns,
                info.st_ctime_ns,
            )

        if actual != legacy["sha256"] or identity(executable.lstat()) != identity(
            before
        ):
            raise ValueError("helper digest or identity changed")
    except (OSError, ValueError, TypeError, KeyError) as error:
        raise RuntimeError(
            f"unavailable or mismatched packaged legacy helper: {error}"
        ) from error
    return executable


def _send(stream, value):
    raw = json.dumps(value, ensure_ascii=True, allow_nan=False).encode("ascii")
    if not raw or len(raw) > _MAX_FRAME:
        raise RuntimeError("native CLI transport frame exceeds its bound")
    stream.write(struct.pack("!I", len(raw)))
    stream.write(raw)
    stream.flush()


def _read_exact(stream, size):
    chunks = bytearray()
    while len(chunks) < size:
        data = stream.read(size - len(chunks))
        if not data:
            raise RuntimeError("native CLI transport closed before its result")
        chunks.extend(data)
    return chunks


def _receive(stream):
    size = struct.unpack("!I", _read_exact(stream, 4))[0]
    if not 0 < size <= _MAX_FRAME:
        raise RuntimeError("invalid native CLI transport frame size")
    value = json.loads(_read_exact(stream, size))
    if not isinstance(value, dict):
        raise RuntimeError("invalid native CLI transport frame")
    return value


def _stop(child):
    """Join cancellation first; always reap, including callback failures."""
    if child.poll() is None:
        for sig, timeout in (
            (signal.SIGINT, 2),
            (signal.SIGTERM, 2),
            (signal.SIGKILL, None),
        ):
            try:
                child.send_signal(sig)
                try:
                    child.stdin.close()
                except OSError:
                    pass
                child.wait(timeout=timeout)
                break
            except subprocess.TimeoutExpired:
                continue
            except KeyboardInterrupt:
                # A repeated interrupt must not abandon the process owner.
                continue
    child.wait()
    for stream in (child.stdin, child.stdout):
        stream.close()


def _invoke(arguments, state, *, error=None, context=None):
    state["handling_error"] = error is not None
    executable = legacy_binary()
    # A private diagnostic file avoids a second pipe deadlock and keeps ordered
    # application output exclusively on the acknowledged protocol channel.
    with tempfile.TemporaryFile() as diagnostic:
        child = subprocess.Popen(
            [str(executable), _ARGUMENT],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=diagnostic,
            start_new_session=True,
        )
        interrupted = None
        result = None
        state["callback"] = False
        try:
            request = {"kind": "argv", "version": 1, "argv": arguments}
            if error is not None:
                request = {
                    "kind": "render-error",
                    "version": 1,
                    "context": context,
                    "error": error,
                }
            _send(child.stdin, request)
            while result is None:
                frame = _receive(child.stdout)
                kind = frame.get("kind")
                if kind == "dispatch":
                    if interrupted is not None:
                        raise RuntimeError("duplicate native CLI dispatch frame")
                    interrupted = frame["interrupted"]
                    state["context"] = frame["context"]
                elif kind == "handling-error":
                    if interrupted is None:
                        raise RuntimeError("native CLI error phase preceded dispatch")
                    state["handling_error"] = True
                elif kind in ("write", "flush"):
                    name = frame.get("stream")
                    if name not in ("stdout", "stderr"):
                        raise RuntimeError("invalid native CLI output stream")
                    stream = getattr(sys, name)
                    state["callback"] = True
                    try:
                        if kind == "write":
                            stream.write(frame["text"])
                        else:
                            stream.flush()
                    except (
                        CatabolicError,
                        OSError,
                        sqlite3.Error,
                        ValueError,
                    ) as failure:
                        if frame.get("owner_catches") is not True:
                            raise
                        state["callback"] = False
                        _send(
                            child.stdin,
                            {
                                "kind": "callback-error",
                                "error": {
                                    "message": str(failure),
                                    "type": type(failure).__name__,
                                },
                            },
                        )
                        continue
                    state["callback"] = False
                elif kind == "read":
                    size = frame.get("size")
                    if type(size) is not int or not 0 < size <= 128 * 1024 * 1024 + 1:
                        raise RuntimeError("invalid native CLI stdin request")
                    state["callback"] = True
                    value = sys.stdin.read(size)
                    state["callback"] = False
                    if not isinstance(value, str):
                        raise TypeError("CLI stdin must return text")
                    if len(value) > size:
                        raise RuntimeError(
                            "CLI stdin returned more than its requested bound"
                        )
                    for start in range(0, len(value), 16384):
                        _send(
                            child.stdin,
                            {"kind": "stdin", "text": value[start : start + 16384]},
                        )
                    _send(child.stdin, {"kind": "stdin-end"})
                    continue
                elif kind in ("return", "system-exit"):
                    code = frame.get("code")
                    if (
                        type(code) is not int
                        or not 0 <= code <= 255
                        or ((kind == "return") != (interrupted is not None))
                    ):
                        raise RuntimeError("invalid native CLI result")
                    result = (kind, code)
                    continue
                else:
                    raise RuntimeError("unknown native CLI transport frame")
                _send(child.stdin, {"kind": "ack"})
            if child.wait(timeout=5) != 0:
                raise RuntimeError("native CLI transport failed after its result")
        except KeyboardInterrupt:
            if interrupted is None or state.get("handling_error"):
                raise
            state["handling_error"] = True
            # The native renderer supplied this before dispatch. Cancel and reap
            # before delivering it; no Python parser/envelope replica is needed.
            _stop(child)
            if interrupted["stdout"]:
                sys.stdout.write(interrupted["stdout"])
            if interrupted["stderr"]:
                sys.stderr.write(interrupted["stderr"])
            return interrupted["code"]
        finally:
            _stop(child)
        if result[0] == "system-exit":
            raise SystemExit(result[1])
        return result[1]


def main(argv: list[str] | None = None) -> int:
    """Return native dispatch status, or raise SystemExit for parser help/errors.

    Native command gaps remain explicit; this never invokes the Python engine.
    """
    arguments = list(sys.argv[1:] if argv is None else argv)
    if not all(isinstance(argument, str) for argument in arguments):
        raise TypeError("CLI arguments must be strings")
    state = {}
    try:
        return _invoke(arguments, state)
    except (CatabolicError, OSError, sqlite3.Error, ValueError) as error:
        if (
            not state.get("callback")
            or "context" not in state
            or state.get("handling_error")
        ):
            raise
        # Frozen main catches these text-stream errors after parsing. Render
        # them through Rust only after the failed child has been reaped. Errors
        # raised while reporting the failure propagate unchanged to the caller.
        return _invoke(
            arguments,
            {},
            context=state["context"],
            error={"message": str(error), "type": type(error).__name__},
        )
