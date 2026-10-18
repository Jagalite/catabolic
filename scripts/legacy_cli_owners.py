#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Regenerate explicit leaf ownership coverage; parser coverage is separate."""

import argparse
import json
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ADAPTED = {
    "processor": "put show jobs enqueue claim tick job retry cancel heartbeat dispatch complete",
    "catalog-refresh": "enable disable pending run watch",
    "api": "worker principal-put grant-put token-create token-rotate token-revoke tokens principal-disable grant-disable operation-disable source-expose operation-approve",
    "artifact": "capabilities bind recipe enqueue list recipes run recover show",
    "rendition": "exclude allow policy import-receipt receipt-source define register list definitions show definition",
    "proposal": "put list show accept reject history",
    "expected": "put report",
    "copies": "put show plan",
    "process": "enqueue run list facts show cancel retry attempts",
    "content": "duplicates search",
    "refresh": "configure list run retry",
    "fallback": "schema put show list bind preview execute resolve enable disable history worker",
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
    "projection": "schema put show verify recover enable-auto disable-auto mapping-capabilities http-operation-list http-operation-put http-operation-show http-operation-disable http-operation-retry mapping-preview mapping-apply mapping-events mapping-recover preview execute",
    "watcher": "put show enable disable run history preview list pending schema",
    "consumer": "plex-login plex-login-complete import metadata metadata-match metadata-matches connection-put discover bind create bindings connections attempts events creations publications disable enable retry verify-indexing run watch",
    "notify": "put status run disable retry",
    "inbox": "list show summary",
}
SINGLES = {
    "watch",
    "identify",
    "maintenance",
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
    "watch": "catabolic_cli::legacy::watch; catabolic_runtime::{public_scan,public_processing}; catabolic_store::maintenance",
    "maintenance": "catabolic_runtime::maintenance; catabolic_store::maintenance",
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
    "projection preview": "Rollback-only writer orchestration forwards sealed layout prepare/apply and public reconciliation preview; fallback bindings refuse before planning. Selected ordinary report, rollback and admission evidence only; SIGINT may be frozen SQL CatabolicError/2 versus native KeyboardInterrupt/130; full owner/platform parity remains unqualified",
    "projection execute": "Commits a safe staging, then publishes through the shared journal executor (Reconciler notify_consumers=False) and verifies, as frozen Projections.run; symlink replace/remove, hardlink retirement and fallback bindings keep their gates (track C5)",
    "fallback worker": "Preserved singleton/once/perpetual transport forwards the existing native tick; due projection apply and recovery retain M6 refusals. Selected lifecycle/output evidence does not qualify publication, all SQLite error categories or platform parity",
    "watch": "Polling adapter forwards native scan/stable-revision/processing owners, releasing the writer between ticks. Durable scan report coverage/reuse/retry/header/progress fields are present; ordinary batch/committed-byte accounting has paired evidence; timing-derived report bytes and stricter freshness differences remain explicit. Automatic output refresh refuses at the public Reconciler gate before planning",
    "maintenance": "Ordinary inventory/analysis orchestration exists; output planning/publication and rule rendering refuse at M6. Existing scan-owner report fields differ from the frozen oracle; full JSON/machine report parity is not qualified",
    "projection mapping-events": "Combined local-owner publication/HTTP history, complete-event selection, bounded tail and same-snapshot nested subject-order presentation are implemented; malformed-subject diagnostics, installed artifacts and broader admission parity remain unqualified",
    "projection mapping-recover": "Combined history resolves native v1 readback-only recovery, HTTP-v3 queue status and v2 folder recovery through the shared executor (track C4); v2 export/import recovery remains gated. Installed artifacts, arbitrary payload ordering, live consumers and full-leaf parity remain unqualified",
    "projection mapping-preview": "Native v1 metadata/collection preview, rollback-only v2 folder/export/import previews and v3 HTTP owner are forwarded. Arbitrary nested insertion order, malformed-input diagnostics, installed artifacts and full-leaf parity remain unqualified",
    "projection mapping-apply": "Native v1 reviewed metadata/collection apply and v3 durable HTTP queue share existing metadata serialization; v2 folder apply publishes through the shared executor (track C4) and v2 export/import apply remains gated. Installed artifacts, live consumers, platforms and full-leaf parity remain unqualified",
    "projection mapping-capabilities": "Exact values are embedded natively; frozen set-derived adapter order varies with Python hash seed, so arbitrary-seed stdout byte order remains an explicit gap",
    "operation put": "Native immutable external metadata admission is adapted alongside analysis/render. Full malformed-input, transport, SQLite error-category, installed and platform parity remains unqualified",
    "rule run": "Bounded --worker selection/claim/dispatch/readback orchestration is adapted. Existing processor transport encoding/redirect/oversize/timeout diagnostics and Store reattachment contention semantics retain explicit nonparity; full receipt, installed, platform and M6 gates remain open",
    "scan": "Shared existing native Application.scan owner, with known continuation/barrier/signal qualification gaps retained",
    "catalog bind": "Implicit creation of an omitted output root remains refused; hardlink safety restrictions remain",
    "sync": "Existing native symlink/hardlink/fallback mutation qualification gates remain enforced",
    "recover": "Existing native symlink/hardlink/fallback recovery qualification gates remain enforced",
    "fallback execute": "Native fallback publication remains restricted to explicitly disposable debug fixtures; legacy flags cannot bypass it",
    "artifact run": "Native execution owner exists; exhaustive legacy cancellation/output behavior is not qualified",
    "spec roundtrip": "Command-local extension-order adapter has focused exact-byte evidence and accepted independent narrow review; full-leaf qualification remains pending",
}
GAPS = {
    "supervise": "Preserved supervise lifecycle facade is not implemented",
    "inbox": "Underlying native query owners exist; the preserved inbox evidence adapter is missing",
    "projection": "Underlying native planning/mapping owners exist; this preserved operation adapter is missing",
    "fallback": "Underlying native fallback owners exist; this preserved operation adapter is missing",
}
# Exact reviewed transport entrypoints override family-level implementation
# associations. Neither form constitutes a behavioral or safety qualification.
QUALIFICATION = {
    "operation put": "Selected immutable revision/admission/size-bound frozen pairs and native metadata-only checks; see docs/RUST_MIGRATION_EXTERNAL_RULE.md",
    "rule run": "Selected numeric-loopback frozen/native one-step, batch, membership, durable state, lock and cancellation fixtures only; no full-leaf or output-publication qualification; see docs/RUST_MIGRATION_EXTERNAL_RULE.md",
    "projection preview": "Source-bound fixed frozen/native ordinary preview, all-table rollback, lock/admission/SIGINT and explicit refusal fixtures; local partial adaptation only; see docs/RUST_MIGRATION_PROJECTION_RUN.md",
    "projection execute": "Sequential paired frozen/native execute, repeat and refusal fixtures over identical catalogs (tests/test_native_projection_execute.py)",
    "projection mapping-events": "Selected pinned Python/Rust local authority, profile/filter/order, cap, read-only and exact arbitrary nested subject-order fixtures; leaf remains partial",
    "projection mapping-recover": "Selected pinned Python/Rust v1 durable readback states and HTTP-v3 queue/error/status, lock and interruption fixtures; sequential paired v2 folder recovery (tests/test_native_publication_mapping_folder.py); full leaf remains unqualified",
    "projection mapping-preview": "Selected pinned Python/Rust v1 metadata/collection plan identity and no-write fixtures, v2 complete folder/export/import plan identity, rollback and bounded account-identity fixtures, and HTTP-v3 CLI controls; leaf remains partial",
    "projection mapping-apply": "Selected pinned Python/Rust v1 isolated publication/recovery and v3 durable queue, plus lock/interruption fixtures; sequential paired v2 folder apply (tests/test_native_publication_mapping_folder.py); leaf remains partial",
    "fallback worker": "Exact frozen empty/disabled/future/watcher-skip, contention, SIGINT channels and restart fixtures; due/publication refusal remains explicit; selected standalone SIGTERM exits by signal after native cleanup. See docs/RUST_MIGRATION_FALLBACK_WORKER.md and docs/RUST_MIGRATION_LEGACY_SIGTERM.md",
    "watch": "Selected frozen-oracle loop/admission/backlog/state and shell-report checks; two known scan-report nonparity witnesses, separately failed full-result diagnostics, SIGINT idle/processing wait and immediate restart. No full leaf, underlying owner, installed, platform or M6 qualification. See docs/RUST_MIGRATION_SCAN_WATCH.md",
    "identify": "Selected frozen-process loopback intake/proposal/cancellation evidence; malformed network JSON diagnostics, live provider and full platform parity remain unqualified",
    "consumer watch": "Same-executable cancellable ticks forward production consumer/notification owners; selected frozen line/effect/restart checks, not full consumer/platform qualification",
    "maintenance": "Selected frozen-oracle inventory/text/analysis/state, report-shell and admission checks; two explicit known scan JSON/machine nonparity witnesses and separately failed full-report diagnostics, narrow cancellation/metadata rollback checks; no M6 or full-leaf qualification. See docs/RUST_MIGRATION_MAINTENANCE.md",
    "catalog-refresh watch": "Bounded per-tick owner, idle writer release and interruption fixtures; publication/platform parity remains unqualified",
}
ENTRYPOINTS = {
    "projection preview": "catabolic_runtime::projection::run -> catabolic_store::projection_run::preview -> layout_mutation::{prepare,apply} + public_reconcile::call(preview), rollback-only",
    "projection execute": "catabolic_runtime::projection::run -> catabolic_store::projection_run::execute -> layout_mutation::{prepare,apply} + public_reconcile::{publish(apply),call(verify)}",
    "projection mapping-events": "catabolic_cli::legacy::mapping_events::execute -> catabolic_store::mapping_history::events_with_subject",
    "projection mapping-recover": "catabolic_cli::legacy::projection::history -> catabolic_runtime::destination_mappings::recover",
    "projection mapping-preview": "catabolic_cli::legacy::projection::mapping -> catabolic_runtime::destination_mappings::run(v1) | catabolic_store::{publication_mappings::run_with_account(v2 preview),http_mappings::execute(v3)}",
    "projection mapping-apply": "catabolic_cli::legacy::projection::mapping -> catabolic_runtime::destination_mappings::run(v1) | catabolic_store::{publication_mappings::run_with_account(v2),writer_lock,http_mappings::execute(v3)}",
    "fallback worker": "catabolic_cli::legacy::fallback_worker::run -> catabolic_runtime::public_fallback::run -> public_fallback::tick",
    "watch": "catabolic_cli::legacy::watch::execute -> catabolic_runtime::{public_scan,public_processing}::execute; catabolic_store::maintenance::stable_page_with_limit",
    "identify": "catabolic_cli::legacy::identify::execute -> catabolic_runtime::identify::execute -> catabolic_store::curation::proposal_put (apply only)",
    "consumer watch": "catabolic_cli::legacy::consumer_watch::execute -> catabolic_runtime::api::execute(consumers, run; notify, run)",
    "maintenance": "catabolic_cli::legacy::maintenance::execute -> catabolic_runtime::maintenance::run",
    "api worker": "catabolic_cli::legacy::api_worker::run -> catabolic_runtime::http_worker::run",
    "catalog-refresh watch": "catabolic_cli::legacy::catalog_refresh::execute -> catabolic_runtime::catalog_refresh::execute",
    "refresh configure": "catabolic_store::legacy_refresh::configure",
    "refresh list": "catabolic_store::legacy_refresh::list",
    "refresh run": "catabolic_runtime::legacy_refresh::run",
    "refresh retry": "catabolic_store::legacy_refresh::retry",
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
    "operation put": "catabolic_runtime::operations::put -> existing artifact/media/processor/output owners",
    "rule run": "catabolic_runtime::rules::{dispatch,run_external} -> processor_mutation::{claim,job}, processors::{dispatch_with_transport,SupervisedTransport}",
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


# Non-issuing local administration routes retain their selected evidence.
for operation in (
    "principal-put",
    "grant-put",
    "token-revoke",
    "tokens",
    "principal-disable",
    "grant-disable",
    "operation-disable",
    "source-expose",
    "operation-approve",
):
    path = "api " + operation
    ENTRYPOINTS[path] = (
        "catabolic_cli::legacy::admin_cli::execute_detailed -> "
        "catabolic_store::admin_cli::execute_detailed(" + operation + ")"
    )
    QUALIFICATION[path] = (
        "Selected independent-store and typed-error/rollback evidence; SQLite/JSON "
        "diagnostics, definition key order and exhaustive cancellation remain unqualified"
    )


# Exclusive credential output is implemented, with explicit path and diagnostic
# gaps. These ownership mappings do not promote development evidence to release.
for operation in ("token-create", "token-rotate"):
    path = "api " + operation
    ENTRYPOINTS[path] = (
        "catabolic_cli::legacy::admin_cli::execute_detailed -> "
        "catabolic_store::admin_cli::execute_detailed(" + operation + ")"
    )
    QUALIFICATION[path] = (
        "Selected disposable create/rotate, exclusive-output, final-revoke failure "
        "and lexical-path evidence; strict installed-artifact gate passed on both pinned "
        "Pythons for cohort five, with fresh consolidated-artifact replay still required; "
        "no full-leaf behavioral parity claim"
    )
    PARTIAL[path] = (
        "Native access_admin issuance and private_secret exclusive-file owner are "
        "implemented; named ~user and HOME-unset passwd expansion remain unsupported, "
        "native SQLite/I/O exception envelopes differ, and exhaustive failure/cancellation "
        "schedules and platform parity remain unqualified"
    )


def build_ledger(inventory):
    nodes = inventory["cli"]
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
            "qualification": QUALIFICATION.get(
                path,
                "No full-leaf behavioral parity claim; selected scenario evidence only",
            ),
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
                path,
                GAPS.get(
                    command,
                    "Native family owner exists; this preserved command adapter is not implemented",
                ),
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
    return {
        "reference": "453fca983222c6665a48775eef67c489c96b527e",
        "parser_nodes": 271,
        "leaf_nodes": 237,
        "dynamic_variants": 5,
        "python_remains_default": True,
        "full_surface_parity": False,
        "rows": rows,
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check", action="store_true", help="refuse stale ledger without writing"
    )
    args = parser.parse_args(argv)
    inventory = json.loads((ROOT / "tests/parity/inventory.json").read_text())
    ledger = build_ledger(inventory)
    expected = json.dumps(ledger, indent=2) + "\n"
    path = ROOT / "tests/parity/legacy-cli-owners.json"
    if args.check:
        if not path.is_file() or path.read_text() != expected:
            parser.exit(
                1,
                "legacy CLI owner ledger is stale; run scripts/legacy_cli_owners.py\n",
            )
    else:
        path.write_text(expected)
    print(dict(Counter(row["status"] for row in ledger["rows"])))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
