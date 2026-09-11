# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Optional shared native hints. Periodic authoritative scans remain necessary."""

from pathlib import Path
from threading import Lock

from .domain import CatabolicError


class NativeMonitor:
    def __init__(self, roots, excluded=()):
        try:
            from watchdog.events import FileSystemEventHandler
            from watchdog.observers import Observer
        except ImportError as exc:
            raise CatabolicError("native monitoring requires catabolic[watch]") from exc
        self.roots = roots
        self.pending = set(roots)
        self.lock = Lock()
        self.observer = Observer()
        self.observer.event_queue.maxsize = 4096
        excluded = tuple(Path(p) for p in excluded)
        owner = self

        class Handler(FileSystemEventHandler):
            def on_any_event(self, event):
                if event.event_type not in (
                    "created",
                    "deleted",
                    "modified",
                    "moved",
                    "closed",
                ):
                    return
                paths = [Path(event.src_path)]
                if getattr(event, "dest_path", None):
                    paths.append(Path(event.dest_path))
                with owner.lock:
                    for source, root in owner.roots.items():
                        if any(
                            path.is_relative_to(Path(root))
                            and not any(
                                path.is_relative_to(exclusion) for exclusion in excluded
                            )
                            for path in paths
                        ):
                            owner.pending.add(source)

        self.handler = Handler()
        for root in sorted(set(roots.values())):
            try:
                self.observer.schedule(self.handler, root, recursive=True)
            except OSError:
                # Unavailable sources remain dirty and are polled by the scheduler.
                pass

    def start(self):
        self.observer.start()

    def drain(self):
        with self.lock:
            if self.observer.event_queue.full() or not self.observer.is_alive():
                self.pending.update(self.roots)
            pending, self.pending = self.pending, set()
            return sorted(pending)

    def close(self):
        self.observer.stop()
        self.observer.join(timeout=2)
