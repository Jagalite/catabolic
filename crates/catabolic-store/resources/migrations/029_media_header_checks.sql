CREATE TABLE media_header_checks(
 profile TEXT NOT NULL REFERENCES profiles(id),
 file_id TEXT NOT NULL REFERENCES files(id),
 scan_id TEXT NOT NULL REFERENCES scans(id),
 size INTEGER NOT NULL, mtime_ns INTEGER NOT NULL,
 device INTEGER NOT NULL, inode INTEGER NOT NULL,
 status TEXT NOT NULL CHECK(status IN ('invalid','not_detected','unknown')),
 reason TEXT, bytes_read INTEGER NOT NULL,
 PRIMARY KEY(profile,file_id)
);
CREATE INDEX media_header_scan ON media_header_checks(scan_id,status);
