# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Disposable disk staging: inventory size does not determine Python heap size."""

import sqlite3


class ScanStaging:
    def __enter__(self):
        self.db = sqlite3.connect("")
        self.db.row_factory = sqlite3.Row
        self.db.execute("PRAGMA cache_size=-2048")
        self.db.execute("PRAGMA journal_mode=OFF")
        self.db.execute(
            "CREATE TABLE staged(path TEXT,size INTEGER,mtime_ns INTEGER,device INTEGER,inode INTEGER)"
        )
        self.buffer = []
        self.count = 0
        return self

    def append(self, entry):
        self.buffer.append(
            tuple(entry[k] for k in ("path", "size", "mtime_ns", "device", "inode"))
        )
        self.count += 1
        if len(self.buffer) >= 500:
            self.flush()

    def flush(self):
        if self.buffer:
            self.db.executemany("INSERT INTO staged VALUES (?,?,?,?,?)", self.buffer)
            self.db.commit()
            self.buffer.clear()

    def __len__(self):
        return self.count

    def __iter__(self):
        self.flush()
        yield from self.db.execute("SELECT * FROM staged")

    def __exit__(self, *_):
        self.db.close()
