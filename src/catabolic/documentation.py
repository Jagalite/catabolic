# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Offline guides shipped with the CLI; no database or checkout is required."""

from importlib.resources import files

from .domain import CatabolicError

# README.md and docs/ are authoritative. scripts/sync_docs.py publishes copies
# into package data; tests reject drift between a guide and its bundled copy.
TOPICS = (
    (
        "trust",
        "TRUST_POLICIES.md",
        "Owner trust policies",
        "Source settings, policy history, truthful validation and retained correctness boundaries.",
    ),
    (
        "consumer-validation",
        "CONSUMER_VALIDATION.md",
        "Consumer validation evidence",
        "Protocol, recovery, installed-wheel and optional notification evidence with live-server limits.",
    ),
    (
        "consumers",
        "CONSUMERS.md",
        "Output consumers and notifications",
        "Plex and Jellyfin setup, durable scans, independent Apprise notifications and recovery.",
    ),
    (
        "consumer-design",
        "CONSUMER_DESIGN.md",
        "Consumer implementation decisions",
        "Publication ownership, schema 17 and adapter boundaries.",
    ),
    (
        "programmable",
        "PROGRAMMABLE_CATALOG.md",
        "Queries, rules and projections",
        "Reusable query contracts, operation rules, safe projections and migration compatibility.",
    ),
    (
        "programmable-review",
        "PROGRAMMABLE_CATALOG_REVIEW.md",
        "Programmable catalog implementation review",
        "Repository findings, retained owners and schema 15-16 redesign decisions.",
    ),
    (
        "programmable-validation",
        "PROGRAMMABLE_CATALOG_VALIDATION.md",
        "Programmable catalog validation",
        "Executed regression, installed consumer and scale evidence with explicit limits.",
    ),
    (
        "experience-acceptance",
        "EXPERIENCE_ACCEPTANCE.md",
        "Experience acceptance milestone",
        "Runnable messy-collection reference journey, independent evaluator and planned isolated agent judgment trials.",
    ),
    (
        "catalog-refresh",
        "CATALOG_REFRESH.md",
        "Automatic catalog link updates",
        "Durable rendition completion triggers, saved layout refreshes and restartable link-only retries.",
    ),
    (
        "processors",
        "PROCESSORS.md",
        "Network processors and distributed workers",
        "HTTP receipt adapters, worker leases, measured estimate calibration and large paged rules.",
    ),
    (
        "renditions",
        "RENDITION_WORKFLOWS.md",
        "Rendition publication and external receipts",
        "Catalog-specific transcode libraries, rendition purpose, receipt imports and retry-safe requirements.",
    ),
    (
        "processing-review",
        "PROCESSING_REVIEW.md",
        "Processing architecture review",
        "Source research and proposed integration of rendition publication, completion and external processors.",
    ),
    (
        "rules",
        "RULES.md",
        "Processing rules and space estimates",
        "Retroactive recipe rules, storage estimates, bounded backfills and maintenance integration.",
    ),
    (
        "maintenance",
        "MAINTENANCE.md",
        "On-demand maintenance",
        "One cycle of scanning, bounded analysis, safe link sync, manifests and backlog statistics.",
    ),
    (
        "workflow",
        "WORKFLOW.md",
        "Recommended workflow",
        "Scan, curate, track required work, publish links, verify, complete and monitor.",
    ),
    (
        "worklog",
        "WORKLOG.md",
        "Entry status and worklog",
        "Completion gates, required work, journal notes, history and agent revision checks.",
    ),
    (
        "artifacts",
        "ARTIFACTS.md",
        "Generated media and processing artifacts",
        "Custom renditions, external registration, output definitions, recipes and recovery.",
    ),
    (
        "installation",
        "INSTALLATION.md",
        "Installation and upgrades",
        "pip, pipx, revision pins, optional tools, configuration and application upgrades.",
    ),
    (
        "getting-started",
        "GETTING_STARTED.md",
        "First catalog walkthrough",
        "A disposable document fixture, identification, native links, tags and a query folder.",
    ),
    (
        "operations",
        "OPERATIONS.md",
        "Inventory and recovery",
        "Multiple sources, profiles, scan evidence, queries, safe synchronization and recovery.",
    ),
    (
        "automation",
        "AUTOMATION.md",
        "CLI automation and agents",
        "Schema discovery, JSON contracts, pagination, decision workflows and retry boundaries.",
    ),
    (
        "troubleshooting",
        "TROUBLESHOOTING.md",
        "Troubleshooting",
        "Bindings, incomplete scans, empty outputs, container paths, hardlinks and diagnostics.",
    ),
    (
        "faq",
        "FAQ.md",
        "Frequently asked questions",
        "Sources, paths, media kinds, naming, query folders, backups and compatibility.",
    ),
    (
        "development",
        "DEVELOPMENT.md",
        "Development and documentation",
        "Checkout setup, tests, module ownership, migrations and wiki publishing.",
    ),
    (
        "testing",
        "RELEASE_TESTING.md",
        "Release verification",
        "Regression suite, installed-wheel media workflows, real storage mounts and Jellyfin acceptance.",
    ),
    (
        "enrichment",
        "ENRICHMENT.md",
        "Media enrichment and curation",
        "Probes, hashing, jobs, proposals, sidecars, copy selection, completeness, text search and refresh.",
    ),
    (
        "hardlinks",
        "HARDLINKS.md",
        "Hardlink outputs",
        "Explicit link modes, cross-filesystem checks, final-reference protection and retained data.",
    ),
    (
        "tags",
        "TAGGING.md",
        "Tags and curation",
        "Namespaced tags, aliases, hierarchy, provenance, boolean queries and manifest v2.",
    ),
    (
        "compatibility",
        "COMPATIBILITY.md",
        "Application outputs",
        "Twenty targets, versioned presets, XSPF/OPDS/NFO exports and calibre/Immich imports.",
    ),
    (
        "quickstart",
        "README.md",
        "Getting started",
        "Installation, first catalog, commands and operating limits.",
    ),
    (
        "schema",
        "SCHEMAS.md",
        "Schemas and documentation",
        "Discover database, SQL, GraphQL, manifest and layout contracts.",
    ),
    (
        "query",
        "QUERYING.md",
        "SQL queries",
        "Query examples, views, parameters, pagination and agent JSON responses.",
    ),
    (
        "graphql",
        "GRAPHQL.md",
        "GraphQL queries",
        "Query examples, variables, relationships, pagination and limits.",
    ),
    (
        "query-folders",
        "QUERY_FOLDERS.md",
        "Query folders",
        "Generate and refresh symlink collections from SQL or GraphQL.",
    ),
    (
        "native",
        "NATIVE_LAYOUT.md",
        "Native Catabolic layout",
        "Versioned folders, original filenames, media kinds and manifest provenance.",
    ),
    (
        "layouts",
        "OUTPUT_LAYOUTS.md",
        "Output layouts",
        "Native, Plex and custom templates; preview, apply and ownership.",
    ),
    (
        "manifest",
        "MANIFESTS.md",
        "Metadata manifests",
        "Export contents, output files, snapshot meaning and refresh semantics.",
    ),
    (
        "spec",
        "OPEN_CATALOG.md",
        "Open Catalog contract",
        "JSON Schema, validation, versioning and compatibility rules.",
    ),
    (
        "media",
        "MEDIA_MODEL.md",
        "Media model",
        "Kinds, file roles, editions, associations and relationships.",
    ),
    (
        "migrations",
        "MIGRATIONS.md",
        "Database migrations",
        "Versioned SQL upgrades, backups and preservation checks.",
    ),
    (
        "design",
        "DESIGN.md",
        "Architecture",
        "Catalog model, filesystem safety and implementation boundaries.",
    ),
    (
        "scale",
        "SCALE_BENCHMARKS.md",
        "Scale benchmarks",
        "Synthetic performance fixtures, measurements and limits.",
    ),
)


