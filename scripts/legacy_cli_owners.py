#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Regenerate explicit leaf ownership coverage; parser coverage is separate."""

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ADAPTED = {
    "processor": "put show jobs enqueue claim tick job retry cancel heartbeat dispatch complete",
    "catalog-refresh": "enable disable pending run",
    "artifact": "capabilities bind recipe enqueue list recipes run recover show",
    "rendition": "exclude allow policy import-receipt receipt-source define register list definitions show definition",
    "proposal": "put list show accept reject history",
    "expected": "put report",
    "copies": "put show plan",
    "process": "enqueue run list facts show cancel retry attempts",
    "content": "duplicates search",
    "fallback": "schema put show list bind preview execute resolve enable disable history",
    "component": "schema show extract package",
    "rule": "put list show stats enable disable preview apply run",
    "target": "list show import",
    "spec": "schema docs check validate roundtrip diff",
    "layout": "presets list show put preview apply",
    "db": "status upgrade",
    "profile": "list add",
    "location": "list scan-policy trust bind",
    "catalog": "list bind retained",
    "item": "status note worklog require requirements resolve types list show put",
    "tag": "list put rename alias parent add remove assignments",
    "association": "list put disable",
    "relationship": "list put disable",
    "mapping": "list put disable",
    "program": "export import",
    "operation": "types schema put show list",
    "projection": "schema put show verify recover enable-auto disable-auto mapping-capabilities http-operation-list http-operation-put http-operation-show http-operation-disable http-operation-retry",
    "watcher": "put show enable disable run history preview list pending schema",
    "consumer": "plex-login plex-login-complete import metadata metadata-match metadata-matches connection-put discover bind create bindings connections attempts events creations publications disable enable retry verify-indexing run",
    "notify": "put status run disable retry",
    "inbox": "list show summary",
}
SINGLES = {
    "supervise",
    "sidecar",
    "init",
    "status",
    "export",
    "docs",
    "manifest",
    "graphql",
    "query",
    "remount",
    "scan",
    "files",
    "sync",
    "verify",
    "recover",
}
OWNERS = {
    "item": "catabolic_store::{query,curation,item_workflow}",
    "profile": "catabolic_store::embedding",
    "tag": "catabolic_store::{query,tagging}",
    "association": "catabolic_store::{query,curation}",
    "relationship": "catabolic_store::{query,curation}",
    "mapping": "catabolic_store::{query,embedding}",
    "artifact": "catabolic_runtime::artifacts; catabolic_store::{artifact_mutation,public_reads}",
    "process": "catabolic_runtime::{public_processing,media}; catabolic_store::{media_mutation,public_reads}",
    "content": "catabolic_store::public_reads",
    "rendition": "catabolic_runtime::receipts; catabolic_store::{output_mutation,definitions,public_reads}",
    "scan": "catabolic_runtime::public_scan -> observation",
    "sync": "catabolic_store::filesystem_mutation",
    "verify": "catabolic_store::filesystem_mutation",
    "recover": "catabolic_store::filesystem_mutation",
    "query": "catabolic_store::{sql,selection,definitions}",
    "graphql": "catabolic_store::graphql",
    "docs": "embedded frozen documentation + native search/rendering",
    "spec": "catabolic_store::interchange + embedded frozen schemas/artifact checks",
    "consumer": "catabolic_runtime::{consumers,plex_login,plex_import,plex_matching,plex_metadata}",
    "notify": "catabolic_runtime::notifications; catabolic_store::notifications",
    "watcher": "catabolic_runtime::watchers; catabolic_store::{watcher,plans}",
    "fallback": "catabolic_runtime::fallback; catabolic_store::{fallback_mutation,public_controls}",
    "component": "catabolic_runtime::component_packaging; catabolic_store::component_mutation",
    "target": "catabolic_runtime::imports; catabolic_store::targets",
    "export": "catabolic_store::{exports,export_bundle,manifest}",
    "manifest": "catabolic_store::manifest",
    "program": "catabolic_store::programs",
    "rule": "catabolic_runtime::rules; catabolic_store::definitions",
    "operation": "catabolic_runtime::{media,artifacts}; catabolic_store::{media_mutation,public_reads}",
    "projection": "catabolic_store::{definitions,filesystem_mutation,http_operations,public_controls}; catabolic_runtime::catalog_refresh",
    "catalog-refresh": "catabolic_runtime::catalog_refresh",
    "layout": "catabolic_store::{definitions,layout_mutation}",
    "proposal": "catabolic_store::curation",
    "sidecar": "catabolic_store::curation",
    "expected": "catabolic_store::curation",
    "copies": "catabolic_store::{definitions,public_controls}",
    "processor": "catabolic_runtime::processors; catabolic_store::processor_mutation",
    "location": "catabolic_store::{embedding,public_reads}; catabolic_runtime::observation",
    "catalog": "catabolic_store::{embedding,public_reads,work_inbox}",
    "remount": "catabolic_store::remount",
    "inbox": "catabolic_store::work_inbox",
    "supervise": "catabolic_runtime::source_events -> watchers",
    "files": "catabolic_store::query",
    "init": "catabolic_store::migration",
    "status": "catabolic_store::embedding",
    "db": "catabolic_store::migration",
}
PARTIAL = {
    "projection mapping-capabilities": "Exact values are embedded natively; frozen set-derived adapter order varies with Python hash seed, so arbitrary-seed stdout byte order remains an explicit gap",
    "operation put": "Analysis and render branches are adapted; external-definition admission remains refused",
    "rule run": "--worker orchestration remains refused; default native rule execution is adapted",
    "scan": "Shared existing native Application.scan owner, with known continuation/barrier/signal qualification gaps retained",
    "catalog bind": "Implicit creation of an omitted output root remains refused; hardlink safety restrictions remain",
    "sync": "Existing native symlink/hardlink/fallback mutation qualification gates remain enforced",
    "recover": "Existing native symlink/hardlink/fallback recovery qualification gates remain enforced",
    "fallback execute": "Native fallback publication remains restricted to explicitly disposable debug fixtures; legacy flags cannot bypass it",
    "artifact run": "Native execution owner exists; exhaustive legacy cancellation/output behavior is not qualified",
    "spec roundtrip": "Unknown fields and large integer values are preserved; arbitrary extension insertion-order byte parity remains open",
}
GAPS = {
    "watch": "Aggregate scan/process watch-loop facade is not implemented",
    "maintenance": "Aggregate maintenance orchestration and its report renderer are not implemented",
    "supervise": "Preserved supervise lifecycle facade is not implemented",
    "identify": "TMDB candidate-intake business owner is not implemented",
    "refresh": "Preserved legacy refresh-adapter owner is not implemented",
    "api": "Administrative parser exists; native access-control CLI adapters/secret-file contracts remain missing",
    "inbox": "Underlying native query owners exist; the preserved inbox evidence adapter is missing",
    "projection": "Underlying native planning/mapping owners exist; this preserved operation adapter is missing",
    "fallback": "Underlying native fallback owners exist; this preserved operation adapter is missing",
}
# Exact reviewed transport entrypoints override family-level implementation
# associations. Neither form constitutes a behavioral or safety qualification.
ENTRYPOINTS = {
    "docs": "catabolic_cli::legacy::dispatch::documentation (embedded frozen guides)",
    "item types": "embedded frozen resource: types",
    "layout presets": "embedded frozen resource: presets",
    "operation types": "embedded frozen resource: operation_types",
    "operation schema": "embedded frozen resource: definition_schemas.operation",
    "projection schema": "embedded frozen resource: definition_schemas.projection",
    "projection mapping-capabilities": "embedded frozen resource: mapping_capabilities",
    "watcher schema": "embedded frozen resource: extra_schemas.watcher",
    "fallback schema": "embedded frozen resource: extra_schemas.fallback",
    "component schema": "embedded frozen resource: extra_schemas.component",
    "spec schema": "embedded frozen resource: spec[format_version].schema",
    "spec docs": "embedded frozen resource: spec[format_version].markdown",
    "spec check": "catabolic_store::interchange::dispatch(schema) + embedded frozen artifact checks",
    "spec validate": "catabolic_store::interchange::decode",
    "spec roundtrip": "catabolic_store::interchange::decode",
    "spec diff": "catabolic_cli::legacy::dispatch::diff + embedded frozen schema",
    "query": "catabolic_store::sql::execute",
    "query contract": "embedded frozen resource: definition_schemas.query",
    "query save": "catabolic_runtime::api::execute(definitions, query-put)",
    "query show": "catabolic_runtime::api::execute(definitions, query-get)",
    "query list": "catabolic_runtime::api::execute(definitions, query-list)",
    "query run": "catabolic_store::selection::Evaluation::run",
    "status": "catabolic_store::embedding::application(status)",
    "profile add": "catabolic_store::embedding::application(add_profile)",
    "profile list": "catabolic_store::migration::rows (profile IDs)",
    "location list": "catabolic_store::public_reads::bindings(source)",
    "catalog list": "catabolic_store::public_reads::bindings(output)",
    "scan": "catabolic_runtime::public_scan::execute -> observation::execute",
    "copies plan": "catabolic_store::public_controls::copies_plan -> layout::copy_filter",
    "rendition exclude": "catabolic_store::public_controls::rendition_decide(excluded=true)",
    "rendition allow": "catabolic_store::public_controls::rendition_decide(excluded=false)",
    "rendition policy": "catabolic_store::definitions::dispatch(rendition-get/rendition-put)",
    "rendition list": "catabolic_store::public_reads::output(definitions=false)",
    "rendition definitions": "catabolic_store::public_reads::output(definitions=true)",
    "artifact list": "catabolic_runtime::public_artifacts::call -> catabolic_store::public_reads::artifact(list)",
    "artifact recipes": "catabolic_runtime::public_artifacts::call -> catabolic_store::public_reads::artifact(recipes)",
    "operation list": "catabolic_store::public_reads::artifact(recipes)",
    "process list": "catabolic_store::public_reads::processing(list)",
    "process attempts": "catabolic_store::public_reads::processing(attempts)",
    "process facts": "catabolic_store::public_reads::processing(facts)",
    "content duplicates": "catabolic_store::public_reads::processing(duplicates)",
    "content search": "catabolic_store::public_reads::processing(search)",
    "fallback enable": "catabolic_store::public_controls::fallback(enable)",
    "fallback disable": "catabolic_store::public_controls::fallback(disable)",
    "fallback history": "catabolic_store::public_controls::fallback(history)",
    "projection http-operation-list": "catabolic_store::public_controls::http_operation(list)",
    "projection http-operation-show": "catabolic_store::public_controls::http_operation(get)",
    "projection http-operation-put": "catabolic_runtime::api::execute(http-operations, put)",
    "projection http-operation-disable": "catabolic_runtime::api::execute(http-operations, disable)",
    "projection http-operation-retry": "catabolic_runtime::api::execute(http-operations, retry)",
}


