// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Isolated native and frozen differential checks for access administration.
use super::*;
use crate::{migration, selection::Evaluation};
use std::{
    collections::BTreeMap,
    io::Write,
    process::{Command, Stdio},
};

fn owner(writable: bool, setup: &[Value]) -> (tempfile::TempDir, Session) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.db");
    migration::initialize(&path).unwrap();
    let store = Store::open(&path, true, false).unwrap();
    for sql in setup {
        store
            .transaction(|db| {
                db.execute(sql.as_str().unwrap(), [])?;
                Ok(())
            })
            .unwrap();
    }
    drop(store);
    (root, Session::open(&path, writable, false).unwrap())
}
fn setup() -> Vec<Value> {
    vec![
        json!("INSERT INTO profiles VALUES ('alternate')"),
        json!(
            "INSERT INTO api_principals(id,profile) VALUES ('fixture-principal','default'),('fixture-other','alternate'),('fixture-disabled','default'),('123','default')"
        ),
        json!("UPDATE api_principals SET enabled=0 WHERE id='fixture-disabled'"),
        json!(
            "INSERT INTO api_grants(id,principal,definition) VALUES ('fixture-grant','fixture-principal','{\"actions\": [\"metadata:read\"]}'),('disabled-principal-grant','fixture-disabled','{\"actions\": [\"metadata:read\"]}'),('123','fixture-principal','{\"actions\": [\"metadata:read\"]}')"
        ),
        json!(
            "INSERT INTO items(id,kind,metadata) VALUES ('item-a','movie','{}'),('item-b','movie','{}')"
        ),
    ]
}
fn snapshot(store: &Store) -> Value {
    let queries = [
        (
            "principals",
            "SELECT id,profile,enabled FROM api_principals ORDER BY id",
        ),
        (
            "grants",
            "SELECT id,principal,definition,revision,enabled FROM api_grants ORDER BY id",
        ),
        (
            "tokens",
            "SELECT id,principal,digest,grants,expires,revoked FROM api_tokens ORDER BY id",
        ),
        (
            "audit",
            "SELECT id,principal,action,resource FROM api_audit ORDER BY id",
        ),
        ("epoch", "SELECT generation FROM fallback_epoch WHERE id=1"),
    ];
    let mut result = json!({"in_transaction":!store.db.is_autocommit()});
    for (name, sql) in queries {
        result[name] = json!(migration::rows(&store.db, sql, &[]).unwrap());
    }
    result
}
fn normalize(value: &mut Value, generated: &BTreeMap<String, String>) {
    match value {
        Value::String(s) => {
            if let Some(replacement) = generated.get(s) {
                *s = replacement.clone();
            }
        }
        Value::Array(values) => values.iter_mut().for_each(|v| normalize(v, generated)),
        Value::Object(values) => values.values_mut().for_each(|v| normalize(v, generated)),
        _ => {}
    }
}
fn native(scenario: &Value) -> Value {
    let (_root, mut session) = owner(
        scenario["writable"].as_bool().unwrap_or(true),
        scenario["setup"].as_array().unwrap(),
    );
    let mut output = vec![];
    let mut scopes = vec![];
    let mut generated = BTreeMap::new();
    for step in scenario["steps"].as_array().unwrap() {
        let result = (|| -> DetailedResult<Value> {
            match step["op"].as_str().unwrap() {
                "principal" => principal_put_detailed(
                    session.store()?,
                    step.get("profile").unwrap_or(&json!("default")),
                    &step["id"],
                ),
                "grant" => {
                    let result = grant_put_detailed(
                        &mut session,
                        &step["principal"],
                        &step["definition"],
                        step.get("id"),
                    )?;
                    if !step.get("id").is_some_and(truthy) {
                        let id = result["grant_id"].as_str().unwrap();
                        assert_eq!(uuid::Uuid::parse_str(id).unwrap().get_version_num(), 4);
                        generated.insert(
                            id.to_owned(),
                            format!("generated-grant-{}", generated.len()),
                        );
                    }
                    Ok(result)
                }
                "issue" => issue_with_sources_detailed(
                    session.store()?,
                    &step["principal"],
                    &step["grants"],
                    step.get("ttl").unwrap_or(&json!(3600)),
                    |bytes| {
                        bytes.fill(step["seed"].as_u64().unwrap_or(53) as u8);
                        Ok(())
                    },
                    || step["now"].as_f64().unwrap_or(4102444800.25),
                ),
                "access_members" => {
                    assert!(
                        session.store()?.db.is_autocommit(),
                        "separate authentication fixture requires committed owner writes"
                    );
                    let raw = [step["seed"].as_u64().unwrap() as u8; 44];
                    let id = raw[..12]
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>();
                    let secret = format!("{id}.{}", URL_SAFE_NO_PAD.encode(&raw[12..]));
                    let mut access = crate::access::Access::authenticate(
                        Store::open(&session.store()?.path, false, false)?,
                        &secret,
                    )?;
                    let grants = access.grants().to_vec();
                    let mut values = vec![];
                    for index in step["checks"].as_array().unwrap() {
                        let grant = &grants[index.as_u64().unwrap() as usize];
                        let members = access.members(grant)?.clone();
                        values.push(
                            json!({"id":grant["_id"],"actions":grant["actions"],"members":members}),
                        );
                    }
                    Ok(json!(values))
                }
                "origin" => Ok(
                    match crate::notifications::callback_origin(step["value"].as_str().unwrap()) {
                        Ok(origin) => json!([origin.scheme, origin.host, origin.port]),
                        Err(_) => json!({"invalid":true}),
                    },
                ),
                "sql" => {
                    session.store()?.transaction(|db| {
                        db.execute(step["sql"].as_str().unwrap(), [])?;
                        Ok(())
                    })?;
                    Ok(Value::Null)
                }
                "begin" => {
                    scopes.push(session.begin()?);
                    Ok(Value::Null)
                }
                "finish" => {
                    session.finish(scopes.pop().unwrap(), step["commit"].as_bool().unwrap())?;
                    Ok(Value::Null)
                }
                op => panic!("unknown fixture op: {op}"),
            }
        })();
        let outcome = match result {
            Ok(value) => json!({"ok":value}),
            Err(error) => {
                let status = match &error {
                    Failure::Access(error) => Some(error.status()),
                    Failure::Native(error)
                        if step["op"] == "access_members" && error.0 == "unauthorized" =>
                    {
                        Some(401)
                    }
                    Failure::Native(_) => None,
                };
                json!({"error":error.to_string(),"status":status})
            }
        };
        let mut row = json!({"result":outcome,"state":snapshot(session.store().unwrap())});
        normalize(&mut row, &generated);
        row["state"]["grants"]
            .as_array_mut()
            .unwrap()
            .sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        output.push(row);
    }
    assert!(scopes.is_empty());
    json!(output)
}
fn difference(actual: &Value, expected: &Value, path: &str) -> String {
    match (actual, expected) {
        (Value::Object(a), Value::Object(b)) if a.keys().eq(b.keys()) => {
            for (key, value) in a {
                if value != &b[key] {
                    return difference(value, &b[key], &format!("{path}/{key}"));
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (index, (a, b)) in a.iter().zip(b).enumerate() {
                if a != b {
                    return difference(a, b, &format!("{path}/{index}"));
                }
            }
        }
        _ => {}
    }
    let short = |v: &Value| {
        let text = encode(v);
        if text.len() > 1000 {
            format!("{} bytes; sha256 {}", text.len(), sha256(&text))
        } else {
            text
        }
    };
    format!(
        "{path}: native {}; frozen {}",
        short(actual),
        short(expected)
    )
}
fn frozen(label: &str, scenario: &Value) -> Value {
    let python = std::env::var("CATABOLIC_PYTHON_ORACLE")
        .expect("set CATABOLIC_PYTHON_ORACLE to the pinned Python 3.14.6 interpreter");
    let reference = std::env::var("CATABOLIC_PYTHON_REFERENCE")
        .expect("set CATABOLIC_PYTHON_REFERENCE to the frozen checkout's src directory");
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/reference/access_admin_oracle.py");
    let mut child = Command::new(python)
        .arg(script)
        .arg(reference)
        .env_remove("PYTHONOPTIMIZE")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(encode(scenario).as_bytes())
        .unwrap();
    let oracle = child.wait_with_output().unwrap();
    assert!(
        oracle.status.success(),
        "{label}: {}",
        String::from_utf8_lossy(&oracle.stderr)
    );
    catabolic_core::decode_json_bytes(&oracle.stdout).unwrap()
}
fn compare(label: &str, setup: Vec<Value>, steps: Vec<Value>, writable: bool) {
    let scenario = json!({"setup":setup,"steps":steps,"writable":writable});
    let expected = frozen(label, &scenario);
    let actual = native(&scenario);
    for (index, (actual, expected)) in actual
        .as_array()
        .unwrap()
        .iter()
        .zip(expected.as_array().unwrap())
        .enumerate()
    {
        if actual != expected {
            panic!(
                "{label}, step {index}, op {}: {}",
                scenario["steps"][index]["op"],
                difference(actual, expected, "")
            );
        }
    }
    assert_eq!(
        actual.as_array().unwrap().len(),
        expected.as_array().unwrap().len()
    );
    eprintln!(
        "{label}: {} paired state transitions",
        scenario["steps"].as_array().unwrap().len()
    );
}
fn grant(definition: Value, id: Value) -> Value {
    json!({"op":"grant","principal":"fixture-principal","definition":definition,"id":id})
}
fn issue_step(grants: Value, ttl: Value, seed: u8) -> Value {
    json!({"op":"issue","principal":"fixture-principal","grants":grants,"ttl":ttl,"seed":seed})
}
fn query(setup: &mut Vec<Value>, id: &str, definition: Value, profile: &str) {
    let encoded = encode(&definition).replace('\'', "''");
    setup.push(json!(format!("INSERT INTO saved_queries(id,profile,name,revision,definition,digest) VALUES ('{id}','{profile}','{id}',1,'{encoded}','fixture-digest-{id}')")));
}

#[test]
#[ignore = "requires pinned frozen checkout and Python 3.14.6; run with --ignored"]
fn frozen_access_owner_differential() {
    let mut steps = vec![];
    for id in [
        json!(null),
        json!(true),
        json!(5),
        json!([]),
        json!({}),
        json!(""),
        json!("雪".repeat(129)),
        json!("x".repeat(129)),
        json!("雪".repeat(128)),
        json!("x"),
        json!("line\n\0principal"),
    ] {
        steps.push(json!({"op":"principal","id":id}));
    }
    steps.extend([
        json!({"op":"principal","id":"fixture-disabled"}),
        json!({"op":"principal","id":"fixture-principal","profile":"alternate"}),
        json!({"op":"principal","id":"fixture-principal","profile":"missing"}),
        json!({"op":"principal","id":"new","profile":"missing"}),
        json!({"op":"principal","id":"new","profile":null}),
        json!({"op":"principal","id":"new","profile":[]}),
        json!({"op":"principal","id":"new","profile":123}),
    ]);
    compare(
        "principal validation, conflict, enablement, Unicode",
        setup(),
        steps,
        true,
    );

    let mut steps = vec![];
    for definition in [
        json!(null),
        json!([]),
        json!("bad"),
        json!({}),
        json!({"actions":["x"],"extra":true}),
        json!({"actions":[]}),
        json!({"actions":[1]}),
        json!({"actions":["x"],"operator":1}),
        json!({"actions":["x"],"derivatives":null}),
        json!({"actions":["x"],"revisions":[]}),
        json!({"actions":["x"],"revisions":{"file":5}}),
    ] {
        steps.push(grant(definition, json!("validation")));
    }
    for key in [
        "actions",
        "item_ids",
        "file_ids",
        "occurrence_ids",
        "metadata_fields",
        "operation_ids",
        "projection_ids",
        "report_ids",
        "fallback_policy_ids",
        "callback_origins",
    ] {
        for bad in [
            json!(null),
            json!(true),
            json!({}),
            json!([1]),
            json!(["雪".repeat(257)]),
            json!(vec![""; 10001]),
        ] {
            let mut definition = json!({"actions":["metadata:read"]});
            definition[key] = bad;
            steps.push(grant(definition, json!("validation")));
        }
    }
    steps.extend([
        grant(json!({"actions":["","wild:unrecognized","*"],"item_ids":["", "雪".repeat(256)],"metadata_fields":vec!["";10000],"revisions":{"":"", "雪":"revision"},"operator":false,"derivatives":true,"query_id":false}), json!("valid-full")),
        grant(json!({"actions":["metadata:read"],"query_id":[]}), json!("false-query")),
        json!({"op":"grant","principal":"missing","definition":{"actions":["x"],"revisions":false},"id":"validation"}),
        json!({"op":"grant","principal":"missing","definition":{"actions":["x"]},"id":"validation"}),
        json!({"op":"grant","principal":[],"definition":{"actions":["x"]},"id":"validation"}),
        grant(json!({"actions":["x"]}), json!([])),
        grant(json!({"actions":["x"]}), json!("")),
        grant(json!({"actions":["x"]}), json!(0)),
        grant(json!({"actions":["x"]}), json!([1])),
    ]);
    compare("grant definitions and error ordering", setup(), steps, true);

    let mut steps = vec![];
    for origin in [
        "https://example.invalid",
        "HTTP://Example.Invalid:80/",
        "https://example.invalid:443",
        "https://example.invalid?",
        "https://example.invalid/#",
        "https://example.invalid/?#",
        "https://[::1]:8443/",
        "http://[fe80::1%25fixture]/",
        "https://雪.invalid/",
        "http://example.invalid:",
        "ftp://example.invalid",
        "https://user:fake@example.invalid",
        "https://example.invalid/#fragment",
        "https://example.invalid/a",
        "https://example.invalid//",
        "https://example.invalid/?query",
        "https:///",
        "https://example.invalid:0",
        "https://example.invalid:65536",
        "https://example.invalid:abc",
        "https://example.invalid\\path",
        "https://example.invalid\n",
        "https://[invalid]/",
        "https://example.invalid／bad",
        "http://[v1.fixture]/",
        "https://[vaF.future:fixture]:8443/",
        "http://[v1.%fixture%scope]/",
        "http://[v1..]/",
        "http://[v0.future]/",
        "http://[V1.fixture]/",
        "http://[v.fixture]/",
        "http://[vg.fixture]/",
        "http://[v1.]/",
        "http://[v1fixture]/",
        "http://[v1.fixture]:0/",
        "http://[v1.fixture]:65536/",
        "http://user@[v1.fixture]/",
        "http://[v1.fi\nxture]/",
        "http://[127.0.0.1]/",
        "http://[fe80::1%]/",
        "http://[fe80::1%one%two]/",
        "http://[::1]suffix/",
    ] {
        steps.push(json!({"op":"origin","value":origin}));
        steps.push(grant(
            json!({"actions":["processing:request"],"callback_origins":[origin]}),
            json!("origin"),
        ));
    }
    compare("callback origins", setup(), steps, true);

    let mut definitions = setup();
    query(
        &mut definitions,
        "sql-items",
        json!({"mode":"selection","selection":{"language":"sql","profile":"default","query":"SELECT id AS item_id FROM items"}}),
        "default",
    );
    query(
        &mut definitions,
        "graphql-items",
        json!({"mode":"selection","selection":{"language":"graphql","profile":"default","query":"query($after:String){items(first:100,after:$after){nodes{id} pageInfo{endCursor hasNextPage}}}"}}),
        "default",
    );
    query(
        &mut definitions,
        "wrong-entity",
        json!({"mode":"selection","selection":{"language":"sql","profile":"default","query":"SELECT id AS file_id FROM items"}}),
        "default",
    );
    query(
        &mut definitions,
        "empty-items",
        json!({"mode":"selection","selection":{"language":"sql","profile":"default","query":"SELECT id AS item_id FROM items WHERE 0"}}),
        "default",
    );
    query(
        &mut definitions,
        "rows-mode",
        json!({"mode":"rows","selection":{"language":"sql","profile":"default","query":"SELECT id FROM items"}}),
        "default",
    );
    query(
        &mut definitions,
        "small-budget",
        json!({"mode":"selection","max_ids":1,"selection":{"language":"sql","profile":"default","query":"SELECT id AS item_id FROM items"}}),
        "default",
    );
    query(
        &mut definitions,
        "composition",
        json!({"mode":"selection","combine":"union","queries":["sql-items","graphql-items"]}),
        "default",
    );
    query(
        &mut definitions,
        "other-profile",
        json!({"mode":"selection","selection":{"language":"sql","profile":"alternate","query":"SELECT id AS item_id FROM items"}}),
        "alternate",
    );
    query(
        &mut definitions,
        "123",
        json!({"mode":"selection","selection":{"language":"sql","profile":"default","query":"SELECT id AS item_id FROM items"}}),
        "default",
    );
    query(
        &mut definitions,
        "private-sql",
        json!({"mode":"selection","selection":{"language":"sql","profile":"default","query":"SELECT digest AS item_id FROM api_tokens"}}),
        "default",
    );
    query(
        &mut definitions,
        "invalid-graphql",
        json!({"mode":"selection","selection":{"language":"graphql","profile":"default","query":"query($after:String){items(unknown:true,after:$after){nodes{id} pageInfo{endCursor hasNextPage}}}"}}),
        "default",
    );
    let mut steps = vec![];
    for id in [
        json!("missing"),
        json!("sql-items"),
        json!("graphql-items"),
        json!("wrong-entity"),
        json!("empty-items"),
        json!("rows-mode"),
        json!("small-budget"),
        json!("composition"),
        json!("other-profile"),
        json!("private-sql"),
        json!("invalid-graphql"),
        json!(["sql-items"]),
        json!(123),
    ] {
        steps.push(grant(
            json!({"actions":["metadata:read"],"query_id":id}),
            json!("query-grant"),
        ));
    }
    steps.extend([
        json!({"op":"begin"}),
        json!({"op":"sql","sql":"UPDATE saved_queries SET definition='{\"mode\":\"selection\",\"selection\":{\"language\":\"sql\",\"profile\":\"default\",\"query\":\"SELECT id AS file_id FROM items\"}}' WHERE id='sql-items'"}),
        grant(json!({"actions":["metadata:read"],"query_id":"sql-items"}), json!("uncommitted")),
        json!({"op":"finish","commit":false}),
        grant(json!({"actions":["metadata:read"],"query_id":"sql-items"}), json!("after-rollback")),
    ]);
    steps.push(json!({"op":"grant","principal":"fixture-other","definition":{"actions":["metadata:read"],"query_id":"other-profile"},"id":"alternate-grant"}));
    compare(
        "same-owner saved SQL and GraphQL preflight",
        definitions,
        steps,
        true,
    );

    let mut steps = vec![
        grant(json!({"actions":["metadata:read"]}), json!("updated")),
        json!({"op":"sql","sql":"UPDATE api_grants SET enabled=0 WHERE id='updated'"}),
        grant(json!({"actions":["metadata:read"]}), json!("updated")),
        grant(json!({"actions":["different"]}), json!("updated")),
        json!({"op":"grant","principal":"fixture-other","definition":{"actions":["x"]},"id":"updated"}),
        json!({"op":"grant","principal":"fixture-disabled","definition":{"actions":["x"]},"id":"disabled-grant"}),
    ];
    for (grants, ttl) in [
        (json!(null), json!(3600)),
        (json!([]), json!(3600)),
        (json!(vec!["fixture-grant"; 101]), json!(3600)),
        (json!(["fixture-grant"]), json!(true)),
        (json!(["fixture-grant"]), json!(1.0)),
        (json!(["fixture-grant"]), json!(0)),
        (json!(["fixture-grant"]), json!(2592001)),
        (json!(["missing"]), json!(3600)),
        (json!([null]), json!(3600)),
        (json!([[]]), json!(3600)),
    ] {
        steps.push(issue_step(grants, ttl, 1));
    }
    steps.extend([
        issue_step(json!(["fixture-grant", "fixture-grant"]),json!(1),2),
        issue_step(json!(vec!["fixture-grant";100]),json!(2592000),3),
        issue_step(json!([123]),json!(3600),4),
        json!({"op":"issue","principal":"fixture-disabled","grants":["disabled-principal-grant"],"seed":5}),
        json!({"op":"issue","principal":"fixture-other","grants":["fixture-grant"],"seed":6}),
        json!({"op":"sql","sql":"UPDATE api_grants SET enabled=0 WHERE id='fixture-grant'"}),
        issue_step(json!(["fixture-grant"]),json!(3600),7),
        grant(json!({"actions":["metadata:read"]}),json!("fixture-grant")),
        issue_step(json!(["fixture-grant"]),json!(3600),8),
        issue_step(json!(["fixture-grant"]),json!(3600),8),
    ]);
    compare(
        "grant revisions, principal enablement, token validation and digests",
        setup(),
        steps,
        true,
    );

    let steps = vec![
        json!({"op":"begin"}),
        json!({"op":"principal","id":"outer"}),
        grant(json!({"actions":["x"]}), json!("outer")),
        json!({"op":"begin"}),
        grant(json!({"actions":["inner"]}), json!("outer")),
        issue_step(json!(["outer"]), json!(3600), 12),
        json!({"op":"finish","commit":false}),
        issue_step(json!(["outer"]), json!(3600), 13),
        json!({"op":"finish","commit":false}),
        json!({"op":"begin"}),
        grant(json!({"actions":["x"]}), json!("committed")),
        issue_step(json!(["committed"]), json!(3600), 14),
        json!({"op":"finish","commit":true}),
    ];
    compare("nested and outer rollback", setup(), steps, true);
    let steps = vec![
        json!({"op":"principal","id":""}),
        json!({"op":"principal","id":"valid"}),
        grant(json!({}), json!("invalid")),
        json!({"op":"grant","principal":"missing","definition":{"actions":["x"]},"id":"missing"}),
        grant(json!({"actions":["x"]}), json!("valid")),
        issue_step(json!([]), json!(3600), 15),
        issue_step(json!(["missing"]), json!(3600), 16),
        issue_step(json!(["fixture-grant"]), json!(3600), 17),
    ];
    compare("read-only precedence", setup(), steps, false);
    let steps = vec![
        json!({"op":"sql","sql":"CREATE TRIGGER fixture_audit_failure BEFORE INSERT ON api_audit BEGIN SELECT RAISE(ABORT,'fixture audit failure'); END"}),
        grant(json!({"actions":["x"]}), json!("failed-new")),
        grant(json!({"actions":["changed"]}), json!("fixture-grant")),
        issue_step(json!(["fixture-grant"]), json!(3600), 21),
        json!({"op":"begin"}),
        json!({"op":"principal","id":"preserved-outer"}),
        grant(json!({"actions":["changed"]}), json!("fixture-grant")),
        issue_step(json!(["fixture-grant"]), json!(3600), 22),
        json!({"op":"finish","commit":true}),
        json!({"op":"sql","sql":"DROP TRIGGER fixture_audit_failure"}),
        grant(json!({"actions":["reused"]}), json!("fixture-grant")),
        issue_step(json!(["fixture-grant"]), json!(3600), 23),
    ];
    compare(
        "audit failure atomicity and owner reuse",
        setup(),
        steps,
        true,
    );
}

#[test]
fn token_entropy_clock_and_validation_follow_frozen_order() {
    let (_root, session) = owner(true, &setup());
    let store = session.store().unwrap();
    let calls = std::cell::RefCell::new(vec![]);
    let invalid = issue_with_sources(
        store,
        &json!("fixture-principal"),
        &json!(["missing"]),
        &json!(3600),
        |_| panic!("entropy before grant validation"),
        || panic!("clock before grant validation"),
    );
    assert_eq!(invalid.unwrap_err().0, "invalid_token_grant");
    let token = issue_with_sources(
        store,
        &json!("fixture-principal"),
        &json!(["fixture-grant", "fixture-grant"]),
        &json!(3600),
        |bytes| {
            calls.borrow_mut().push("entropy");
            bytes.fill(0x35);
            Ok(())
        },
        || {
            calls.borrow_mut().push("clock");
            4102444800.25
        },
    )
    .unwrap();
    assert_eq!(*calls.borrow(), ["entropy", "clock"]);
    let id = token["token_id"].as_str().unwrap();
    let secret = token["token"].as_str().unwrap();
    assert_eq!(id.len(), 24);
    assert!(
        id.bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    assert_eq!(secret.len(), 68);
    assert_eq!(&secret[..25], format!("{id}."));
    assert_eq!(URL_SAFE_NO_PAD.decode(&secret[25..]).unwrap().len(), 32);
    assert_eq!(token["expires"], 4102448400.25);
    let rows = snapshot(store);
    assert_eq!(rows["tokens"][0]["digest"], sha256(secret));
    assert_eq!(
        rows["tokens"][0]["grants"],
        "[\"fixture-grant\", \"fixture-grant\"]"
    );
    assert_eq!(rows["tokens"][0]["revoked"], 0);
    assert!(
        !encode(&rows).contains(secret),
        "cleartext fixture secret must not enter catalog rows"
    );
    let before = snapshot(store);
    let error = issue_with_sources(
        store,
        &json!("fixture-principal"),
        &json!(["fixture-grant"]),
        &json!(1),
        |_| Err(Error("fixture entropy failure".into())),
        || panic!("clock after failed entropy"),
    )
    .unwrap_err();
    assert_eq!(error.0, "fixture entropy failure");
    assert_eq!(snapshot(store), before);
}

#[test]
fn grant_update_reenables_only_grant_and_increments_every_revision() {
    let (_root, mut session) = owner(true, &setup());
    let definition = json!({"actions":["metadata:read"],"operator":true});
    grant_put(
        &mut session,
        &json!("fixture-disabled"),
        &definition,
        Some(&json!("fixture-update")),
    )
    .unwrap();
    session
        .store()
        .unwrap()
        .db
        .execute(
            "UPDATE api_grants SET enabled=0 WHERE id='fixture-update'",
            [],
        )
        .unwrap();
    for _ in 0..2 {
        grant_put(
            &mut session,
            &json!("fixture-disabled"),
            &definition,
            Some(&json!("fixture-update")),
        )
        .unwrap();
    }
    let store = session.store().unwrap();
    let actual: (i64, i64) = store
        .db
        .query_row(
            "SELECT revision,enabled FROM api_grants WHERE id='fixture-update'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(actual, (3, 1));
    principal_put(store, &json!("default"), &json!("fixture-disabled")).unwrap();
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT enabled FROM api_principals WHERE id='fixture-disabled'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let before = snapshot(store);
    assert_eq!(
        grant_put(
            &mut session,
            &json!("fixture-other"),
            &definition,
            Some(&json!("fixture-update"))
        )
        .unwrap_err()
        .0,
        "grant_principal_conflict"
    );
    assert_eq!(snapshot(session.store().unwrap()), before);
}

#[test]
fn failed_audit_and_deferred_commit_roll_back_owned_mutations() {
    let (_root, mut session) = owner(true, &setup());
    session.store().unwrap().db.execute_batch("CREATE TRIGGER fixture_audit_failure BEFORE INSERT ON api_audit BEGIN SELECT RAISE(ABORT,'fixture audit failure'); END").unwrap();
    let before = snapshot(session.store().unwrap());
    assert_eq!(
        grant_put(
            &mut session,
            &json!("fixture-principal"),
            &json!({"actions":["x"]}),
            Some(&json!("never-committed"))
        )
        .unwrap_err()
        .0,
        "fixture audit failure"
    );
    assert_eq!(snapshot(session.store().unwrap()), before);
    assert_eq!(
        issue_with_sources(
            session.store().unwrap(),
            &json!("fixture-principal"),
            &json!(["fixture-grant"]),
            &json!(1),
            |bytes| {
                bytes.fill(1);
                Ok(())
            },
            || 4102444800.0
        )
        .unwrap_err()
        .0,
        "fixture audit failure"
    );
    assert_eq!(snapshot(session.store().unwrap()), before);
    session
        .store()
        .unwrap()
        .db
        .execute_batch("DROP TRIGGER fixture_audit_failure; PRAGMA defer_foreign_keys=ON")
        .unwrap();
    assert!(
        principal_put(
            session.store().unwrap(),
            &json!("missing-profile"),
            &json!("orphan")
        )
        .is_err()
    );
    assert_eq!(snapshot(session.store().unwrap()), before);
    grant_put(
        &mut session,
        &json!("fixture-principal"),
        &json!({"actions":["x"]}),
        Some(&json!("after-failures")),
    )
    .unwrap();
}

#[test]
fn admin_preflight_is_temporary_and_does_not_change_http_evaluation() {
    let mut fixture = setup();
    query(
        &mut fixture,
        "graphql-items",
        json!({"mode":"selection","selection":{"language":"graphql","profile":"default","query":"query($after:String){items(first:100,after:$after){nodes{id} pageInfo{endCursor hasNextPage}}}"}}),
        "default",
    );
    let (root, mut session) = owner(true, &fixture);
    grant_put(
        &mut session,
        &json!("fixture-principal"),
        &json!({"actions":["metadata:read"],"query_id":"graphql-items"}),
        Some(&json!("graphql-grant")),
    )
    .unwrap();
    let error = session
        .query_call(
            "default",
            "select",
            &json!({"id":"graphql-items","_http":true,"grant_preflight":true,"local_grant_preflight":true}),
        )
        .unwrap_err();
    assert_eq!(
        error.0,
        "GraphQL HTTP evaluation requires authorization context"
    );
    assert!(
        session
            .query_call("default", "select", &json!({"id":"graphql-items"}))
            .is_ok()
    );
    assert!(
        !session
            .store()
            .unwrap()
            .db
            .pragma_query_value(None, "query_only", |r| r.get::<_, bool>(0))
            .unwrap()
    );
    session.close().unwrap();
    let store = Store::open(&root.path().join("catalog.db"), false, false).unwrap();
    let mut evaluation = Evaluation::new(store, "default", 5000, 10000, true).unwrap();
    assert_eq!(
        evaluation
            .select(&json!({"query_id":"graphql-items"}))
            .err()
            .unwrap()
            .0,
        "GraphQL HTTP evaluation requires authorization context"
    );
}

#[test]
fn token_authentication_retains_enablement_digest_revocation_and_expiry_gates() {
    for (principal, change, allowed) in [
        ("fixture-principal", "", true),
        ("fixture-disabled", "", false),
        (
            "fixture-principal",
            "UPDATE api_tokens SET revoked=1",
            false,
        ),
        (
            "fixture-principal",
            "UPDATE api_tokens SET expires=0",
            false,
        ),
        (
            "fixture-principal",
            "UPDATE api_tokens SET digest='fixture-wrong-digest'",
            false,
        ),
    ] {
        let (root, mut session) = owner(true, &setup());
        let grant = if principal == "fixture-disabled" {
            "disabled-principal-grant"
        } else {
            "fixture-grant"
        };
        let token = issue_with_sources(
            session.store().unwrap(),
            &json!(principal),
            &json!([grant]),
            &json!(3600),
            |bytes| {
                bytes.fill(0x41);
                Ok(())
            },
            || 4102444800.0,
        )
        .unwrap();
        if !change.is_empty() {
            session.store().unwrap().db.execute(change, []).unwrap();
        }
        session.close().unwrap();
        let result = crate::access::Access::authenticate(
            Store::open(&root.path().join("catalog.db"), false, false).unwrap(),
            token["token"].as_str().unwrap(),
        );
        assert_eq!(result.is_ok(), allowed, "fixture {principal}: {change}");
        if let Err(error) = result {
            assert_eq!(error.0, "unauthorized");
        }
    }
}

#[test]
fn readonly_errors_occur_after_validation_and_entropy_but_before_writes() {
    let (_root, mut session) = owner(false, &setup());
    let before = snapshot(session.store().unwrap());
    assert_eq!(
        principal_put(session.store().unwrap(), &json!("default"), &json!(""))
            .unwrap_err()
            .0,
        "invalid_principal"
    );
    assert_eq!(
        grant_put(
            &mut session,
            &json!("missing"),
            &json!({"actions":["x"]}),
            None
        )
        .unwrap_err()
        .0,
        "unknown_principal"
    );
    let generated = std::cell::Cell::new(false);
    let clock = std::cell::Cell::new(false);
    let error = issue_with_sources(
        session.store().unwrap(),
        &json!("fixture-principal"),
        &json!(["fixture-grant"]),
        &json!(3600),
        |bytes| {
            generated.set(true);
            bytes.fill(1);
            Ok(())
        },
        || {
            clock.set(true);
            4102444800.0
        },
    )
    .unwrap_err();
    assert_eq!(
        error.0,
        "a read-only session cannot start a write transaction"
    );
    assert!(generated.get() && clock.get());
    assert_eq!(snapshot(session.store().unwrap()), before);
}

#[test]
fn preflight_keeps_uncommitted_snapshot_and_restores_owner_after_graphql_failure() {
    let mut fixture = setup();
    for (id, field) in [("query", "items"), ("bad-query", "unknown")] {
        query(
            &mut fixture,
            id,
            json!({"mode":"selection","selection":{"language":"graphql","profile":"default","query":format!("query($after:String){{{field}(first:100,after:$after){{nodes{{id}} pageInfo{{endCursor hasNextPage}}}}}}")}}),
            "default",
        );
    }
    let (_root, mut session) = owner(true, &fixture);
    let scope = session.begin().unwrap();
    session
        .store()
        .unwrap()
        .db
        .execute(
            "INSERT INTO api_principals(id,profile) VALUES ('uncommitted','default')",
            [],
        )
        .unwrap();
    let before = snapshot(session.store().unwrap());
    assert!(
        grant_put(
            &mut session,
            &json!("uncommitted"),
            &json!({"actions":["x"],"query_id":"bad-query"}),
            Some(&json!("failed"))
        )
        .is_err()
    );
    assert_eq!(snapshot(session.store().unwrap()), before);
    grant_put(
        &mut session,
        &json!("uncommitted"),
        &json!({"actions":["x"],"query_id":"query"}),
        Some(&json!("visible")),
    )
    .unwrap();
    assert!(!session.store().unwrap().db.is_autocommit());
    session.finish(scope, false).unwrap();
    assert_eq!(
        session
            .store()
            .unwrap()
            .db
            .query_row(
                "SELECT count(*) FROM api_grants WHERE id='visible'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        session
            .store()
            .unwrap()
            .db
            .query_row(
                "SELECT count(*) FROM api_principals WHERE id='uncommitted'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn grant_owner_refuses_detached_and_closed_sessions_then_reconnects() {
    let (_root, mut session) = owner(true, &setup());
    session.detach().unwrap();
    let error = grant_put(
        &mut session,
        &json!("fixture-principal"),
        &json!({"actions":["x"]}),
        Some(&json!("fixture-grant")),
    )
    .unwrap_err();
    assert!(error.0.contains("detached"), "{error}");
    session.reconnect().unwrap();
    grant_put(
        &mut session,
        &json!("fixture-principal"),
        &json!({"actions":["x"]}),
        Some(&json!("fixture-grant")),
    )
    .unwrap();
    session.close().unwrap();
    let error = grant_put(
        &mut session,
        &json!("fixture-principal"),
        &json!({"actions":["x"]}),
        Some(&json!("fixture-grant")),
    )
    .unwrap_err();
    assert_eq!(error.0, "native store is closed");
}

#[test]
#[ignore = "requires pinned frozen checkout and Python 3.14.6; intentional fail-closed divergence witness"]
fn inherited_scalar_cache_confusion_remains_fail_closed_native() {
    // Deliberately synthetic and entirely local. This is a regression witness
    // for an unresolved inherited behavior, NOT a request to reproduce it.
    for (ids, expected_members) in [
        (json!([true, 1, 1.0, true]), "item-a"),
        (json!([1.0, true, 1, 1.0]), "item-b"),
    ] {
        let steps = vec![
            grant(
                json!({"actions":["metadata:read"],"item_ids":["item-a"]}),
                json!("1"),
            ),
            grant(
                json!({"actions":["processing:request"],"item_ids":["item-b"]}),
                json!("1.0"),
            ),
            issue_step(ids, json!(3600), 61),
            json!({"op":"access_members","seed":61,"checks":[0,1,2,3]}),
        ];
        let scenario = json!({"setup":setup(),"steps":steps,"writable":true});
        let reference = frozen("inherited scalar cache witness", &scenario);
        let candidate = native(&scenario);
        for index in 0..3 {
            assert_eq!(candidate[index], reference[index]);
        }
        assert_eq!(
            candidate[3]["result"],
            json!({"error":"unauthorized","status":401})
        );
        let observed = reference[3]["result"]["ok"].as_array().unwrap();
        assert_eq!(observed.len(), 4);
        for grant in observed {
            assert_eq!(grant["members"], json!([expected_members]));
        }
        assert_ne!(
            observed[0]["actions"],
            observed[if expected_members == "item-a" { 2 } else { 1 }]["actions"]
        );
        assert_eq!(candidate[3]["state"], reference[3]["state"]);
    }
    // Ordinary string identities remain distinct and match the frozen owner.
    compare(
        "distinct string grant membership control",
        setup(),
        vec![
            grant(
                json!({"actions":["metadata:read"],"item_ids":["item-a"]}),
                json!("1"),
            ),
            grant(
                json!({"actions":["processing:request"],"item_ids":["item-b"]}),
                json!("1.0"),
            ),
            issue_step(json!(["1", "1.0", "1"]), json!(3600), 62),
            json!({"op":"access_members","seed":62,"checks":[0,1,2]}),
        ],
        true,
    );
}

#[test]
fn malformed_scalar_token_lists_keep_existing_native_refusal() {
    for grants in [
        "null",
        "{}",
        "[[1]]",
        "[{\"id\":\"fixture-grant\"}]",
        "[true]",
        "[1]",
        "[1.0]",
        "[null]",
        "not json",
    ] {
        let (root, mut session) = owner(true, &setup());
        let token = issue_with_sources(
            session.store().unwrap(),
            &json!("fixture-principal"),
            &json!(["fixture-grant"]),
            &json!(3600),
            |bytes| {
                bytes.fill(0x41);
                Ok(())
            },
            || 4102444800.0,
        )
        .unwrap();
        session
            .store()
            .unwrap()
            .db
            .execute("UPDATE api_tokens SET grants=?", [grants])
            .unwrap();
        session.close().unwrap();
        let error = crate::access::Access::authenticate(
            Store::open(&root.path().join("catalog.db"), false, false).unwrap(),
            token["token"].as_str().unwrap(),
        )
        .err()
        .unwrap();
        assert_eq!(error.0, "unauthorized", "{grants}");
    }
}

#[test]
fn shared_origin_and_grant_owner_keep_frozen_ipvfuture_syntax() {
    let (_root, mut session) = owner(true, &setup());
    for (url, host, port) in [
        ("http://[v1.fixture]/", "v1.fixture", 80),
        (
            "https://[vaF.Future:fixture]:8443/",
            "vaf.future:fixture",
            8443,
        ),
        ("http://[v1.%scope%tail]/", "v1.%scope%tail", 80),
        ("http://[v1..]/", "v1..", 80),
        ("https://[::1]/", "::1", 443),
        (
            "http://[fe80::1%25fixture]:8080/",
            "fe80::1%25fixture",
            8080,
        ),
    ] {
        let origin = crate::notifications::callback_origin(url).unwrap();
        assert_eq!(origin.host, host);
        assert_eq!(origin.port, port);
        grant_put(
            &mut session,
            &json!("fixture-principal"),
            &json!({"actions":["processing:request"],"callback_origins":[url]}),
            Some(&json!("origin-fixture")),
        )
        .unwrap();
    }
    let before = snapshot(session.store().unwrap());
    for url in [
        "http://[V1.fixture]/",
        "http://[v.fixture]/",
        "http://[vg.fixture]/",
        "http://[v1.]/",
        "http://[127.0.0.1]/",
        "http://[fe80::1%]/",
        "http://[fe80::1%one%two]/",
        "http://user@[v1.fixture]/",
        "http://[v1.fixture]:0/",
        "http://[v1.fixture]:65536/",
        "http://[v1.fi\nxture]/",
        "http://[v1.fixture]\\path",
    ] {
        assert!(crate::notifications::callback_origin(url).is_err(), "{url}");
        let error = grant_put(
            &mut session,
            &json!("fixture-principal"),
            &json!({"actions":["processing:request"],"callback_origins":[url]}),
            Some(&json!("origin-fixture")),
        )
        .unwrap_err();
        assert_eq!(error.0, "invalid_callback_url", "{url}");
        assert_eq!(snapshot(session.store().unwrap()), before);
    }
}

#[test]
fn typed_transaction_preserves_failure_and_cleanup_provenance() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let (_root, session) = owner(true, &setup());
    let store = session.store().unwrap();
    let before = snapshot(store);
    let failure = store
        .transaction_detailed::<(), Failure>(|db| {
            db.execute(
                "INSERT INTO api_principals(id,profile) VALUES ('rollback-tag','default')",
                [],
            )?;
            fail(AccessFailure::InvalidGrant)
        })
        .unwrap_err();
    assert!(matches!(
        failure,
        Failure::Access(AccessFailure::InvalidGrant)
    ));
    assert_eq!(snapshot(store), before);
    let panic = catch_unwind(AssertUnwindSafe(|| {
        let _ = store.transaction_detailed::<(), Failure>(|db| {
            db.execute(
                "INSERT INTO api_principals(id,profile) VALUES ('rollback-panic','default')",
                [],
            )?;
            panic!("fixture typed transaction unwind")
        });
    }));
    assert!(panic.is_err());
    assert_eq!(snapshot(store), before);
    store
        .db
        .execute_batch("PRAGMA defer_foreign_keys=ON")
        .unwrap();
    let failure = store
        .transaction_detailed::<(), Failure>(|db| {
            db.execute(
                "INSERT INTO api_principals(id,profile) VALUES ('rollback-commit','missing')",
                [],
            )?;
            Ok(())
        })
        .unwrap_err();
    assert!(matches!(failure, Failure::Native(_)));
    assert_eq!(snapshot(store), before);
    principal_put_detailed(store, &json!("default"), &json!("reuse")).unwrap();

    // A lost nested savepoint makes rollback fail and poisons the owner. It
    // overrides a business tag rather than returning an ordinary AccessError.
    let outer = store.begin_transaction().unwrap();
    let failure = store
        .transaction_detailed::<(), Failure>(|db| {
            db.execute_batch("ROLLBACK; BEGIN IMMEDIATE")?;
            fail(AccessFailure::UnknownPrincipal)
        })
        .unwrap_err();
    match failure {
        Failure::Native(error) => assert!(error.0.contains("native rollback failed")),
        Failure::Access(_) => panic!("cleanup failure retained a business tag"),
    }
    assert!(store.finish_transaction(outer, Ok(())).is_err());
    assert!(matches!(
        principal_put_detailed(store, &json!("default"), &json!("refuse")),
        Err(Failure::Native(_))
    ));

    let (_root, readonly) = owner(false, &setup());
    assert!(matches!(
        principal_put_detailed(
            readonly.store().unwrap(),
            &json!("default"),
            &json!("readonly")
        ),
        Err(Failure::Native(_))
    ));
}
