# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen access-owner oracle. Only temporary catalogs and synthetic credentials.

The input is a checked-in test scenario, never an HTTP or production dispatcher.
No account, credential file, external recipient, or network is involved.
"""
import hashlib
import importlib
import json
import pathlib
import subprocess
import sys
import tempfile

if not __debug__ or sys.version_info[:3] != (3, 14, 6):
    raise SystemExit("access owner oracle requires assertion-enabled Python 3.14.6")
source = pathlib.Path(sys.argv[1]).resolve()
revision = subprocess.check_output(
    ["git", "-C", str(source.parent), "rev-parse", "HEAD"], text=True
).strip()
if revision != "453fca983222c6665a48775eef67c489c96b527e":
    raise SystemExit("access owner oracle is not the frozen Git checkout")
subprocess.run(
    ["git", "-C", str(source.parent), "diff", "--exit-code", "HEAD", "--",
     "src/catabolic/access/__init__.py", "src/catabolic/saved_queries.py",
     "src/catabolic/selection.py", "src/catabolic/callback_urls.py", "src/catabolic/store.py"],
    check=True, stdout=subprocess.DEVNULL,
)
sys.path.insert(0, str(source))
access = importlib.import_module("catabolic.access")
from catabolic.store import Store
if pathlib.Path(access.__file__).resolve() != source / "catabolic/access/__init__.py":
    raise SystemExit("access oracle imported a different checkout")

QUERIES = {
    "principals": "SELECT id,profile,enabled FROM api_principals ORDER BY id",
    "grants": "SELECT id,principal,definition,revision,enabled FROM api_grants ORDER BY id",
    "tokens": "SELECT id,principal,digest,grants,expires,revoked FROM api_tokens ORDER BY id",
    "audit": "SELECT id,principal,action,resource FROM api_audit ORDER BY id",
    "epoch": "SELECT generation FROM fallback_epoch WHERE id=1",
}

def main(scenario):
    with tempfile.TemporaryDirectory(prefix="catabolic-access-oracle-") as root:
        path = pathlib.Path(root) / "catalog.db"
        Store.initialize(path)
        with Store(path, writable=True) as store:
            for sql in scenario.get("setup", []):
                with store.transaction() as db:
                    db.execute(sql)
        store = Store(path, writable=scenario.get("writable", True))
        scopes = []
        output = []
        generated = {}
        try:
            for step in scenario["steps"]:
                op = step["op"]
                try:
                    if op == "principal":
                        result = access.principal_put(store, step.get("profile", "default"), step["id"])
                    elif op == "grant":
                        result = access.grant_put(store, step["principal"], step["definition"], step.get("id"))
                        if not step.get("id"):
                            generated[result["grant_id"]] = "generated-grant-" + str(len(generated))
                    elif op == "issue":
                        import base64
                        raw = bytes([step.get("seed", 53)]) * 44
                        identifier = raw[:12].hex()
                        secret = identifier + "." + base64.urlsafe_b64encode(raw[12:]).decode().rstrip("=")
                        access.secret_pair = lambda: (identifier, secret, hashlib.sha256(secret.encode()).hexdigest())
                        old_time = access.time.time
                        access.time.time = lambda: step.get("now", 4102444800.25)
                        try:
                            result = access.issue(store, step["principal"], step["grants"], step.get("ttl", 3600))
                        finally:
                            access.time.time = old_time
                    elif op == "access_members":
                        import base64
                        raw = bytes([step["seed"]]) * 44
                        secret = raw[:12].hex() + "." + base64.urlsafe_b64encode(raw[12:]).decode().rstrip("=")
                        authenticated = access.authenticate(store, secret)
                        result = []
                        for index in step["checks"]:
                            grant = authenticated.grants[index]
                            result.append({"id": grant["_id"], "actions": grant["actions"],
                                           "members": sorted(authenticated.members(grant))})
                    elif op == "origin":
                        from catabolic.callback_urls import origin
                        try:
                            result = list(origin(step["value"]))
                        except ValueError:
                            result = {"invalid": True}
                    elif op == "sql":
                        with store.transaction() as db:
                            db.execute(step["sql"])
                        result = None
                    elif op == "begin":
                        scope = store.transaction()
                        scope.__enter__()
                        scopes.append(scope)
                        result = None
                    elif op == "finish":
                        scope = scopes.pop()
                        if step["commit"]:
                            scope.__exit__(None, None, None)
                        else:
                            error = RuntimeError("fixture rollback")
                            scope.__exit__(RuntimeError, error, None)
                        result = None
                    else:
                        raise AssertionError(op)
                    outcome = {"ok": result}
                except Exception as error:
                    outcome = {"error": str(error), "status": error.status if isinstance(error, access.AccessError) else None}
                snapshot = {key: store.rows(sql) for key, sql in QUERIES.items()}
                snapshot["in_transaction"] = store.db.in_transaction
                row = {"result": outcome, "state": snapshot}
                def normalize(value):
                    if isinstance(value, str):
                        return generated.get(value, value)
                    if isinstance(value, list):
                        return [normalize(v) for v in value]
                    if isinstance(value, dict):
                        return {k: normalize(v) for k, v in value.items()}
                    return value
                row = normalize(row)
                row["state"]["grants"].sort(key=lambda grant: grant["id"])
                output.append(row)
        finally:
            while scopes:
                scopes.pop().__exit__(RuntimeError, RuntimeError("fixture cleanup"), None)
            store.close()
        return output

print(json.dumps(main(json.load(sys.stdin)), ensure_ascii=False))