def _guide(filename):
    return files("catabolic").joinpath("guides", filename).read_text(encoding="utf-8")


def documentation(topic=None, search=None):
    if topic is not None and search is not None:
        raise CatabolicError("choose a documentation topic or --search, not both")
    if search is not None and not search.strip():
        raise CatabolicError("documentation search must not be empty")
    base = {"documentation_version": 1}
    terms = search.casefold().split() if search is not None else []
    matches = []
    for identifier, filename, title, summary in TOPICS:
        entry = {
            "topic": identifier,
            "title": title,
            "summary": summary,
            "source": filename,
        }
        if topic == identifier:
            return {**base, **entry, "markdown": _guide(filename)}
        if topic is not None:
            continue
        if search is not None:
            markdown = _guide(filename)
            haystack = " ".join((identifier, title, summary, markdown)).casefold()
            if not all(term in haystack for term in terms):
                continue
            entry["matches"] = [
                {"line": number, "text": line[:240]}
                for number, line in enumerate(markdown.splitlines(), 1)
                if any(term in line.casefold() for term in terms)
            ][:3]
        matches.append(entry)
    if topic is not None:
        raise CatabolicError(
            f"unknown documentation topic: {topic}; run catabolic docs to list topics"
        )
    return {
        **base,
        **({"query": search} if search is not None else {}),
        "topics": matches,
    }


def render_documentation(result):
    if "markdown" in result:
        return result["markdown"].rstrip()
    lines = [
        "Catabolic documentation"
        if "query" not in result
        else f"Documentation matches for: {result['query']}",
        "",
    ]
    for entry in result["topics"]:
        lines.append(f"{entry['topic']}: {entry['title']} — {entry['summary']}")
        for match in entry.get("matches", []):
            lines.append(f"  {entry['source']}:{match['line']}: {match['text']}")
    if not result["topics"]:
        lines.append("No matching documentation topics.")
    lines.extend(
        [
            "",
            "Read: catabolic docs TOPIC",
            "Search: catabolic docs --search TEXT",
            "Agent output: catabolic --json docs [TOPIC]",
        ]
    )
    return "\n".join(lines)
