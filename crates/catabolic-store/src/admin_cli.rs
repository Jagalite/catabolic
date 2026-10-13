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
            save_issued(store, text(args, "output"), &issued).map_err(Into::into)
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

fn save_issued(store: &Store, filename: &str, issued: &Value) -> Result<Value> {
    use std::{
        io::Write,
        path::{Component, Path, PathBuf},
    };
    // Pathlib normalizes separators and '.' before expanduser. Expand only the
    // complete leading '~' component, so '~//...' cannot turn the remaining
    // suffix into an absolute path that discards HOME during join.
    let normalized: PathBuf = Path::new(filename)
        .components()
        .filter(|part| !matches!(part, Component::CurDir))
        .collect();
    let expanded = if let Ok(relative) = normalized.strip_prefix("~") {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| Error("could not determine home directory".into()))?;
        PathBuf::from(home).join(relative)
    } else if normalized.as_os_str().as_encoded_bytes().starts_with(b"~") {
        return Err(Error(
            "named-user home expansion is not yet qualified in native credential output".into(),
        ));
    } else {
        normalized
    };
    // Path components remove dot/trailing separators without resolving symlinks
    // or collapsing '..', matching pathlib's ordinary absolute path boundary.
    let path: PathBuf = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()?.join(expanded)
    }
    .components()
    .collect();
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
