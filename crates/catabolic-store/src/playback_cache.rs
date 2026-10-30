// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Descriptor-pinned temporary playback cache and closed-segment playlists.
#![allow(clippy::useless_conversion)] // mode_t is u16 on macOS, u32 on Linux.
use crate::{Error, Result, Store, artifact_mutation, filesystem_mutation as fs, migration::rows};
use serde_json::Value;
use std::{
    ffi::CString,
    fs::File,
    io::Read,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
};
const MAX_PLAYLIST: u64 = 4 * 1024 * 1024;
pub fn segment(name: &str) -> bool {
    name.len() == 17
        && name.starts_with("segment-")
        && name.ends_with(".ts")
        && name.as_bytes()[8..14].iter().all(u8::is_ascii_digit)
}
pub fn directory_name(job: &Value) -> Result<String> {
    let id = job["id"]
        .as_str()
        .ok_or_else(|| Error("invalid_playback_cache".into()))?;
    if id.len() != 36 || uuid::Uuid::parse_str(id).is_err() {
        return Err(Error("invalid_playback_cache".into()));
    }
    Ok(format!(".playback-{id}"))
}
pub fn destination_root(store: &Store, profile: &str, binding: &Value) -> Result<File> {
    artifact_mutation::owned_root(store, profile, binding)
        .map_err(|_| Error("playback_destination_changed".into()))
}
pub fn job_directory(store: &Store, job: &Value) -> Result<File> {
    let binding = catabolic_core::decode_json(
        job["destination"]
            .as_str()
            .ok_or_else(|| Error("invalid_playback_cache".into()))?,
    )?;
    let root = destination_root(
        store,
        job["profile"]
            .as_str()
            .ok_or_else(|| Error("invalid_playback_cache".into()))?,
        &binding,
    )?;
    let fd = fs::open_at(&root, &directory_name(job)?, true)?;
    let st = fd.metadata()?;
    if Some(st.dev()) != job["directory_device"].as_u64()
        || Some(st.ino()) != job["directory_inode"].as_u64()
    {
        return Err(Error("playback_cache_changed".into()));
    }
    Ok(fd)
}
fn read_file(directory: &File, name: &str, maximum: u64) -> Result<Vec<u8>> {
    let file = fs::open_at(directory, name, false)?;
    let st = file.metadata()?;
    if !st.is_file() || st.len() > maximum {
        return Err(Error("invalid_playback_cache".into()));
    }
    let mut bytes = vec![];
    file.take(maximum + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != st.len() {
        return Err(Error("invalid_playback_cache".into()));
    }
    Ok(bytes)
}
#[derive(Debug)]
pub struct Playlist {
    pub raw: Option<String>,
    pub segments: Vec<String>,
    pub seconds: f64,
}
pub fn playlist(directory: &File) -> Result<Playlist> {
    let raw = match read_file(directory, "index.m3u8", MAX_PLAYLIST) {
        Ok(v) => String::from_utf8(v).map_err(|_| Error("invalid_playback_cache".into()))?,
        Err(e) if e.0.contains("No such file or directory") => {
            return Ok(Playlist {
                raw: None,
                segments: vec![],
                seconds: 0.0,
            });
        }
        Err(e) => return Err(e),
    };
    parse_playlist(raw)
}
pub fn parse_playlist(raw: String) -> Result<Playlist> {
    let fail = || Error("invalid_playback_cache".into());
    if !raw.starts_with("#EXTM3U\n") {
        return Err(fail());
    }
    let (mut segments, mut seconds, mut pending) = (vec![], 0.0, None);
    let mut seen = std::collections::BTreeSet::new();
    for line in raw.lines() {
        if let Some(duration) = line.strip_prefix("#EXTINF:") {
            let value = duration
                .split(',')
                .next()
                .unwrap_or("")
                .parse::<f64>()
                .map_err(|_| fail())?;
            if !value.is_finite() || value <= 0.0 {
                return Err(fail());
            }
            pending = Some(value);
        } else if !line.is_empty() && !line.starts_with('#') {
            if !segment(line) || pending.is_none() || !seen.insert(line.to_owned()) {
                return Err(fail());
            }
            segments.push(line.to_owned());
            seconds += pending.take().unwrap();
        } else if line.starts_with('#') {
            let known = [
                "#EXTM3U",
                "#EXT-X-PLAYLIST-TYPE:EVENT",
                "#EXT-X-INDEPENDENT-SEGMENTS",
                "#EXT-X-ENDLIST",
            ]
            .contains(&line);
            let number = [
                "#EXT-X-VERSION:",
                "#EXT-X-TARGETDURATION:",
                "#EXT-X-MEDIA-SEQUENCE:",
            ]
            .iter()
            .any(|prefix| {
                line.strip_prefix(prefix)
                    .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
            });
            if !known && !number {
                return Err(fail());
            }
        }
    }
    if pending.is_some() || !seconds.is_finite() {
        return Err(fail());
    }
    Ok(Playlist {
        raw: Some(raw),
        segments,
        seconds,
    })
}
pub fn cache_size(directory: &File) -> Result<u64> {
    let mut size = 0u64;
    fs::entries(directory, |name| {
        if !["index.m3u8", "index.m3u8.tmp"].contains(&name)
            && !segment(name)
            && !name.strip_suffix(".tmp").is_some_and(segment)
        {
            return Err(Error("invalid_playback_cache".into()));
        }
        let (mode, st) = match fs::stat_at(directory, name) {
            Ok(v) => v,
            Err(e) if e.0.contains("No such file or directory") => return Ok(true),
            Err(e) => return Err(e),
        };
        if mode != u32::from(libc::S_IFREG) {
            return Err(Error("invalid_playback_cache".into()));
        }
        size = size
            .checked_add(
                st["size"]
                    .as_u64()
                    .ok_or_else(|| Error("invalid_playback_cache".into()))?,
            )
            .ok_or_else(|| Error("invalid_playback_cache".into()))?;
        Ok(true)
    })?;
    Ok(size)
}
pub fn open_segment(directory: &File, name: &str, playlist: &Playlist) -> Result<File> {
    if !segment(name) || !playlist.segments.iter().any(|v| v == name) {
        return Err(Error("not_found".into()));
    }
    let file = fs::open_at(directory, name, false)?;
    let stat = file.metadata()?;
    if !stat.is_file() || stat.len() == 0 {
        return Err(Error("invalid_playback_cache".into()));
    }
    Ok(file)
}
pub fn create_directory(root: &File, job: &Value) -> Result<File> {
    let name = directory_name(job)?;
    let raw = CString::new(name.clone()).map_err(|_| Error("invalid_playback_cache".into()))?;
    // SAFETY: live root descriptor and NUL-terminated generated basename.
    if unsafe { libc::mkdirat(root.as_raw_fd(), raw.as_ptr(), 0o700) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    fs::open_at(root, &name, true)
}
pub fn reserved_bytes(store: &Store, device: u64) -> Result<i64> {
    Ok(store.db.query_row("SELECT (SELECT coalesce(sum(bytes),0) FROM api_reservations WHERE device=?)+(SELECT coalesce(sum(reserved_bytes),0) FROM api_playback_jobs WHERE reservation_device=? AND state IN ('queued','running'))",[device as i64,device as i64],|r|r.get(0))?)
}
pub fn available_bytes(root: &File) -> Result<u128> {
    // SAFETY: zeroed output struct is initialized by fstatvfs on success.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: live descriptor and valid output buffer.
    if unsafe { libc::fstatvfs(root.as_raw_fd(), &mut stat) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(u128::from(stat.f_bavail) * u128::from(stat.f_frsize))
}
pub fn cleanup(store: &Store, profile: &str) -> Result<()> {
    cleanup_inner(
        store,
        profile,
        #[cfg(test)]
        |_, _| Ok(()),
    )
}
fn cleanup_inner(
    store: &Store,
    profile: &str,
    #[cfg(test)] mut before_recheck: impl FnMut(&Store, &Value) -> Result<()>,
) -> Result<()> {
    if !store.writable || !store.owns_writer_lock() {
        return Err(Error(
            "playback cleanup requires a writable catalog session".into(),
        ));
    }
    if !store.db.is_autocommit() {
        return Err(Error(
            "playback cleanup requires a standalone transaction boundary".into(),
        ));
    }
    let now = crate::notifications::now();
    let jobs = rows(
        &store.db,
        "SELECT j.* FROM api_playback_jobs j WHERE j.profile=? AND j.state!='running' AND (j.state!='stale' OR j.directory_inode IS NOT NULL) AND NOT EXISTS(SELECT 1 FROM api_playback_sessions s WHERE s.job_id=j.id AND s.cancelled=0 AND s.expires>?) AND (j.state!='ready' OR j.updated_at<?) ORDER BY j.created_at LIMIT 20",
        &[profile.to_owned().into(), now.into(), (now - 3600.0).into()],
    )?;
    for selected in jobs {
        #[cfg(test)]
        before_recheck(store, &selected)?;
        // Re-read the candidate and live session demand immediately before
        // filesystem work; the initial query may no longer describe it.
        let now = crate::notifications::now();
        let current = rows(
            &store.db,
            "SELECT j.*, EXISTS(SELECT 1 FROM api_playback_sessions s WHERE s.job_id=j.id AND s.cancelled=0 AND s.expires>?) AS active_session FROM api_playback_jobs j WHERE j.id=? AND j.profile=?",
            &[
                now.into(),
                selected["id"]
                    .as_str()
                    .ok_or_else(|| Error("invalid_playback_cache".into()))?
                    .to_owned()
                    .into(),
                profile.to_owned().into(),
            ],
        )?;
        let Some(job) = current.into_iter().next() else {
            continue;
        };
        if !catabolic_core::playback_admission::cache_retirement_allowed(
            job["state"].as_str().unwrap_or("running"),
            !job["directory_inode"].is_null(),
            job["active_session"].as_i64() != Some(0),
            job["updated_at"].as_f64().unwrap_or(now),
            now,
        ) {
            continue;
        }
        let retired: Result<()> = (|| {
            if !job["directory_inode"].is_null() {
                let dir = job_directory(store, &job)?;
                cache_size(&dir)?;
                let mut names = vec![];
                fs::entries(&dir, |name| {
                    names.push(name.to_owned());
                    Ok(true)
                })?;
                for name in names {
                    // Recheck type immediately before unlink; foreign names or
                    // links are never followed. Only cache-owned names qualify.
                    let (mode, _) = fs::stat_at(&dir, &name)?;
                    if mode != u32::from(libc::S_IFREG) {
                        return Err(Error("invalid_playback_cache".into()));
                    }
                    let name =
                        CString::new(name).map_err(|_| Error("invalid_playback_cache".into()))?;
                    // SAFETY: owned directory and checked generated basename.
                    if unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                        return Err(std::io::Error::last_os_error().into());
                    }
                }
                let binding = catabolic_core::decode_json(
                    job["destination"]
                        .as_str()
                        .ok_or_else(|| Error("invalid_playback_cache".into()))?,
                )?;
                let root = destination_root(store, profile, &binding)?;
                let name = directory_name(&job)?;
                let (_, st) = fs::stat_at(&root, &name)?;
                if st["device"] != job["directory_device"] || st["inode"] != job["directory_inode"]
                {
                    return Err(Error("playback_cache_changed".into()));
                }
                let name =
                    CString::new(name).map_err(|_| Error("invalid_playback_cache".into()))?;
                // SAFETY: owned root and identity-checked empty cache directory.
                if unsafe { libc::unlinkat(root.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) }
                    != 0
                {
                    return Err(std::io::Error::last_os_error().into());
                }
            }
            Ok(())
        })();
        if retired
            .as_ref()
            .is_err_and(|e| !e.0.contains("No such file or directory"))
        {
            continue;
        }
        store.transaction(|db|{db.execute("UPDATE api_playback_jobs SET state='stale',directory_inode=NULL,directory_device=NULL,reserved_bytes=0 WHERE id=?",[job["id"].as_str()])?;Ok(())})?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readonly_cleanup_preserves_expired_owned_cache_and_catalog() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.sqlite3");
        let generated = root.path().join("generated");
        std::fs::create_dir(&generated).unwrap();
        let source = root.path().join("source.bin");
        std::fs::write(&source, b"source bytes").unwrap();
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let binding =
            artifact_mutation::bind(&store, "default", "generated", generated.to_str().unwrap())
                .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let root_fd = destination_root(&store, "default", &binding).unwrap();
        let job = serde_json::json!({"id":id,"profile":"default","destination":catabolic_core::encode(&binding)});
        let directory = create_directory(&root_fd, &job).unwrap();
        let st = directory.metadata().unwrap();
        let cache = generated.join(directory_name(&job).unwrap());
        let manifest = "#EXTM3U\n#EXTINF:1,\nsegment-000000.ts\n#EXT-X-ENDLIST\n";
        std::fs::write(cache.join("index.m3u8"), manifest).unwrap();
        std::fs::write(cache.join("segment-000000.ts"), b"closed segment bytes").unwrap();
        store.transaction(|db| {db.execute("INSERT INTO api_playback_jobs(id,profile,cache_key,body,snapshot,recipe,tools,destination,state,reserved_bytes,reservation_device,directory_device,directory_inode,created_at,updated_at) VALUES (?,'default',?,'{}','{}','{}','{}',?,'ready',0,?,?,?,0,0)",rusqlite::params![id,id,catabolic_core::encode(&binding),st.dev() as i64,st.dev() as i64,st.ino() as i64])?;Ok(())}).unwrap();
        // Also reject a surrounding transaction: caller rollback must not undo
        // only the tombstone after filesystem deletion already took effect.
        store
            .transaction(|_| {
                assert!(
                    cleanup(&store, "default")
                        .unwrap_err()
                        .0
                        .contains("standalone transaction boundary")
                );
                Ok(())
            })
            .unwrap();
        store.transaction(|db| {
            db.execute("INSERT INTO api_principals(id,profile) VALUES ('p','default')", [])?;
            db.execute("INSERT INTO api_tokens(id,principal,digest,grants,expires) VALUES ('t','p',?,'[]',?)", rusqlite::params![catabolic_core::sha256("t.secret"), 4_000_000_000.0])?;
            Ok(())
        }).unwrap();
        cleanup_inner(&store, "default", |store, selected| {
            assert_eq!(selected["id"], id);
            store.transaction(|db| {
                db.execute("INSERT INTO api_playback_sessions(id,profile,principal,token_id,idempotency_key,body,job_id,created_at,expires) VALUES ('s','default','p','t','key','{}',?,0,?)", rusqlite::params![id, 4_000_000_000.0])?;
                Ok(())
            })
        }).unwrap();
        assert!(cache.join("index.m3u8").exists());
        assert_eq!(crate::playback::job(&store, &id).unwrap()["state"], "ready");
        store
            .transaction(|db| {
                db.execute("DELETE FROM api_playback_sessions WHERE id='s'", [])?;
                Ok(())
            })
            .unwrap();
        drop(store);
        let before = std::fs::read(&path).unwrap();
        let readonly = Store::open(&path, false, false).unwrap();
        assert!(
            cleanup(&readonly, "default")
                .unwrap_err()
                .0
                .contains("writable catalog session")
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            std::fs::read(cache.join("index.m3u8")).unwrap(),
            manifest.as_bytes()
        );
        assert_eq!(
            std::fs::read(cache.join("segment-000000.ts")).unwrap(),
            b"closed segment bytes"
        );
        assert_eq!(std::fs::read(&source).unwrap(), b"source bytes");
        assert_eq!(
            crate::playback::job(&readonly, &id).unwrap()["state"],
            "ready"
        );
    }
    #[test]
    fn job_directory_is_a_generated_basename() {
        for id in ["../../source", "../secret", "x/y", "", "not-a-uuid"] {
            assert!(directory_name(&serde_json::json!({"id":id})).is_err());
        }
        let id = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            directory_name(&serde_json::json!({"id":id})).unwrap(),
            format!(".playback-{id}")
        );
    }
    #[test]
    fn playlist_allowlist() {
        let p=parse_playlist("#EXTM3U\n#EXT-X-PLAYLIST-TYPE:EVENT\n#EXTINF:2.4,\nsegment-000000.ts\n#EXT-X-ENDLIST\n".into()).unwrap();
        assert_eq!(p.seconds, 2.4);
        assert_eq!(p.segments, ["segment-000000.ts"]);
        for raw in [
            "#EXTM3U\n#EXTINF:nan,\nsegment-000000.ts\n",
            "#EXTM3U\n#EXTINF:2,\n../secret\n",
            "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=http://evil\n",
            "#EXTM3U\n#EXTINF:2,\nsegment-000000.ts\n#EXTINF:1,\nsegment-000000.ts\n",
            "#EXTM3U\n#EXTINF:1e308,\nsegment-000000.ts\n#EXTINF:1e308,\nsegment-000001.ts\n",
        ] {
            assert!(parse_playlist(raw.into()).is_err());
        }
    }
}
