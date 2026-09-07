# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Export repository guides to a GitHub wiki checkout; never commits or pushes."""

import argparse
import posixpath
import re
from pathlib import Path
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = "https://github.com/Jagalite/catabolic"
NOTICE = """<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->
"""
GROUPS = {
    "Start here": (
        ("README.md", "Overview", "What Catabolic does and how to try it"),
        ("INSTALLATION.md", "Installation", "pip, pipx, optional tools and upgrades"),
        (
            "GETTING_STARTED.md",
            "Getting-Started",
            "A runnable disposable first catalog",
        ),
        (
            "WORKFLOW.md",
            "Recommended-Workflow",
            "The everyday cataloging and maintenance cycle",
        ),
        ("FAQ.md", "FAQ", "Common questions and scope"),
    ),
    "Catalog and curate": (
        (
            "WORKLOG.md",
            "Entry-Worklog",
            "Entry status, completion checks and append-only worklogs",
        ),
        ("OPERATIONS.md", "Operations", "Sources, profiles, scans, sync and recovery"),
        (
            "MEDIA_MODEL.md",
            "Media-Model",
            "Items, identities, file roles and relationships",
        ),
        ("TAGGING.md", "Tagging", "Vocabulary, aliases, hierarchy and provenance"),
        ("ENRICHMENT.md", "Enrichment", "Probes, hashes, jobs, proposals and refresh"),
    ),
    "Query and automate": (
        ("QUERYING.md", "SQL-Queries", "Views, joins, grouping, parameters and limits"),
        ("GRAPHQL.md", "GraphQL", "Nested queries, variables and pagination"),
        ("QUERY_FOLDERS.md", "Query-Folders", "Saved selections as generated folders"),
        (
            "AUTOMATION.md",
            "Automation",
            "Agent workflows, JSON, exit codes and retries",
        ),
        (
            "SCHEMAS.md",
            "Schema-Discovery",
            "Discover installed contracts and offline docs",
        ),
    ),
    "Generate and share": (
        ("ARTIFACTS.md", "Generated-Media", "Recipes, saved outputs and recovery"),
        ("OUTPUT_LAYOUTS.md", "Output-Layouts", "Custom templates and naming rules"),
        ("NATIVE_LAYOUT.md", "Native-Layout", "Catabolic's versioned all-media naming"),
        (
            "COMPATIBILITY.md",
            "Application-Compatibility",
            "20 targets, exports and imports",
        ),
        ("HARDLINKS.md", "Hardlinks", "Filesystem limits and retained data"),
        ("MANIFESTS.md", "Manifests", "JSON metadata snapshots and safe publication"),
        (
            "OPEN_CATALOG.md",
            "Open-Catalog",
            "Typed interchange and version compatibility",
        ),
    ),
    "Operate and contribute": (
        ("MAINTENANCE.md", "Maintenance", "On-demand upkeep and backlog statistics"),
        (
            "MIGRATIONS.md",
            "Database-Migrations",
            "Backups, rehearsal and schema upgrades",
        ),
        (
            "TROUBLESHOOTING.md",
            "Troubleshooting",
            "Diagnose blockers and consumer paths",
        ),
        (
            "SCALE_BENCHMARKS.md",
            "Performance",
            "Measured workloads and scaling boundaries",
        ),
        ("DESIGN.md", "Architecture", "Application and database ownership boundaries"),
        ("DEVELOPMENT.md", "Development", "Setup, tests and documentation publishing"),
        (
            "RELEASE_TESTING.md",
            "Release-Testing",
            "Package, storage and consumer acceptance",
        ),
        ("ROADMAP.md", "Roadmap", "Recorded plans and implementation milestones"),
    ),
}
PAGES = {
    source if source == "README.md" else f"docs/{source}": slug
    for group in GROUPS.values()
    for source, slug, _ in group
}
LINK = re.compile(r"\[([^\]]+)\]\(([^\s)]+)\)")


