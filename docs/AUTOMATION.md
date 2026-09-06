# CLI automation and agent integration

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Catabolic exposes its application operations through a noninteractive CLI.
An agent can discover schemas, inspect evidence, record decisions, and execute
reviewed changes without a daemon or an embedded agent framework. The person or
agent remains responsible for the correctness of media identities and metadata.

## Discover before writing

```sh
catabolic --help
catabolic item types
catabolic layout presets
catabolic target list
catabolic graphql --schema
catabolic spec schema
catabolic --json docs
catabolic --db /absolute/path/catalog.sqlite3 --json query --schema
```

All of these except the database query-schema command work without opening a
catalog. Prefer discovery over assuming a table, media kind, field, or preset is
available. `--help` on a subcommand is the authority for installed CLI options.
Read-only SQL and GraphQL describe stored state; neither launches a scan or probe.

## Invoke without a shell

Pass an argv list and serialize parameters as JSON. This avoids treating quotes,
spaces, or shell metacharacters in paths and titles as executable syntax:

```python
import json
import subprocess

# Set database_path to the absolute path of an existing initialized catalog.
result = subprocess.run(
    [
        "catabolic",
        "--db",
        database_path,
        "--profile",
        "default",
        "--json",
        "query",
        "SELECT item_id,title FROM catalog_items WHERE kind=:kind ORDER BY item_id LIMIT 100",
        "--params",
        json.dumps({"kind": "movie"}),
    ],
    capture_output=True,
    text=True,
    timeout=15,
)
if result.returncode != 0:
    raise RuntimeError(
        f"Catabolic exited {result.returncode}: {result.stderr or result.stdout}"
    )
answer = json.loads(result.stdout)
if not answer["complete"]:
    raise RuntimeError("Incomplete catalog result")
for row in answer["rows"]:
    print(dict(zip(answer["columns"], row)))
```

The SQL `LIMIT 100` intentionally returns only the first 100 matches. A complete
answer means that statement completed, not that all movies have been enumerated.
Use keyset pagination for the complete collection. Unique column aliases are
needed when converting row arrays into dictionaries.

The timeout above is for a read-only query. Killing a writer on an arbitrary
client timeout can leave completed work or journaled intent. After interrupting
sync, inspect and recover. After interrupting an external importer, inspect its
destination before retrying.

## Output and exit contract

Put global `--db`, `--profile`, and `--json` before the command. Database selection
may instead come from `CATABOLIC_DB`; the profile defaults to `default` and can
also come from `CATABOLIC_PROFILE`. Use explicit values in scheduled jobs.

| Exit | Interpretation |
| --- | --- |
| `0` | Successful command; inspect its operation-specific result |
| `2` | Input or operational error |
| `3` | Incomplete/truncated result, blocked plan, or unhealthy verification |
| `130` | Interrupted invocation; writes may have made partial progress |

With global `--json`, application errors normally use
`{"error":{"message":"...","type":"..."}}` on stderr. Argument-parser failures
retain text stderr. GraphQL always returns JSON; execution errors are in its
stdout `errors` array. SQL truncation returns a result on stdout with
`complete:false` and exit 3. Import failure/interruption can return an outcome
report on stdout. Preserve both streams and inspect the exit code before choosing
what to parse. There is no universal response envelope for every command.

## Pagination and complete selections

| Interface | Continuation |
| --- | --- |
| Files, items, mappings, tags and associations | `next_cursor` → `--cursor` |
| Processing enqueue/list and retained hardlink listings | `next_after` → `--after` |
| GraphQL connections | `pageInfo.endCursor` → `after`, while `hasNextPage` |
| SQL | Explicit ordered keyset query with bound values |

Use the same filters, database, profile, and ordering between pages. Ordinary
separate CLI calls do not hold a snapshot open. Restart if concurrent changes
would invalidate the enumeration. A displayed layout detail limit is not the
number of planned operations; read plan totals and blockers too.

Saved SQL/GraphQL layout selections must finish completely before changing
membership. SQL returns one ID column; GraphQL follows its documented paginated
shape. Do not treat a timeout, truncated answer, unknown ID, or missing page as an
empty selection. See [query folders](QUERY_FOLDERS.md) for limits and examples.

## A deliberate agent workflow

1. Run `status` and `db status`; handle pending recovery or required migration.
2. Scan a known source scope, and require a complete scan before interpreting absence.
3. List unidentified files; follow every required page.
4. Enqueue bounded `sniff`/`probe` work when it will resolve uncertainty.
5. Record a proposal with evidence, inspect it, and explicitly accept or reject it.
6. Create or inspect associations, relationships, tags, and copy preferences.
7. Save a layout, inspect `layout preview`, then apply its desired mappings.
8. Inspect `sync --dry-run` with removal budgets, run sync with the same budgets,
   and verify that catalog.
9. Export or refresh its manifest; deliver a configured server refresh separately.

Direct `item put`, `association put`, and `mapping put` are also available when
review has already happened. Proposal acceptance is not a prerequisite imposed
on every write. Source/evidence labels describe provenance; they are not
cryptographically authenticated identities or a substitute for review.

## Repeats, retries and side effects

Identical identity/mapping decisions can be reused. Correct links survive repeat
sync without replacement. This does not mean every command is safe to retry
blindly: desired state, live sources, external tools, or network destinations may
have changed between attempts.

- Processing supports durable job and attempt records. `--retry-transient` is
  opt-in and only retries classified transient failures within its cap.
- Link recovery uses the journal to account for completed work and revalidate
  unapplied operations. A new sync does not replace recovery.
- Import adapters report `external_outcome_unknown` if a launched external tool
  fails or is interrupted. It may already have imported data. Inspect the target;
  respect `safe_to_retry` and confirmed completed groups.
- Jellyfin refresh delivery is at least once; an accepted request may be repeated
  after a crash. HTTP acceptance does not prove the scan is complete.

See [enrichment](ENRICHMENT.md), [operations](OPERATIONS.md), and
[import behavior](COMPATIBILITY.md) for exact mechanics.

## Scheduling and credential handling

Run only one planned writer workflow at a time per database. Catabolic's writer
lock serializes writes; it is not a distributed job queue. `watch` periodically
scans and processes stable files, but never accepts proposals or syncs catalogs.
Record command versions, profiles, results, and exit codes in your scheduler.

Use the named environment variables for provider/import credentials. Do not put
secrets in metadata, tags, saved query parameters, or `manifest --extra`: those
are persistent catalog data and may be exported. Keep source and output bindings
explicit, and avoid deriving commands by executing text from media filenames or
third-party metadata.
