// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Ordinary local administration owners; no external account or transport.
use crate::{Error, Result, Store, access_admin, embedding::Session, migration::rows};
use rusqlite::params;
use serde_json::{Value, json};
fn text<'a>(args: &'a Value, key: &str) -> &'a str {
    args[key].as_str().unwrap_or("")
}

pub fn execute(
    session: &mut Session,
    profile: &str,
    operation: &str,
    args: &Value,
) -> Result<Value> {
    execute_detailed(session, profile, operation, args).map_err(Into::into)
}
pub fn execute_detailed(
    session: &mut Session,
    profile: &str,
    operation: &str,
    args: &Value,
) -> access_admin::DetailedResult<Value> {
    match operation {
        "principal-put" => {
            access_admin::principal_put_detailed(session.store()?, &json!(profile), &args["id"])
        }
        "grant-put" => access_admin::grant_put_detailed(
            session,
            &args["principal"],
            &args["definition"],
            args.get("id"),
        ),
        "token-create" => {
            let store = session.store()?;
            let issued = access_admin::issue_detailed(
                store,
                &args["principal"],
                &args["grant"],
                &args["ttl"],
            )?;
            save_issued(store, text(args, "output"), &issued)
        }
        "token-rotate" => {
            let store = session.store()?;
            let tokens = rows(
                &store.db,
                "SELECT principal,grants FROM api_tokens WHERE id=? AND revoked=0",
                &[text(args, "id").to_string().into()],
            )?;
            let old = tokens
                .first()
                .ok_or_else(|| Error("unknown active token".into()))?;
            let grants = catabolic_core::decode_json(old["grants"].as_str().unwrap_or(""))
                .map_err(Error::from)?;
            let issued =
                access_admin::issue_detailed(store, &old["principal"], &grants, &args["ttl"])?;
            let saved = save_issued(store, text(args, "output"), &issued)?;
            local(store, profile, "token-revoke", args)?;
            Ok(saved)
        }
        _ => local(session.store()?, profile, operation, args).map_err(Into::into),
    }
}

/// Frozen pathlib's POSIX split: an exact leading `//` is a distinct root,
/// while one or three-plus separators are `/`. Empty and `.` parts vanish;
/// `..` is retained and nothing is resolved.
fn pathlib_parse(path: &[u8]) -> (&'static [u8], Vec<Vec<u8>>) {
    let root: &'static [u8] = if path.starts_with(b"//") && !path.starts_with(b"///") {
        b"//"
    } else if path.starts_with(b"/") {
        b"/"
    } else {
        b""
    };
    let parts = path
        .split(|byte| *byte == b'/')
        .filter(|part| !part.is_empty() && *part != b".")
        .map(<[u8]>::to_vec)
        .collect();
    (root, parts)
}

/// The passwd home of a named user, or of the real user ID for `None`.
/// Every lookup failure is Python's `KeyError`, which leaves `~` unexpanded.
fn passwd_home(name: Option<&[u8]>) -> Option<Vec<u8>> {
    let name = name.map(std::ffi::CString::new).transpose().ok()?;
    let mut buffer = vec![0 as libc::c_char; 4096];
    loop {
        // SAFETY: zeroed passwd is a valid out-parameter; the buffer and
        // CString outlive the call, and the result is copied before reuse.
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer refers to live storage sized as passed, and
        // the name is a NUL-terminated CString.
        let status = unsafe {
            match &name {
                Some(name) => libc::getpwnam_r(
                    name.as_ptr(),
                    &mut entry,
                    buffer.as_mut_ptr(),
                    buffer.len(),
                    &mut found,
                ),
                None => libc::getpwuid_r(
                    libc::getuid(),
                    &mut entry,
                    buffer.as_mut_ptr(),
                    buffer.len(),
                    &mut found,
                ),
            }
        };
        if status == libc::ERANGE && buffer.len() < 1 << 20 {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if status != 0 || found.is_null() || entry.pw_dir.is_null() {
            return None;
        }
        // SAFETY: a successful lookup stores a NUL-terminated pw_dir in buffer.
        return Some(
            unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) }
                .to_bytes()
                .to_vec(),
        );
    }
}

