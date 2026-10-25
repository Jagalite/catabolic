#!/usr/bin/env python3
"""Reproduce only native40 metadata schema signature/manifest, no Rust build.

All databases are private in-memory schema fixtures. Prior migration bytes and
prior signatures are read-only. This is generation, not runtime qualification.
"""
import argparse, hashlib, importlib.util, json
from contextlib import closing
from pathlib import Path
spec = importlib.util.spec_from_file_location("foundation", Path(__file__).with_name("native_catalog_schema.py"))
foundation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(foundation)
REPO = Path(__file__).resolve().parents[1]
RESOURCES = foundation.RESOURCE_PATH
ALLOWED_CHANGED = {
    "catalog_metadata_subjects", "catalog_metadata_history", "catalog_metadata_sources", "catalog_metadata_projection",
    "catalog_metadata_history_insert", "catalog_metadata_source_update",
    "catalog_metadata_projection_insert", "catalog_metadata_projection_update",
}

def generate():
    prior_files = list((REPO / RESOURCES).glob("native-schema-*.json"))
    prior_hashes = {str(p.relative_to(REPO)): hashlib.sha256(p.read_bytes()).hexdigest() for p in prior_files if p.name != "native-schema-40.json"}
    migrations = sorted((REPO / RESOURCES / "migrations").glob("*.sql"))
    if [int(p.name[:3]) for p in migrations] != list(range(1, 41)):
        raise ValueError("native40 requires exact contiguous migrations")
    with closing(foundation.legacy_db(REPO)) as db:
        db.execute("PRAGMA recursive_triggers=ON")
        for path in migrations[32:39]: db.executescript(path.read_text())
        prior = foundation.objects(db)
        if prior != json.loads((REPO / RESOURCES / "native-schema-39.json").read_text()):
            raise ValueError("schema39 prerequisite signature differs")
        generation = db.execute("SELECT generation FROM catalog_state").fetchone()
        db.executescript(migrations[39].read_text())
        current = foundation.objects(db)
        by_name = {(x[0], x[1]): x for x in current}
        changes = [x[1] for x in prior if by_name.get((x[0], x[1])) != x]
        if not set(changes) <= ALLOWED_CHANGED:
            raise ValueError(f"unexpected historical DDL changes: {set(changes) - ALLOWED_CHANGED}")
        if db.execute("PRAGMA foreign_key_check").fetchall(): raise ValueError("native40 foreign key check failed")
        if db.execute("SELECT generation FROM catalog_state").fetchone() != generation:
            raise ValueError("schema-only migration changed generation")
    for name, digest in prior_hashes.items():
        if hashlib.sha256((REPO / name).read_bytes()).hexdigest() != digest:
            raise ValueError("prior manifest changed during generation")
    signature = json.dumps(current, indent=2) + "\n"
    manifest = {
        "schema_version": 40, "prior_schema": 39,
        "qualification_scope": "schema-only resource generation, not runtime migration/owner qualification",
        "changed_historical_objects": changes,
        "prior_signature_sha256": prior_hashes,
        "input_sha256": {str(p.relative_to(REPO)): hashlib.sha256(p.read_bytes()).hexdigest() for p in migrations},
        "signature_sha256": hashlib.sha256(signature.encode()).hexdigest(),
        "external_execution_cleanup_authority": False,
    }
    return {RESOURCES / "native-schema-40.json": signature, Path("tests/parity/native-metadata-schemas/schema-manifest.json"): json.dumps(manifest, indent=2) + "\n"}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, required=True)
    output = parser.parse_args().output_dir.resolve()
    if output.exists() or output.is_relative_to(REPO): parser.error("output must be new and outside source")
    artifacts = generate()
    output.mkdir()
    for path, text in artifacts.items():
        target = output / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)
        print(target)
if __name__ == "__main__": main()
