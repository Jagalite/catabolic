# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Installed-CLI query/rule/projection proof inside the existing disposable fixture."""


def exercise(workflow, file_id):
    w = workflow

    def cli(*args, expected=0):
        envelope = w.cli("--machine", *args, expected=expected)
        assert envelope["interface_version"] == 1 and envelope["exit_code"] == expected
        return envelope["data"]

    def save(name, query, params):
        return cli(
            "query",
            "save",
            name,
            "--definition",
            w.json_file(
                name + ".json",
                {"selection": {"language": "sql", "query": query, "params": params}},
            ),
        )["id"]

    cli("process", "enqueue", "probe", "--file-id", file_id, "--refresh")
    cli("process", "run", "--limit", "1")
    operation = cli(
        "operation",
        "put",
        "programmable-proxy",
        "--definition",
        w.json_file(
            "programmable-operation.json", {"kind": "render", "operation": "h264-720p"}
        ),
    )
    gap = save(
        "programmable-gap",
        "SELECT file_id FROM catalog_files f WHERE profile=:profile AND file_id=:file AND NOT EXISTS(SELECT 1 FROM catalog_rendition_state r WHERE r.profile=f.profile AND r.source_file_id=f.file_id AND r.operation_id=:operation AND r.recorded_current=1)",
        {"file": file_id, "operation": operation["id"]},
    )
    assert cli("query", "run", gap)["ids"] == [file_id]
    rule = cli(
        "rule",
        "put",
        "programmable-rule",
        "--query",
        gap,
        "--operation",
        operation["id"],
        "--location",
        "generated",
        "--required",
    )
    assert cli("rule", "preview", rule["id"])["matched_inputs"] == 1
    assert len(cli("rule", "apply", rule["id"], "--batch", "1")["queued"]) == 1
    result = cli("rule", "run", rule["id"], "--batch", "1")
    assert result["complete"]
    output = result["execution"]["completed"][0]
    assert cli("query", "run", gap)["ids"] == []
    generated = save(
        "programmable-generated",
        "SELECT file_id FROM catalog_rendition_state WHERE profile=:profile AND operation_id=:operation AND recorded_current=1",
        {"operation": operation["id"]},
    )
    assert cli("query", "run", generated)["ids"] == [output["file_id"]]
    target = w.root / "programmable-projection"
    target.mkdir()
    cli("catalog", "bind", "programmable", "--root", str(target))
    cli("layout", "put", "programmable-flat", "--preset", "flat")
    policy = w.json_file("programmable-policy.json", {"purpose": "transcode"})
    cli(
        "projection",
        "put",
        "programmable",
        "--query",
        generated,
        "--layout",
        "programmable-flat",
        "--renditions",
        policy,
    )
    assert cli("projection", "preview", "programmable")["safe"]
    assert list(target.iterdir()) == []
    # A disappeared output destination must be retriable without another render.
    offline = target.with_name(target.name + "-offline")
    target.rename(offline)
    try:
        blocked = cli("projection", "execute", "programmable", expected=3)
        assert not blocked["safe"] and not blocked["applied"]
    finally:
        offline.rename(target)
    assert cli("projection", "execute", "programmable")["complete"]
    manifest = cli("manifest", "--catalog", "programmable")
    stats = manifest["content"]["extra"]["catabolic:statistics"]
    assert stats["version"] == 1
    assert stats["current_recorded"]["desired_entries"] == 1
    assert stats["current_recorded"]["selection"]["files"] == 1
    assert int(stats["current_recorded"]["selection"]["known_referenced_bytes"]) > 0
    assert stats["last_execution"]["applied_actions"]["create"] == 1
    assert stats["last_execution"]["verification"]["verified_links"] == 1
    assert not stats["filesystem_verified_at_export"]
    links = [p for p in target.rglob("*") if p.is_symlink()]
    assert len(links) == 1 and links[0].resolve().is_relative_to(w.root / "generated")
    inode = links[0].lstat().st_ino
    assert cli("projection", "execute", "programmable")["layout"]["change_count"] == 0
    assert links[0].lstat().st_ino == inode
    repeated_stats = cli("manifest", "--catalog", "programmable")["content"]["extra"][
        "catabolic:statistics"
    ]
    assert repeated_stats["last_execution"]["applied_actions"] == {}
    assert repeated_stats["last_execution"]["planned_actions"]["unchanged"] == 1
    assert cli("projection", "verify", "programmable")["healthy"]
    assert cli("rule", "apply", rule["id"])["queued"] == []
    assert cli("rule", "run", rule["id"])["execution"]["completed"] == []
    assert len(cli("rule", "stats", rule["id"])["attempts"]) == 1
    bundle = cli(
        "program", "export", "--rule", rule["id"], "--projection", "programmable"
    )
    bundle_path = w.json_file("program-bundle.json", bundle)
    imported_target = w.root / "imported-projection"
    imported_target.mkdir()
    cli("catalog", "bind", "imported-program", "--root", str(imported_target))
    bindings = w.json_file(
        "program-bindings.json",
        {
            "catalogs": {"programmable": "imported-program"},
            "locations": {"generated": "generated"},
        },
    )
    import_args = ("program", "import", "--file", bundle_path, "--bindings", bindings)
    preview = cli(*import_args, "--dry-run")
    assert preview["complete"] and not preview["applied"]
    assert cli("projection", "show", "imported-program")["legacy"]
    imported = cli(*import_args)
    assert imported["applied"]
    imported_rule = imported["references"]["rules"][rule["id"]]
    assert not cli("rule", "show", imported_rule)["enabled"]
    assert cli(*import_args)["references"] == imported["references"]
    assert list(imported_target.iterdir()) == []
    return {
        "query_id": gap,
        "operation_id": operation["id"],
        "rule_id": rule["id"],
        "output_id": output["output_id"],
        "projection": "programmable",
        "checks": [
            "missing-output convergence",
            "generated selection",
            "lineage-backed publication",
            "offline destination retry",
            "stable repeated run",
            "manifest statistics with full action counts and recorded source bytes",
            "machine envelope",
            "portable definitions with atomic preview and idempotent import",
        ],
    }
