// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Current-grant, revision-pinned content admission and descriptor-owned reads.
use crate::{Error, Result, Store, access::Access, migration::rows, source};
use catabolic_core::{encode, sha256};
use rusqlite::params;
use serde_json::{Value, json};
use std::{
    fs::File,
    os::unix::fs::{FileExt, MetadataExt},
    path::PathBuf,
};

fn denied(code: &str) -> Error {
    Error(code.into())
}
impl Access {
    /// Reopen the current catalog snapshot for a new authorization boundary.
    /// A previous Access object is never proof of authorization after a wait.
    pub fn current(&self) -> Result<Self> {
        let store = Store::open(&self.store().path, false, false)?;
        if store.database_id != self.store().database_id {
            return Err(denied("unauthorized"));
        }
        let current = Self::from_stored_token(store, self.token_id())?;
        if current.principal() != self.principal() || current.profile() != self.profile() {
            return Err(denied("unauthorized"));
        }
        Ok(current)
    }
    pub fn fingerprint(&self) -> String {
        sha256(encode(&json!([self.principal(), self.token_id(), self.grants()])).as_bytes())
    }
    pub fn item(&mut self, id: &str, action: &str) -> Result<bool> {
        for grant in self.matching(action) {
            if grant["operator"] == true || self.members(&grant)?.contains(id) {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub fn component(&self, id: &str) -> bool {
        self.operator("metadata:read")
            || self.matching("component:read").iter().any(|g| {
                g["occurrence_ids"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v == id))
            })
    }
    pub fn file(&mut self, id: &str, action: &str, revision: Option<&str>) -> Result<bool> {
        for grant in self.matching(action) {
            if let Some(pinned) = grant
                .get("revisions")
                .and_then(|v| v.get(id))
                .filter(|v| !v.is_null())
            {
                let actual = match revision {
                    Some(v) => v.to_owned(),
                    None => revision_of(self.store(), self.profile(), id)?,
                };
                if pinned.as_str() != Some(actual.as_str()) {
                    continue;
                }
            }
            if grant["operator"] == true
                || grant["file_ids"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v == id))
            {
                return Ok(true);
            }
            if grant["derivatives"] == true {
                let items = self
                    .store()
                    .db
                    .prepare(
                        "SELECT source_item_id FROM media_outputs WHERE file_id=? AND profile=?",
                    )?
                    .query_map(params![id, self.profile()], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                for item in items {
                    if self.members(&grant)?.contains(&item) {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }
    pub fn metadata(&mut self, id: &str, value: &Value) -> Result<Value> {
        let mut result = serde_json::Map::new();
        for grant in self.matching("metadata:read") {
            if grant["operator"] == true {
                return Ok(value.clone());
            }
            if self.members(&grant)?.contains(id) {
                let default = json!(["title", "year"]);
                for key in grant
                    .get("metadata_fields")
                    .unwrap_or(&default)
                    .as_array()
                    .ok_or_else(|| denied("unauthorized"))?
                {
                    if let Some(key) = key.as_str()
                        && let Some(v) = value.get(key)
                    {
                        result.insert(key.into(), v.clone());
                    }
                }
            }
        }
        Ok(Value::Object(result))
    }
}
pub fn snapshot(store: &Store, profile: &str, id: &str) -> Result<Value> {
    let mut row=rows(&store.db,"SELECT f.id,f.location,f.path,o.size,o.mtime_ns,o.device,o.inode,o.status,b.root,b.device AS root_device,b.inode AS root_inode FROM main.files f LEFT JOIN main.observations o ON o.file_id=f.id AND o.profile=? LEFT JOIN main.bindings b ON b.profile=? AND b.kind='source' AND b.owner=f.location WHERE f.id=?",&[profile.to_owned().into(),profile.to_owned().into(),id.to_owned().into()])?.into_iter().next().ok_or_else(||denied("not_found"))?;
    let location = row["location"]
        .as_str()
        .ok_or_else(|| denied("not_found"))?
        .to_owned();
    row["source_policy"] = crate::observation::trust_policy(store, profile, &location)?;
    row["volume_uuid"] = rows(
        &store.db,
        "SELECT volume_uuid FROM binding_volumes WHERE profile=? AND kind='source' AND owner=?",
        &[profile.to_owned().into(), location.into()],
    )?
    .first()
    .map(|r| r["volume_uuid"].clone())
    .unwrap_or(Value::Null);
    Ok(row)
}
pub fn revision_of(store: &Store, profile: &str, id: &str) -> Result<String> {
    Ok(sha256(encode(&snapshot(store, profile, id)?).as_bytes()))
}
pub fn authorize(access: &mut Access, id: &str, revision: &str) -> Result<Value> {
    authorize_snapshot(&mut access.current()?, id, revision)
}
fn authorize_snapshot(access: &mut Access, id: &str, revision: &str) -> Result<Value> {
    if revision.is_empty() || !access.file(id, "content:read", Some(revision))? {
        return Err(denied("not_found"));
    }
    let row = snapshot(access.store(), access.profile(), id)?;
    if !access
        .store()
        .db
        .prepare("SELECT 1 FROM api_sources WHERE profile=? AND location=?")?
        .exists(params![access.profile(), row["location"].as_str()])?
    {
        return Err(denied("source_not_exposed"));
    }
    let filename = PathBuf::from(
        row["root"]
            .as_str()
            .filter(|v| !v.is_empty())
            .unwrap_or("/"),
    )
    .join(row["path"].as_str().ok_or_else(|| denied("not_found"))?);
    let database = &access.store().path;
    let database_sidecar = format!("{}-", database.display());
    let private = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|p| p.join(".config/catabolic/credentials"));
    if &filename == database
        || filename.to_string_lossy().starts_with(&database_sidecar)
        || private.is_some_and(|p| filename.starts_with(p))
        || access
            .store()
            .db
            .prepare("SELECT 1 FROM api_private_files WHERE path=? OR (device=? AND inode=?)")?
            .exists(params![
                filename.to_str(),
                row["device"].as_i64(),
                row["inode"].as_i64()
            ])?
    {
        return Err(denied("private_content"));
    }
    // Paths alone do not protect hardlinked database/WAL/SHM aliases. Match
    // current live identities as well as explicitly registered private files.
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let private = PathBuf::from(format!("{}{suffix}", database.display()));
        if private.metadata().is_ok_and(|m| {
            Some(m.dev()) == row["device"].as_u64() && Some(m.ino()) == row["inode"].as_u64()
        }) {
            return Err(denied("private_content"));
        }
    }
    for credential in access
        .store()
        .db
        .prepare(
            "SELECT credential_env FROM consumer_connections WHERE credential_env LIKE 'file:%'",
        )?
        .query_map([], |r| r.get::<_, String>(0))?
    {
        let credential = PathBuf::from(&credential?[5..]);
        if filename == credential
            || credential.metadata().is_ok_and(|m| {
                Some(m.dev()) == row["device"].as_u64() && Some(m.ino()) == row["inode"].as_u64()
            })
        {
            return Err(denied("private_content"));
        }
    }
    if revision != sha256(encode(&row).as_bytes()) {
        return Err(denied("revision_mismatch"));
    }
    if access
        .store()
        .db
        .prepare("SELECT 1 FROM processing_artifacts WHERE file_id=? AND state!='ready'")?
        .exists([id])?
    {
        return Err(denied("rendition_not_ready"));
    }
    Ok(row)
}
/// The descriptor cannot outlive the validated callback. Before/after identity,
/// source root, ctime and named-entry checks also run for successful transfers.
pub fn with_opened<T>(
    access: &mut Access,
    id: &str,
    revision: &str,
    operation: impl FnOnce(&File, &std::fs::Metadata) -> Result<T>,
) -> Result<T> {
    let row = authorize(access, id, revision)?;
    source::with_validated_source(&row, |file| {
        let mut header = [0u8; 16];
        if file.read_at(&mut header, 0)? == 16 && header == *b"SQLite format 3\0" {
            return Err(denied("private_content"));
        }
        operation(file, &file.metadata()?)
    })
}
/// RFC byte range behavior frozen by the Python API: a single bytes range;
/// unsupported units are ignored, suffixes clamp, and huge integers are bounded.
pub fn byte_range(value: Option<&str>, size: u64) -> Result<(u64, u64, bool)> {
    let Some(value) = value else {
        return Ok((0, size, false));
    };
    if !value
        .split('=')
        .next()
        .unwrap_or("")
        .eq_ignore_ascii_case("bytes")
    {
        return Ok((0, size, false));
    }
    let raw = value
        .get(6..)
        .ok_or_else(|| denied("range_not_satisfiable"))?;
    let (left, right) = raw
        .split_once('-')
        .ok_or_else(|| denied("range_not_satisfiable"))?;
    if !left.bytes().all(|b| b.is_ascii_digit())
        || !right.bytes().all(|b| b.is_ascii_digit())
        || size == 0
    {
        return Err(denied("range_not_satisfiable"));
    }
    let number = |v: &str| {
        v.bytes().fold(0u64, |n, b| {
            n.saturating_mul(10).saturating_add(u64::from(b - b'0'))
        })
    };
    let (start, end) = if !left.is_empty() {
        (
            number(left),
            if right.is_empty() {
                size - 1
            } else {
                number(right)
            },
        )
    } else {
        if right.is_empty() || number(right) == 0 {
            return Err(denied("range_not_satisfiable"));
        }
        (size.saturating_sub(number(right)), size - 1)
    };
    if start >= size || end < start {
        return Err(denied("range_not_satisfiable"));
    }
    Ok((start, end.min(size - 1) - start + 1, true))
}
/// Read one bounded chunk, checking inode/revision before every read.
pub fn read_chunk(
    file: &File,
    expected: &std::fs::Metadata,
    offset: u64,
    length: usize,
) -> Result<Vec<u8>> {
    let actual = file.metadata()?;
    let identity = |m: &std::fs::Metadata| {
        (
            m.dev(),
            m.ino(),
            m.size(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    };
    if identity(&actual) != identity(expected) {
        return Err(denied("content changed during transfer"));
    }
    let mut bytes = vec![0; length.min(65536)];
    let read = file.read_at(&mut bytes, offset)?;
    if identity(&file.metadata()?) != identity(expected) {
        return Err(denied("content changed during transfer"));
    }
    if read == 0 && length > 0 {
        return Err(denied("content truncated during transfer"));
    }
    bytes.truncate(read);
    Ok(bytes)
}

/// Create a bounded transfer capability tied to the original bearer token.
pub fn ticket(
    access: &mut Access,
    id: &str,
    revision: &str,
    ttl: u64,
    max_bytes: u64,
) -> Result<Value> {
    use base64::Engine;
    if !(1..=3600).contains(&ttl) || !(1..=107_374_182_400).contains(&max_bytes) {
        return Err(denied("invalid_ticket_configuration"));
    }
    authorize(access, id, revision)?;
    let mut random = [0u8; 44];
    getrandom::fill(&mut random).map_err(|e| Error(e.to_string()))?;
    let identifier = random[..12]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let secret = format!(
        "{identifier}.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&random[12..])
    );
    let now = crate::notifications::now();
    let expires = (now + ttl as f64).min(access.store().db.query_row(
        "SELECT expires FROM api_tokens WHERE id=?",
        [access.token_id()],
        |r| r.get::<_, f64>(0),
    )?);
    access.store().transaction(|db|{
        let count=db.query_row("SELECT count(*) FROM api_tickets WHERE token_id IN (SELECT id FROM api_tokens WHERE principal=?) AND expires>? AND revoked=0",params![access.principal(),now],|r|r.get::<_,i64>(0))?;
        if count>=256{return Err(denied("ticket_limit"));}
        db.execute("INSERT INTO api_tickets(id,digest,token_id,file_id,revision,expires,remaining_bytes) VALUES (?,?,?,?,?,?,?)",params![identifier,sha256(secret.as_bytes()),access.token_id(),id,revision,expires,max_bytes as i64])?;Ok(())
    })?;
    Ok(
        json!({"ticket_id":identifier,"expires":expires,"content_path":format!("/v1/files/{id}/content?revision={revision}&ticket={secret}")}),
    )
}
pub fn revoke_ticket(access: &Access, id: &str) -> Result<Value> {
    drop(access.current()?);
    access.store().transaction(|db|{db.execute("UPDATE api_tickets SET revoked=1 WHERE id=? AND token_id IN (SELECT id FROM api_tokens WHERE principal=?)",params![id,access.principal()])?;Ok(())})?;
    Ok(json!({"revoked":true}))
}
/// Authenticate the secret, then the still-live bearer token and current grants.
/// Content authorization and byte debit remain separate so HEAD/304 do not debit.
pub fn authenticate_ticket(
    store: Store,
    secret: &str,
    id: &str,
    revision: &str,
) -> Result<(Access, String)> {
    if secret.is_empty() || secret.len() > 256 {
        return Err(denied("unauthorized"));
    }
    let identifier = secret.split('.').next().unwrap_or("");
    let row = rows(
        &store.db,
        "SELECT * FROM api_tickets WHERE id=?",
        &[identifier.to_owned().into()],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| denied("unauthorized"))?;
    let expected = row["digest"]
        .as_str()
        .ok_or_else(|| denied("unauthorized"))?
        .as_bytes();
    let actual = sha256(secret.as_bytes());
    let mut difference = expected.len() ^ actual.len();
    for (a, b) in expected.iter().zip(actual.as_bytes()) {
        difference |= usize::from(a ^ b);
    }
    if difference != 0
        || row["expires"]
            .as_f64()
            .is_none_or(|v| !v.is_finite() || v <= crate::notifications::now())
        || row["revoked"] != 0
    {
        return Err(denied("unauthorized"));
    }
    let mut access = Access::from_stored_token(
        store,
        row["token_id"]
            .as_str()
            .ok_or_else(|| denied("unauthorized"))?,
    )?;
    if row["file_id"] != id || row["revision"] != revision {
        return Err(denied("not_found"));
    }
    authorize(&mut access, id, revision)?;
    Ok((access, identifier.to_owned()))
}
pub fn debit_ticket(access: &Access, id: &str, length: u64) -> Result<()> {
    let mut current = access.current()?;
    let ticket = rows(
        &access.store().db,
        "SELECT file_id,revision FROM api_tickets WHERE id=? AND token_id=?",
        &[id.to_owned().into(), access.token_id().to_owned().into()],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| denied("unauthorized"))?;
    authorize_snapshot(
        &mut current,
        ticket["file_id"]
            .as_str()
            .ok_or_else(|| denied("unauthorized"))?,
        ticket["revision"]
            .as_str()
            .ok_or_else(|| denied("unauthorized"))?,
    )?;
    // The owning writer lock prevents a competing admitted mutation. Release
    // the validation reader before COMMIT, avoiding SQLite reader/writer lockup.
    drop(current);
    let bytes = i64::try_from(length).map_err(|_| denied("ticket_transfer_limit"))?;
    access.store().transaction(|db|{
        if db.execute("UPDATE api_tickets SET remaining_bytes=remaining_bytes-? WHERE id=? AND token_id=? AND remaining_bytes>=? AND revoked=0 AND expires>?",params![bytes,id,access.token_id(),bytes,crate::notifications::now()])?!=1 {return Err(denied("ticket_transfer_limit"));}Ok(())
    })
}
pub fn execute(access: &mut Access, operation: &str, request: &Value) -> Result<Value> {
    let text = |key: &str| {
        request[key]
            .as_str()
            .ok_or_else(|| denied("invalid_content_request"))
    };
    let number = |key: &str, default: u64| {
        request
            .get(key)
            .map(|v| v.as_u64().ok_or_else(|| denied("invalid_content_request")))
            .transpose()
            .map(|v| v.unwrap_or(default))
    };
    match operation {
        "inspect" => {
            let id = text("file_id")?;
            let revision = text("revision")?;
            with_opened(access, id, revision, |_, stat| {
                Ok(json!({"file_id":id,"revision":revision,"size":stat.size()}))
            })
        }
        "ticket" => ticket(
            access,
            text("file_id")?,
            text("revision")?,
            number("ttl", 300)?,
            number("max_bytes", 1_073_741_824)?,
        ),
        "revoke-ticket" => revoke_ticket(access, text("ticket_id")?),
        _ => Err(denied("unknown content operation")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Access, String, String) {
        let root = tempfile::tempdir().unwrap();
        let media = root.path().join("media");
        std::fs::create_dir(&media).unwrap();
        std::fs::write(media.join("file.bin"), b"test bytes").unwrap();
        let path = root.path().join("catalog.sqlite3");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        crate::observation::execute(
            &store,
            "default",
            "bind",
            &json!({"source":"media","root":media.to_str().unwrap()}),
        )
        .unwrap();
        crate::observation::execute(&store, "default", "scan", &json!({"source":"media"})).unwrap();
        store.transaction(|db|{db.execute_batch("INSERT INTO api_principals(id,profile) VALUES ('test','default'); INSERT INTO api_grants(id,principal,definition) VALUES ('grant','test','{\"actions\":[\"*\"],\"operator\":true}'); INSERT INTO api_sources VALUES ('default','media');")?;db.execute("INSERT INTO api_tokens(id,principal,digest,grants,expires) VALUES ('token','test',?,'[\"grant\"]',?)",params![sha256("token.secret"),crate::notifications::now()+1000.0])?;Ok(())}).unwrap();
        let id = store
            .db
            .query_row("SELECT id FROM files WHERE path='file.bin'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap();
        let revision = revision_of(&store, "default", &id).unwrap();
        (
            root,
            Access::authenticate(store, "token.secret").unwrap(),
            id,
            revision,
        )
    }
    #[test]
    fn mutation_boundaries_recheck_current_authorization() {
        for update in [
            "UPDATE api_tokens SET revoked=1",
            "UPDATE api_tokens SET expires=0",
            "UPDATE api_principals SET enabled=0",
            "UPDATE api_grants SET enabled=0",
        ] {
            let (_root, mut access, id, revision) = fixture();
            let created = ticket(&mut access, &id, &revision, 300, 100).unwrap();
            access.store().db.execute_batch(update).unwrap();
            assert!(ticket(&mut access, &id, &revision, 300, 100).is_err());
            assert!(debit_ticket(&access, created["ticket_id"].as_str().unwrap(), 1).is_err());
            assert!(with_opened(&mut access, &id, &revision, |_, _| Ok(())).is_err());
            assert_eq!(
                access
                    .store()
                    .db
                    .query_row("SELECT remaining_bytes FROM api_tickets", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                100
            );
        }
    }
    #[test]
    fn chunk_rejects_changed_revision_without_returning_bytes() {
        let (root, mut access, id, revision) = fixture();
        with_opened(&mut access, &id, &revision, |file, metadata| {
            std::fs::write(root.path().join("media/file.bin"), b"new private bytes")?;
            assert!(read_chunk(file, metadata, 0, 10).is_err());
            Ok(())
        })
        .unwrap_err();
    }
    #[test]
    fn ranges_are_bounded_and_single() {
        for (raw, want) in [
            (None, (0, 10, false)),
            (Some("items=1-2"), (0, 10, false)),
            (Some("bytes=1-2"), (1, 2, true)),
            (Some("BYTES=-3"), (7, 3, true)),
            (Some("bytes=3-"), (3, 7, true)),
            (Some("bytes=-9999999999999999999999999"), (0, 10, true)),
            (Some("bytes=0-9999999999999999999999999"), (0, 10, true)),
        ] {
            assert_eq!(byte_range(raw, 10).unwrap(), want);
        }
        for raw in [
            "bytes=-",
            "bytes=-0",
            "bytes=10-",
            "bytes=3-1",
            "bytes=0-1,2-3",
            "bytes= 1-2",
            "bytes=9999999999999999999999999-",
        ] {
            assert_eq!(
                byte_range(Some(raw), 10).unwrap_err().0,
                "range_not_satisfiable"
            );
        }
        assert_eq!(byte_range(None, 0).unwrap(), (0, 0, false));
        assert!(byte_range(Some("bytes=0-"), 0).is_err());
    }
}
