// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Current-grant, revision-pinned content admission and descriptor-owned reads.
use crate::{Error, Result, Store, access::Access, migration::rows, source};
use catabolic_core::{encode, sha256};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    fs::File,
    os::unix::fs::{FileExt, MetadataExt},
    path::PathBuf,
};

fn denied(code: &str) -> Error {
    Error(code.into())
}
/// Conservatively refuse recognizable SQLite payloads even after their original
/// private pathname has disappeared. This is not proof that other bytes are
/// public: damaged, zeroed, or unfamiliar sidecar headers remain unclassified,
/// which is why `sqlite_sidecar_name` also refuses by name (FS4).
fn refuse_sqlite_payload(file: &File) -> Result<()> {
    let mut header = [0u8; 16];
    let length = file.read_at(&mut header, 0)?;
    let bytes = &header[..length];
    // SQLite WAL_MAGIC is big-endian; WalIndexHdr.iVersion is native-endian.
    // Both wal-index byte orders are private, including copied foreign files.
    // A rollback journal starts with the eight-byte aJournalMagic once its
    // header is synced (pager.c in the pinned libsqlite3-sys 0.38.2).
    if bytes == b"SQLite format 3\0"
        || bytes.starts_with(&0x377f0682u32.to_be_bytes())
        || bytes.starts_with(&0x377f0683u32.to_be_bytes())
        || bytes.starts_with(&3_007_000u32.to_le_bytes())
        || bytes.starts_with(&3_007_000u32.to_be_bytes())
        || bytes.starts_with(&[0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7])
    {
        return Err(denied("private_content"));
    }
    Ok(())
}
/// SQLite names every sidecar after its database: `-wal`, `-shm`, `-journal`
/// and the super-journal `-mj` followed by hex digits. The pinned SQLite 3.53.2
/// (libsqlite3-sys 0.38.2) writes `-mj%06X9%02X`, nine digits such as
/// `-mj1234569AB`; older releases wrote eight. An orphaned sidecar
/// keeps that name after its database is renamed, moved or deleted, while its
/// header may be zeroed (a persisted or never-synced rollback journal) or
/// truncated, so no content check can recognize it. Content is refused by name,
/// whatever the bytes (FS4). The suffix must end the name, so ordinary media
/// such as `diary-journal.mkv` is unaffected. Nothing is deleted or renamed.
fn sqlite_sidecar_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if ["-wal", "-shm", "-journal"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
    {
        return true;
    }
    let bytes = lower.as_bytes();
    [8, 9].iter().any(|&digits| {
        bytes.len() >= digits + 3
            && &bytes[bytes.len() - digits - 3..bytes.len() - digits] == b"-mj"
            && bytes[bytes.len() - digits..]
                .iter()
                .all(u8::is_ascii_hexdigit)
    })
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
    // Schema 39 (A5): the ctime a native scan recorded for exactly this
    // observation joins the content revision, so an edit that restores mtime
    // changes the revision after a rescan, and `with_validated_source` refuses
    // the descriptor before one. Without a bound ctime the key is omitted and
    // the revision is the same as frozen Python's.
    if store.schema_version >= crate::observation::SOURCE_CTIME_SCHEMA
        && row["status"] == "present"
        && let Some(ctime) = store
            .db
            .query_row(
                "SELECT ctime_ns FROM native_source_ctimes WHERE profile=? AND file_id=? AND size IS ? AND mtime_ns IS ? AND device IS ? AND inode IS ?",
                params![
                    profile,
                    id,
                    row["size"].as_i64(),
                    row["mtime_ns"].as_i64(),
                    row["device"].as_i64(),
                    row["inode"].as_i64()
                ],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
    {
        row["ctime_ns"] = json!(ctime);
    }
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
        || filename
            .file_name()
            .is_some_and(|name| sqlite_sidecar_name(&name.to_string_lossy()))
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
/// Callers inspect metadata only; delivered bytes go through `with_transfer`,
/// which also proves generated outputs on coarse-timestamp filesystems.
pub fn with_opened<T>(
    access: &mut Access,
    id: &str,
    revision: &str,
    operation: impl FnOnce(&File, &std::fs::Metadata) -> Result<T>,
) -> Result<T> {
    let row = authorize(access, id, revision)?;
    source::with_validated_source(&row, |file| {
        refuse_sqlite_payload(file)?;
        operation(file, &file.metadata()?)
    })
}
/// Ready generated outputs carry a recorded digest. On coarse-timestamp
/// filesystems it, rather than the metadata revision, proves the bytes served.
/// Sources have no recorded digest and keep the metadata revision contract.
fn artifact_digest(store: &Store, id: &str) -> Result<Option<String>> {
    Ok(store
        .db
        .prepare("SELECT sha256 FROM processing_artifacts WHERE file_id=? AND state='ready'")?
        .query_row([id], |r| r.get(0))
        .optional()?)
}
fn verify_artifact_bytes(file: &File, digest: Option<&str>) -> Result<()> {
    match digest {
        Some(digest) => crate::timestamps::verify_if_coarse(file, digest)
            .map_err(|_| denied("revision_mismatch")),
        None => Ok(()),
    }
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

/// Evidence may survive backpressure, but never carries an authorization cache.
#[derive(Clone)]
pub struct TransferEvidence {
    path: PathBuf,
    database_id: String,
    principal: String,
    profile: String,
    token: String,
    file_id: String,
    revision: String,
    ticket: Option<String>,
    /// Recorded SHA-256 of a ready generated output; sources have none.
    digest: Option<String>,
}
impl TransferEvidence {
    /// Deliver `length` bytes from `start`. A response that covers the whole
    /// file of a ready generated output (a full GET, or a range that spans the
    /// whole file) is hashed while it streams, and its final chunk is released
    /// only after the digest matches. Partial ranges and HEAD stay cheap: they
    /// never construct a body, or carry no digest check.
    pub fn body<'a>(
        &'a self,
        file: &'a File,
        stat: &'a std::fs::Metadata,
        start: u64,
        length: u64,
    ) -> Result<TransferBody<'a>> {
        let digest = self
            .digest
            .as_ref()
            .filter(|_| start == 0 && length == stat.len())
            .map(|expected| (<sha2::Sha256 as sha2::Digest>::new(), expected.clone()));
        let mut body = TransferBody {
            evidence: self,
            file,
            stat,
            offset: start,
            remaining: length,
            digest,
        };
        if length == 0 {
            // Nothing streams, so the empty representation is proven up front.
            body.finish()?;
        }
        Ok(body)
    }

    pub fn revalidate(&self) -> Result<()> {
        let store = Store::open(&self.path, false, false)?;
        if store.database_id != self.database_id {
            return Err(denied("unauthorized"));
        }
        let mut access = Access::from_stored_token(store, &self.token)?;
        if access.principal() != self.principal || access.profile() != self.profile {
            return Err(denied("unauthorized"));
        }
        authorize_snapshot(&mut access, &self.file_id, &self.revision)?;
        if let Some(ticket)=&self.ticket&&!access.store().db.prepare("SELECT 1 FROM api_tickets WHERE id=? AND token_id=? AND file_id=? AND revision=? AND revoked=0 AND expires>?")?.exists(params![ticket,self.token,self.file_id,self.revision,crate::notifications::now()])?{return Err(denied("unauthorized"))}
        Ok(())
    }
}
/// One response body. Each chunk revalidates current authorization and the
/// descriptor revision. A digest mismatch withholds the final chunk, so the
/// client sees a failed or truncated transfer rather than a complete wrong body.
pub struct TransferBody<'a> {
    evidence: &'a TransferEvidence,
    file: &'a File,
    stat: &'a std::fs::Metadata,
    offset: u64,
    remaining: u64,
    digest: Option<(sha2::Sha256, String)>,
}
impl TransferBody<'_> {
    pub fn remaining(&self) -> u64 {
        self.remaining
    }
    /// Read the next bounded chunk. The chunk that completes a hashed body is
    /// returned only once the streamed bytes match the recorded digest.
    pub fn next_chunk(&mut self) -> Result<Vec<u8>> {
        if self.remaining == 0 {
            return Err(denied("transfer already complete"));
        }
        self.evidence.revalidate()?;
        let bytes = read_chunk(
            self.file,
            self.stat,
            self.offset,
            self.remaining.min(65536) as usize,
        )?;
        let read = (bytes.len() as u64).min(self.remaining);
        self.offset += read;
        self.remaining -= read;
        if let Some((hasher, _)) = &mut self.digest {
            sha2::Digest::update(hasher, &bytes[..read as usize]);
        }
        if self.remaining == 0 {
            self.finish()?;
        }
        Ok(bytes[..read as usize].to_vec())
    }
    fn finish(&mut self) -> Result<()> {
        let Some((hasher, expected)) = self.digest.take() else {
            return Ok(());
        };
        if format!("{:x}", sha2::Digest::finalize(hasher)) != expected {
            // Quarantine this exact revision for the process: later reuse,
            // HEAD and range requests refuse it on every filesystem class.
            crate::timestamps::record_mismatch(self.stat, &expected);
            return Err(denied("revision_mismatch"));
        }
        Ok(())
    }
}
/// Release the catalog owner after header/range/slot/ticket admission, before
/// delivery. Existing source descriptor guards still fence the whole callback.
pub fn with_transfer<P, T>(
    mut access: Access,
    id: &str,
    revision: &str,
    ticket: Option<&str>,
    prepare: impl FnOnce(&mut Access, &std::fs::Metadata, &Value) -> Result<P>,
    deliver: impl FnOnce(&File, &std::fs::Metadata, TransferEvidence, P) -> Result<T>,
) -> Result<T> {
    let row = authorize(&mut access, id, revision)?;
    let digest = artifact_digest(access.store(), id)?;
    let evidence = TransferEvidence {
        path: access.store().path.clone(),
        database_id: access.store().database_id.clone(),
        principal: access.principal().into(),
        profile: access.profile().into(),
        token: access.token_id().into(),
        file_id: id.into(),
        revision: revision.into(),
        ticket: ticket.map(str::to_owned),
        digest: digest.clone(),
    };
    source::with_validated_source(&row, |file| {
        refuse_sqlite_payload(file)?;
        let stat = file.metadata()?;
        let prepared = prepare(&mut access, &stat, &row)?;
        drop(access);
        // Digest verification runs after catalog ownership is released and
        // before any byte is delivered. A complete response is also hashed
        // while it streams (`TransferEvidence::body`), which catches a rewrite
        // the metadata revision cannot see before the final chunk is released.
        verify_artifact_bytes(file, digest.as_deref())?;
        deliver(file, &stat, evidence, prepared)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coarse_timestamp_artifacts_are_served_only_with_their_recorded_digest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output.bin");
        std::fs::write(&path, b"generated").unwrap();
        let file = File::open(&path).unwrap();
        let digest = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(b"generated"));
        let wrong = "0".repeat(64);
        crate::timestamps::with_forced_granularity(true, || {
            verify_artifact_bytes(&file, Some(&digest)).unwrap();
            assert_eq!(
                verify_artifact_bytes(&file, Some(&wrong)).unwrap_err().0,
                "revision_mismatch"
            );
            // Sources have no recorded digest and keep the metadata contract.
            verify_artifact_bytes(&file, None).unwrap();
        });
        crate::timestamps::with_forced_granularity(false, || {
            verify_artifact_bytes(&file, Some(&wrong)).unwrap();
        });
    }
    fn fixture() -> (tempfile::TempDir, Access, String, String) {
        fixture_with(b"test bytes")
    }
    fn fixture_with(bytes: &[u8]) -> (tempfile::TempDir, Access, String, String) {
        let root = tempfile::tempdir().unwrap();
        let media = root.path().join("media");
        std::fs::create_dir(&media).unwrap();
        std::fs::write(media.join("file.bin"), bytes).unwrap();
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
    /// Record the fixture file as a ready generated output with `digest`. The
    /// processing job chain is irrelevant to delivery, so its keys are absent.
    fn ready_artifact(access: &Access, id: &str, digest: &str) {
        let db = &access.store().db;
        db.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        db.execute("INSERT INTO processing_artifacts(id,job_id,attempt,profile,location,path,temporary_path,binding,state,file_id,item_id,role,sha256) VALUES ('artifact','job',1,'default','media','file.bin','.artifact.partial','{}','ready',?,'item','rendition',?)",params![id,digest]).unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    }
    fn sha(bytes: &[u8]) -> String {
        format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(bytes))
    }
    /// Stream one response through `with_transfer`, collecting every chunk
    /// released before the body completes or fails.
    fn stream(
        access: Access,
        id: &str,
        revision: &str,
        start: u64,
        length: Option<u64>,
    ) -> (Vec<u8>, Result<()>) {
        let mut delivered = Vec::new();
        let result = with_transfer(
            access,
            id,
            revision,
            None,
            |_, _, _| Ok(()),
            |file, stat, evidence, ()| {
                let mut body =
                    evidence.body(file, stat, start, length.unwrap_or(stat.len() - start))?;
                while body.remaining() > 0 {
                    delivered.extend(body.next_chunk()?);
                }
                Ok(())
            },
        );
        (delivered, result)
    }
    #[test]
    fn complete_responses_are_hashed_and_withhold_the_final_chunk_on_mismatch() {
        let bytes = (0..200_000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        // Correct digest: the whole body is delivered on a fine filesystem.
        let (_root, access, id, revision) = fixture_with(&bytes);
        ready_artifact(&access, &id, &sha(&bytes));
        crate::timestamps::with_forced_granularity(false, || {
            let (delivered, result) = stream(access, &id, &revision, 0, None);
            result.unwrap();
            assert_eq!(delivered, bytes);
        });
        // A rewrite the metadata revision cannot see (modelled by a recorded
        // digest that differs from the bytes) aborts before the final chunk.
        let (_root, access, id, revision) = fixture_with(&bytes);
        ready_artifact(&access, &id, &sha(b"other bytes"));
        let retry = access.current().unwrap();
        let ranged = access.current().unwrap();
        crate::timestamps::with_forced_granularity(false, || {
            let digests = crate::timestamps::digests_computed();
            let (delivered, result) = stream(access, &id, &revision, 0, None);
            assert_eq!(result.unwrap_err().0, "revision_mismatch");
            // The fine filesystem skipped the up-front digest; only the
            // streamed hash, which needs no extra read, found the mismatch.
            assert_eq!(crate::timestamps::digests_computed(), digests);
            assert!(!delivered.is_empty());
            assert!(delivered.len() < bytes.len());
            assert_eq!(delivered[..], bytes[..delivered.len()]);
            // The exact revision is now quarantined for this process: even a
            // cheap range request is refused before any byte is delivered.
            let (delivered, result) = stream(ranged, &id, &revision, 0, Some(10));
            assert_eq!(result.unwrap_err().0, "revision_mismatch");
            assert!(delivered.is_empty());
            let (delivered, result) = stream(retry, &id, &revision, 0, None);
            assert_eq!(result.unwrap_err().0, "revision_mismatch");
            assert!(delivered.is_empty());
        });
    }
    #[test]
    fn partial_responses_and_sources_stay_cheap() {
        let bytes = b"partial content bytes";
        let (_root, access, id, revision) = fixture_with(bytes);
        ready_artifact(&access, &id, &sha(b"different"));
        let whole = access.current().unwrap();
        crate::timestamps::with_forced_granularity(false, || {
            // A strict sub-range is never hashed, so it is not refused.
            let (delivered, result) = stream(access, &id, &revision, 1, Some(5));
            result.unwrap();
            assert_eq!(delivered, &bytes[1..6]);
            // A range that spans the whole file is a complete body.
            let (delivered, result) = stream(whole, &id, &revision, 0, Some(bytes.len() as u64));
            assert_eq!(result.unwrap_err().0, "revision_mismatch");
            assert!(delivered.is_empty());
        });
        // Sources have no recorded digest and keep the metadata contract.
        let (_root, access, id, revision) = fixture_with(bytes);
        let (delivered, result) = stream(access, &id, &revision, 0, None);
        result.unwrap();
        assert_eq!(delivered, bytes);
    }
    #[test]
    fn an_empty_complete_body_is_proven_before_the_response() {
        let (_root, access, id, revision) = fixture_with(b"");
        ready_artifact(&access, &id, &sha(b"not empty"));
        crate::timestamps::with_forced_granularity(false, || {
            let (delivered, result) = stream(access, &id, &revision, 0, None);
            assert_eq!(result.unwrap_err().0, "revision_mismatch");
            assert!(delivered.is_empty());
        });
        let (_root, access, id, revision) = fixture_with(b"");
        ready_artifact(&access, &id, &sha(b""));
        crate::timestamps::with_forced_granularity(false, || {
            let (delivered, result) = stream(access, &id, &revision, 0, None);
            result.unwrap();
            assert!(delivered.is_empty());
        });
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
    fn orphaned_catalog_wal_and_shm_aliases_never_reach_content_callbacks() {
        let (root, access, _, _) = fixture();
        let database = access.store().path.clone();
        assert_eq!(
            access
                .store()
                .db
                .query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "wal"
        );
        access.store().db.execute_batch(
            "INSERT INTO api_principals(id,profile) VALUES ('disposable-private-marker','default');",
        ).unwrap();
        let aliases = ["wal", "shm"].map(|kind| {
            let original = PathBuf::from(format!("{}-{kind}", database.display()));
            let alias = root.path().join("media").join(format!("alias-{kind}.bin"));
            std::fs::hard_link(&original, &alias).unwrap();
            (original, alias)
        });
        drop(access);
        for (original, alias) in &aliases {
            assert!(
                !original.exists(),
                "SQLite did not remove the original sidecar"
            );
            assert!(alias.is_file());
            assert_eq!(alias.metadata().unwrap().nlink(), 1);
        }
        let store = Store::open(&database, true, false).unwrap();
        crate::observation::execute(&store, "default", "scan", &json!({"source":"media"})).unwrap();
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM api_private_files", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let mut access = Access::authenticate(store, "token.secret").unwrap();
        for (_, alias) in &aliases {
            let filename = alias.file_name().unwrap().to_str().unwrap();
            let id = access
                .store()
                .db
                .query_row("SELECT id FROM files WHERE path=?", [filename], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap();
            let revision = revision_of(access.store(), "default", &id).unwrap();
            // Names/registration/live-inode guards cannot recognize this orphan.
            authorize(&mut access, &id, &revision).unwrap();
            let called = std::cell::Cell::new(false);
            let result = with_opened(&mut access, &id, &revision, |_, _| {
                called.set(true);
                Ok(())
            });
            assert_eq!(result.unwrap_err().0, "private_content", "{filename}");
            assert!(!called.get());
            let transfer_access = access.current().unwrap();
            let result = with_transfer(
                transfer_access,
                &id,
                &revision,
                None,
                |_, _, _| {
                    called.set(true);
                    Ok(())
                },
                |_, _, _, ()| {
                    called.set(true);
                    Ok(())
                },
            );
            assert_eq!(result.unwrap_err().0, "private_content", "{filename}");
            assert!(!called.get());
        }
    }
    #[test]
    fn recognizable_sidecar_prefixes_are_conservative_and_endian_independent() {
        for prefix in [
            0x377f0682u32.to_be_bytes().as_slice(),
            0x377f0683u32.to_be_bytes().as_slice(),
            3_007_000u32.to_le_bytes().as_slice(),
            3_007_000u32.to_be_bytes().as_slice(),
            // Rollback journal aJournalMagic.
            [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7].as_slice(),
        ] {
            for suffix in [b"".as_slice(), b"arbitrary private body".as_slice()] {
                let file = tempfile::tempfile().unwrap();
                file.write_all_at(&[prefix, suffix].concat(), 0).unwrap();
                assert_eq!(
                    refuse_sqlite_payload(&file).unwrap_err().0,
                    "private_content"
                );
            }
        }
        // Recognizable headers are denied even when the rest is corrupt. No
        // claim is made for a missing or modified signature, regardless of size.
        for bytes in [
            b"".as_slice(),
            b"ordinary media",
            &[0; 128],
            &[0x37, 0x7f, 0x06],
        ] {
            let file = tempfile::tempfile().unwrap();
            file.write_all_at(bytes, 0).unwrap();
            refuse_sqlite_payload(&file).unwrap();
        }
    }
    #[test]
    fn sqlite_sidecar_names_are_recognized_only_as_whole_suffixes() {
        for name in [
            "catalog.sqlite3-wal",
            "catalog.sqlite3-shm",
            "catalog.sqlite3-journal",
            "old catalog.db-JOURNAL",
            "renamed-wal",
            "-shm",
            "catalog.sqlite3-mj7f3319fa",
            // The pinned SQLite's nine-digit `-mj%06X9%02X` form.
            "old.db-mj1234569AB",
            "catalog.sqlite3-mjABCDEF9FF",
            "catalog.sqlite3-MJ7F3319FA",
            "\u{e9}t\u{e9}.db-wal",
        ] {
            assert!(sqlite_sidecar_name(name), "{name}");
        }
        for name in [
            "diary-journal.mkv",
            "movie-wal.mp4",
            "catalog.sqlite3",
            "walnut",
            "show-shm.srt",
            "catalog.sqlite3-mj7f3319f",
            "catalog.sqlite3-mj7f3319fz",
            "catalog.sqlite3-mj1234569ABC",
            "catalog.sqlite3-mj1234569AZ",
            "\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}",
            "",
        ] {
            assert!(!sqlite_sidecar_name(name), "{name}");
        }
    }
    #[test]
    fn unrecognizable_orphaned_sidecars_are_refused_by_name_and_never_removed() {
        let (root, access, _, _) = fixture();
        let media = root.path().join("media");
        // Zeroed or truncated headers carry no SQLite signature. The bodies
        // stand in for private database pages left by a renamed catalog.
        let mut zeroed_journal = vec![0u8; 28];
        zeroed_journal.extend_from_slice(b"private page bytes");
        let zeroed_wal = [0u8; 64];
        let orphans: [(&str, &[u8]); 5] = [
            ("old-catalog.sqlite3-journal", zeroed_journal.as_slice()),
            ("old-catalog.sqlite3-wal", zeroed_wal.as_slice()),
            ("old-catalog.sqlite3-shm", b"".as_slice()),
            (
                "old-catalog.sqlite3-mj0A1B2C9D3",
                b"old-catalog.sqlite3-journal\0".as_slice(),
            ),
            ("nested-WAL", b"private".as_slice()),
        ];
        for (name, bytes) in orphans {
            std::fs::write(media.join(name), bytes).unwrap();
        }
        std::fs::write(media.join("diary-journal.mkv"), b"ordinary media").unwrap();
        let database = access.store().path.clone();
        drop(access);
        let store = Store::open(&database, true, false).unwrap();
        // Sidecar names have no media extension, so they are cataloged only
        // when the source policy admits every file.
        crate::observation::execute(
            &store,
            "default",
            "scan_policy",
            &json!({"source":"media","policy":{"extensions":null},"apply":true}),
        )
        .unwrap();
        let scanned =
            crate::observation::execute(&store, "default", "scan", &json!({"source":"media"}))
                .unwrap();
        let mut access = Access::authenticate(store, "token.secret").unwrap();
        let listed = access
            .store()
            .db
            .prepare("SELECT path FROM files ORDER BY path")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(listed.len(), 7, "{listed:?} {scanned}");
        let lookup = |access: &Access, name: &str| {
            let id = access
                .store()
                .db
                .query_row("SELECT id FROM files WHERE path=?", [name], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap();
            let revision = revision_of(access.store(), "default", &id).unwrap();
            (id, revision)
        };
        for (name, bytes) in orphans {
            let (id, revision) = lookup(&access, name);
            assert_eq!(
                authorize(&mut access, &id, &revision).unwrap_err().0,
                "private_content",
                "{name}"
            );
            assert_eq!(
                ticket(&mut access, &id, &revision, 300, 100).unwrap_err().0,
                "private_content",
                "{name}"
            );
            let called = std::cell::Cell::new(false);
            let result = with_transfer(
                access.current().unwrap(),
                &id,
                &revision,
                None,
                |_, _, _| {
                    called.set(true);
                    Ok(())
                },
                |_, _, _, ()| {
                    called.set(true);
                    Ok(())
                },
            );
            assert_eq!(result.unwrap_err().0, "private_content", "{name}");
            assert!(!called.get());
            // Refusal never deletes or rewrites what Catabolic cannot prove it owns.
            assert_eq!(std::fs::read(media.join(name)).unwrap(), bytes, "{name}");
        }
        let (id, revision) = lookup(&access, "diary-journal.mkv");
        let bytes = with_opened(&mut access, &id, &revision, |file, stat| {
            read_chunk(file, stat, 0, 64)
        })
        .unwrap();
        assert_eq!(bytes, b"ordinary media");
    }
    #[test]
    fn ordinary_content_still_reaches_both_descriptor_callbacks() {
        let (_root, mut access, id, revision) = fixture();
        let bytes = with_opened(&mut access, &id, &revision, |file, stat| {
            read_chunk(file, stat, 0, 10)
        })
        .unwrap();
        assert_eq!(bytes, b"test bytes");
        let bytes = with_transfer(
            access,
            &id,
            &revision,
            None,
            |_, _, _| Ok(()),
            |file, stat, _, ()| read_chunk(file, stat, 0, 10),
        )
        .unwrap();
        assert_eq!(bytes, b"test bytes");
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