def main():
    nodes = json.loads((ROOT / "tests/parity/inventory.json").read_text())["cli"]
    leaves = [
        n
        for n in nodes
        if not any(a["action"] == "_SubParsersAction" for a in n["arguments"])
    ]
    rows = []
    for node in leaves:
        path = node["id"].removeprefix("cli:")
        command, *rest = path.split(" ")
        op = rest[0] if rest else ""
        adapted = (
            command in SINGLES if not op else op in ADAPTED.get(command, "").split()
        )
        row = {
            "id": node["id"],
            "command": path,
            "status": "adapted_unqualified" if adapted else "missing_adapter",
            "native_owner": OWNERS.get(command),
            "parser": "native_frozen_tree",
            "qualification": "No full-leaf behavioral parity claim; selected scenario evidence only",
        }
        if path == "api serve":
            row.update(
                status="safety_gated",
                gap="Legacy api serve refuses; companion requires --experimental-native-http and loopback",
            )
        elif path in PARTIAL:
            row.update(status="partial", gap=PARTIAL[path])
        elif not adapted:
            row["gap"] = GAPS.get(
                command,
                "Native family owner exists; this preserved command adapter is not implemented",
            )
        rows.append(row)
    for op in ["save", "show", "list", "run", "contract"]:
        rows.append(
            {
                "id": "cli:query " + op,
                "command": "query " + op,
                "status": "adapted_unqualified",
                "native_owner": OWNERS["query"],
                "parser": "dynamic_saved_query_dispatch",
                "qualification": "Selected dynamic-query scenarios; no full-leaf claim",
            }
        )
    assert set(ENTRYPOINTS) <= set(row["command"] for row in rows)
    for row in rows:
        row["owner_mapping"] = (
            "entrypoint" if row["command"] in ENTRYPOINTS else "family"
        )
        if row["command"] in ENTRYPOINTS:
            row["native_owner"] = ENTRYPOINTS[row["command"]]
    assert len(rows) == 242 and len({r["id"] for r in rows}) == 242
    out = {
        "reference": "453fca983222c6665a48775eef67c489c96b527e",
        "parser_nodes": 271,
        "leaf_nodes": 237,
        "dynamic_variants": 5,
        "python_remains_default": True,
        "full_surface_parity": False,
        "rows": rows,
    }
    (ROOT / "tests/parity/legacy-cli-owners.json").write_text(
        json.dumps(out, indent=2) + "\n"
    )
    from collections import Counter

    print(dict(Counter(r["status"] for r in rows)))


if __name__ == "__main__":
    main()