/// `Path(filename).expanduser().absolute()` from frozen Python 3.14 pathlib.
/// `Ok(None)` is its `RuntimeError("Could not determine home directory.")`.
fn pathlib_expanded_absolute(filename: &str) -> Result<Option<std::path::PathBuf>> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let (mut root, mut parts) = pathlib_parse(filename.as_bytes());
    if root.is_empty() && parts.first().is_some_and(|part| part.starts_with(b"~")) {
        let first = parts.remove(0);
        // posixpath.expanduser on the first component alone: HOME (even empty)
        // for bare `~`, else the passwd entry; named users use getpwnam.
        let home = if first == b"~" {
            match std::env::var_os("HOME") {
                Some(home) => Some(home.as_bytes().to_vec()),
                None => passwd_home(None),
            }
        } else {
            passwd_home(Some(&first[1..]))
        };
        let mut home = home.unwrap_or(first);
        while home.last() == Some(&b'/') {
            home.pop();
        }
        if home.is_empty() {
            home.push(b'/');
        }
        if home.starts_with(b"~") {
            return Ok(None);
        }
        let (home_root, mut home_parts) = pathlib_parse(&home);
        home_parts.append(&mut parts);
        (root, parts) = (home_root, home_parts);
    }
    if root.is_empty() {
        let cwd = std::env::current_dir()?;
        let (cwd_root, mut cwd_parts) = pathlib_parse(cwd.as_os_str().as_bytes());
        cwd_parts.append(&mut parts);
        (root, parts) = (cwd_root, cwd_parts);
    }
    let mut path = root.to_vec();
    path.extend(parts.join(&b'/'));
    Ok(Some(std::ffi::OsString::from_vec(path).into()))
}