def wiki_links(markdown, source="README.md"):
    """Rewrite inline relative links outside fenced examples; keep anchors."""

    def replace(match):
        label, target = match.groups()
        parsed = urlsplit(target)
        if parsed.scheme or target.startswith(("#", "//")):
            return match.group(0)
        path = posixpath.normpath(
            posixpath.join(posixpath.dirname(source), parsed.path)
        )
        destination = PAGES.get(path, f"{REPOSITORY}/blob/main/{path}")
        if parsed.fragment:
            destination += "#" + parsed.fragment
        if parsed.query:
            return match.group(0)
        return f"[{label}]({destination})"

    output = []
    fence = None
    for line in markdown.splitlines(keepends=True):
        stripped = line.lstrip()
        if fence:
            output.append(line)
            if stripped.startswith(fence):
                fence = None
        elif stripped.startswith(("```", "~~~")):
            fence = stripped[:3]
            output.append(line)
        else:
            output.append(LINK.sub(replace, line))
    return "".join(output)


def render_pages():
    pages = {}
    for source, slug in PAGES.items():
        body = wiki_links((ROOT / source).read_text(encoding="utf-8"), source)
        pages[f"{slug}.md"] = (
            body.rstrip()
            + f"\n\n---\n\n[Edit this guide in the repository]({REPOSITORY}/blob/main/{source}). "
            "This wiki page is generated from that source; "
            "see [Development](Development) for publishing.\n"
        )
    home = [
        "# Catabolic documentation\n\n" + NOTICE,
        "Catabolic catalogs media across source locations in SQLite and builds "
        "organized symlink or hardlink folders. People and agents supply identities "
        "and curation; the CLI supplies inventory, queries, naming and recoverable operations.",
        "Start with [Installation](Installation), then [Getting Started](Getting-Started). "
        "The walkthrough uses a disposable document, so no existing media library is needed.",
        "These pages track the repository's main branch. For the version you installed, "
        "run `catabolic docs` or read a topic with `catabolic docs TOPIC`.",
        "```text\nBind sources → scan → identify and tag → select and name\n"
        "             → preview → apply mappings → sync → verify → export\n```",
    ]
    sidebar = [NOTICE, "[Documentation home](Home)"]
    for group, entries in GROUPS.items():
        home.append(f"## {group}")
        sidebar.append(f"**{group}**")
        sidebar.append(
            "\n".join(f"- [{slug.replace('-', ' ')}]({slug})" for _, slug, _ in entries)
        )
        home.append(
            "| Guide | Contents |\n| --- | --- |\n"
            + "\n".join(
                f"| [{slug.replace('-', ' ')}]({slug}) | {summary} |"
                for _, slug, summary in entries
            )
        )
    home += [
        "## Important distinctions",
        "- Inventory is recorded evidence; queries do not inspect drives.\n"
        "- Identification is independent of output placement.\n"
        "- Layout application changes desired mappings; sync changes links.\n"
        "- A manifest is a catalog snapshot, not a full database backup.\n"
        "- Naming support and successful application scans are separate evidence.",
        f"[Source and issues]({REPOSITORY}) · "
        f"[Current CI]({REPOSITORY}/actions) · [MIT license]({REPOSITORY}/blob/main/LICENSE)",
    ]
    pages["Home.md"] = "\n\n".join(home) + "\n"
    pages["_Sidebar.md"] = "\n\n".join(sidebar) + "\n"
    pages["_Footer.md"] = (
        NOTICE + f"\n[Catabolic]({REPOSITORY}) · [Documentation home](Home) · "
        f"[Report an issue]({REPOSITORY}/issues) · [MIT]({REPOSITORY}/blob/main/LICENSE)\n\n"
        "Offline guides: `catabolic docs`. Wiki sources live in the application repository.\n"
    )
    return pages


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output", type=Path, default=ROOT / ".local-tests/wiki-export"
    )
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    destination = args.output.resolve()
    if destination == ROOT:
        parser.error("choose a separate wiki export directory or wiki checkout")
    pages = render_pages()
    changed = []
    for name, content in pages.items():
        path = destination / name
        if not path.exists() or path.read_text(encoding="utf-8") != content:
            changed.append(name)
            if not args.check:
                destination.mkdir(parents=True, exist_ok=True)
                path.write_text(content, encoding="utf-8")
    if changed and args.check:
        print("Stale or missing wiki pages: " + ", ".join(changed))
        return 1
    print(
        f"{'Checked' if args.check else 'Exported'} {len(pages)} wiki pages: {destination}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