fn save_issued(
    store: &Store,
    filename: &str,
    issued: &Value,
) -> access_admin::DetailedResult<Value> {
    use std::{io::Write, path::Path};
    let Some(path) = pathlib_expanded_absolute(filename)? else {
        // Frozen Python raises this after token issuance; its traceback is
        // replaced by the structured runtime-class presentation.
        let message = "Could not determine home directory.".to_owned();
        return Err(access_admin::Failure::Boundary(
            crate::DetailedError::mapping_runtime(Error(message.clone()), "RuntimeError", message),
        ));
    };
    let payload = format!(
        "{{\"token_id\": {}, \"token\": {}, \"expires\": {}}}",
        catabolic_core::encode_with(&issued["token_id"], true, false),
        catabolic_core::encode_with(&issued["token"], true, false),
        catabolic_core::encode_with(&issued["expires"], true, false)
    );
    crate::private_secret::save(store, Path::new(&path), |stream| {
        stream.write_all(payload.as_bytes())
    })
    .map_err(|error| match error {
        crate::private_secret::Failure::Io(error)
        | crate::private_secret::Failure::Serialization(error) => {
            Error(format!("{}: {}", error, path.display()))
        }
        crate::private_secret::Failure::Native(error) => error,
    })?;
    Ok(json!({"token_id":issued["token_id"],"expires":issued["expires"],"credential_file":path}))
}
fn local(store: &Store, profile: &str, operation: &str, args: &Value) -> Result<Value> {
    let id = text(args, "id");
    match operation {
        "principal-disable" | "grant-disable" | "operation-disable" => {
            let statement = match operation {
                "principal-disable" => "UPDATE api_principals SET enabled=0 WHERE id=?",
                "grant-disable" => "UPDATE api_grants SET enabled=0 WHERE id=?",
                _ => "UPDATE api_operations SET enabled=0 WHERE id=?",
            };
            let changed = store.transaction(|db| Ok(db.execute(statement, [id])?))?;
            Ok(json!({"disabled": changed != 0}))
        }
        "token-revoke" => {
            let changed = store.transaction(|db| {
                let changed = db.execute("UPDATE api_tokens SET revoked=1 WHERE id=?", [id])?;
                db.execute(
                    "INSERT INTO api_audit(action,resource) VALUES ('token.revoke',?)",
                    [id],
                )?;
                Ok(changed)
            })?;
            Ok(json!({"revoked": changed != 0}))
        }
        "tokens" => Ok(json!({"tokens": rows(&store.db,
            "SELECT t.id,t.principal,t.expires,t.revoked FROM api_tokens t JOIN api_principals p ON p.id=t.principal WHERE p.profile=? ORDER BY t.id LIMIT 1000", &[profile.to_string().into()])?})),
        "source-expose" => {
            let location = text(args, "location");
            crate::observation::binding(store, profile, "source", location)?;
            let disable = args["disable"] == true;
            store.transaction(|db| {
                db.execute(
                    if disable {
                        "DELETE FROM api_sources WHERE profile=? AND location=?"
                    } else {
                        "INSERT INTO api_sources VALUES (?,?) ON CONFLICT DO NOTHING"
                    },
                    params![profile, location],
                )?;
                Ok(())
            })?;
            Ok(json!({"location": location, "exposed": !disable}))
        }
        "operation-approve" => {
            let recipe = text(args, "recipe_id");
            let location = text(args, "location");
            crate::artifact_mutation::get_recipe(store, recipe).map_err(|error| {
                if error.0 == "unknown immutable operation revision" {
                    Error("unknown recipe ID; select an explicit immutable revision".into())
                } else {
                    error
                }
            })?;
            if !store
                .db
                .prepare("SELECT 1 FROM generated_locations WHERE profile=? AND location=?")?
                .exists(params![profile, location])?
            {
                return Err(Error("approve an existing generated destination".into()));
            }
            let id = if id.is_empty() {
                uuid::Uuid::new_v4().to_string()
            } else {
                id.to_owned()
            };
            store.transaction(|db| {
                db.execute(
                    "INSERT INTO api_operations(id,profile,recipe_id,location) VALUES (?,?,?,?)",
                    params![id, profile, recipe, location],
                )?;
                Ok(())
            })?;
            Ok(json!({"operation_id": id, "recipe_id": recipe, "location": location}))
        }
        _ => Err(Error("unsupported local administration operation".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pathlib_split_keeps_double_root_and_parent_parts() {
        let parts = |raw: &str| {
            let (root, parts) = pathlib_parse(raw.as_bytes());
            (
                String::from_utf8(root.to_vec()).unwrap(),
                parts
                    .into_iter()
                    .map(|part| String::from_utf8(part).unwrap())
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(
            parts("//h/./a//b/"),
            ("//".into(), vec!["h".into(), "a".into(), "b".into()])
        );
        assert_eq!(
            parts("///h/../a"),
            ("/".into(), vec!["h".into(), "..".into(), "a".into()])
        );
        assert_eq!(
            parts("./~//x"),
            (String::new(), vec!["~".into(), "x".into()])
        );
        assert_eq!(parts(""), (String::new(), vec![]));
    }

    #[test]
    fn absolute_and_unknown_named_user_paths() {
        assert_eq!(
            pathlib_expanded_absolute("//h/./x").unwrap(),
            Some(std::path::PathBuf::from("//h/x"))
        );
        assert_eq!(
            pathlib_expanded_absolute("/a/~b/../c").unwrap(),
            Some(std::path::PathBuf::from("/a/~b/../c"))
        );
        // An unknown user leaves `~name` unexpanded, which pathlib refuses.
        assert_eq!(
            pathlib_expanded_absolute("~catabolic-no-such-user-fixture/x").unwrap(),
            None
        );
        assert!(passwd_home(Some(b"catabolic\0nul")).is_none());
    }
}
