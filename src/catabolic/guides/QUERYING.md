# SQL queries for people and agents

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Catabolic exposes SQLite through a noninteractive, read-only command. Agents use
structured JSON on stdout and errors on stderr. No server or embedded agent
runtime is required.

```sh
catabolic --db /path/to/catalog.sqlite3 --json query --schema
catabolic --db /path/to/catalog.sqlite3 --json query \
  'SELECT item_id,title,year FROM catalog_items ORDER BY title,item_id LIMIT 25'
```

`--schema` returns query-interface version 1, views and columns, underlying tables,
permitted functions available in the installed SQLite, and execution limits.
Prefer the documented views for reusable queries. Raw tables remain readable,
but their structure follows database migrations.

Joins, grouping, aggregates, window functions, CTEs, and ordinary SQLite
expressions are supported. Exactly one result-producing statement is accepted.
The views exist only on the query connection; they do not alter the stored schema
and will not appear in an external `sqlite3` session. The current query command requires
schema 14; older databases must be upgraded explicitly.

## Views and row meaning

Entry review state and journal queries use `catalog_item_workflow`,
`catalog_workflow_checks`, `catalog_item_requirements`, and `catalog_item_worklog`.
These distinguish requested status from effective readiness; see [Entry worklog](WORKLOG.md).

| View | One row represents | Columns |
| --- | --- | --- |
| `catalog_items` | One media item, including unmapped items | `item_id`, `kind`, `title`, `year`, `metadata` |
| `catalog_identities` | One provider identity | `item_id`, `namespace`, `value` |
| `catalog_files` | One file in one profile | `profile`, `file_id`, `location`, `source_relative_path`, `source_root`, `source_path`, `size`, `mtime_ns`, `status`, `scan_id`, `observed_at` |
| `catalog_entries` | One mapping in one profile, including disabled decisions | `profile`, `mapping_id`, `catalog`, `active`, `item_id`, `kind`, `title`, `year`, `file_id`, all source and observation columns from `catalog_files`, `catalog_path`, `output_root`, `output_path`, `recorded_link_target` |
| `catalog_item_files` | One independent identification in one profile, including disabled associations | `profile`, `association_id`, `item_id`, `kind`, `title`, `year`, `file_id`, `role`, `part`, `metadata`, `origin`, `active`, all source and observation columns from `catalog_files` |
| `catalog_relationships` | One directed relationship, shared across profiles | `relationship_id`, `source_id`, `source_kind`, `source_title`, `target_id`, `target_kind`, `target_title`, `kind`, `position`, `metadata`, `active` |
| `catalog_renditions` | One generated or externally registered file rendition in a profile | `id`, `profile`, `file_id`, `source_file_id`, `source_item_id`, `item_id`, `definition_id`, `artifact_id`, `origin`, `metadata`, `created_at` |
| `catalog_output_definitions` | One immutable rendition definition revision, shared across profiles | `id`, `name`, `revision`, `definition`, `digest`, `created_at` |
| `catalog_recipes` | One immutable generation recipe revision | `id`, `name`, `revision`, `preset`, `definition`, `digest`, `output_definition_id`, `created_at` |

Rendition `origin` distinguishes Catabolic-generated artifacts from user-declared
external provenance. Join `definition_id` to output definitions, or `artifact_id`
to `catalog_artifacts` for validated generation evidence. See [Generated media](ARTIFACTS.md).

File, entry, and identification views contain **every profile**. Items, identities,
and relationships are shared and have no profile column. The reserved parameter
`:profile` comes from
`--profile` or `CATABOLIC_PROFILE`, defaulting to `default`. It does not silently
filter SQL: use `WHERE profile=:profile`. Without that condition, file and entry
counts include every profile, including those without observations or bindings.

`status` is recorded `present`, `missing`, or `unknown` availability. Queries never
probe media roots. Unbound roots yield null absolute paths. `recorded_link_target`
is the last owned target at that destination, not proof of current link health or
correspondence to a disabled mapping. Use `scan` and `verify` for fresh evidence.

Entries, associations, and relationships include disabled records; filter
`active=1` for current decisions. `catalog_item_files` retains identification
independently of catalog placement. Use it to find identified copies that have no
active output mapping. Ordered media relationships use source=child and
target=parent; see [MEDIA_MODEL.md](MEDIA_MODEL.md).
Several destinations can reference a file, and items can have several identities.
Joins can multiply rows: use `DISTINCT file_id` or `COUNT(DISTINCT file_id)` when
counting file occurrences. This does not detect byte-identical content or copies
that have not been associated with an item.

`metadata` is JSON text. `title` is a string or null; `year` is an integer between
1 and 9999 or null. String years are not coerced. `casefold(text)` supports Unicode
substring searches. Other permitted functions depend on the installed SQLite;
discover available names with `--schema`. Arbitrary extension functions and
table-valued PRAGMA functions are not exposed.

## Bound values, files, and stdin

Use named parameters instead of interpolating values into SQL:

```sh
catabolic --db /path/to/catalog.sqlite3 --json query \
  'SELECT item_id,title,year FROM catalog_items
   WHERE kind=:kind AND year>=:year
     AND instr(casefold(title),casefold(:term))>0
   ORDER BY year,item_id LIMIT 100' \
  --params '{"kind":"movie","year":2000,"term":"Dune"}'

catabolic --db /path/to/catalog.sqlite3 --json query --file report.sql

catabolic --db /path/to/catalog.sqlite3 --json query --file - <<'SQL'
SELECT location, COUNT(*) AS missing_files
FROM catalog_files
WHERE profile=:profile AND status='missing'
GROUP BY location
ORDER BY location;
SQL
```

`--params` accepts a JSON object with strings, finite numbers, booleans, or null.
Names use letters, digits, and underscores, starting with a letter or underscore.
Integers must fit signed 64 bits. `profile` is reserved; select it using the global
option. Booleans bind as 0 or 1. Encode arrays/objects as JSON strings when needed
by SQL expressions. Extra named parameters are ignored by SQLite; missing bindings
are errors. Positional `?` bindings are not supported by this CLI.

SQL files use UTF-8 and contain one statement, not a batch or migration script.
The command reads stdin only when `--file -` explicitly requests it. Omitting SQL
is an error, not an interactive prompt. `query --format json` is an alternative to
global `--json`; global options precede the command, and global `--json` takes
precedence over `--format table`.

## JSON contract and exit codes

```json
{
  "interface_version": 1,
  "profile": "default",
  "columns": ["location", "missing_files"],
  "rows": [["media", 3]],
  "row_count": 1,
  "max_rows": 1000,
  "truncated": false,
  "truncation_reason": null,
  "complete": true
}
```

Rows are arrays aligned with `columns`, preserving duplicate SQL column names
without losing values. Empty results retain column names. SQL null becomes JSON
null. BLOBs use `{"$blob":"00ff"}` with hexadecimal bytes; nonfinite floats use
`{"$float":"Infinity"}` or `{"$float":"-Infinity"}`. JSON in SQL TEXT stays a
string. No banners or progress messages are mixed into JSON stdout.

| Exit code | Meaning | Output |
| --- | --- | --- |
| 0 | Complete result or schema description | JSON stdout |
| 2 | Invalid input/SQL, denied operation, timeout, or operational failure | Error stderr, no partial stdout |
| 3 | Output truncated | Result stdout, `complete:false`, and a reason |
| 130 | User interruption | Interruption message stderr |

Application errors use `{"error":{"message":"...","type":"..."}}`.
Argument-parser errors such as unknown options retain argparse's text stderr and
code 2; interruption messages are also text. Check the exit code before parsing
the appropriate stream. `row_count` counts returned rows, not all matches.
Do not treat code 3 as a complete answer.

An agent can invoke the executable without shell interpolation:

```python
import json
import subprocess

result = subprocess.run(
    [
        "catabolic",
        "--db",
        database_path,
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
if result.returncode not in (0, 3):
    raise RuntimeError(result.stderr)
page = json.loads(result.stdout)
if not page["complete"]:
    raise RuntimeError(f"Incomplete result: {page['truncation_reason']}")
for row in page["rows"]:
    print(dict(zip(page["columns"], row)))  # Use unique aliases for this conversion.
```

This example deliberately requests at most 100 matches through SQL. A complete
result for that statement does not mean the catalog has only 100 movies.

## Bounds and pagination

`--max-rows` defaults to 1000 and accepts 1–10000. It caps returned output, not SQL
input: aggregations still process all matching rows. One extra result row is
fetched to detect truncation. Encoded columns and rows have an 8 MiB payload
budget; formatting and the envelope add overhead. `truncation_reason` is
`max_rows` or `max_result_bytes` when a cap is reached.

`--timeout-ms` defaults to 5000 and accepts 1–60000. It is a cooperative SQL
execution deadline using SQLite progress callbacks and row-fetch checks, not a
hard operating-system process deadline. Opening the database and setting up views
precede it. Agents should also set a suitable subprocess timeout. SQL input and
individual SQLite values/rows are limited to 1 MiB; excessive expressions,
compound statements, columns, and bound variables are also rejected.

For full enumeration, write explicit keyset pagination:

```sql
SELECT item_id,title FROM catalog_items
WHERE item_id>:after ORDER BY item_id LIMIT 100;
```

Start with `--params '{"after":""}'`, then pass the last returned ID until an
empty page. Choose a SQL page size that fits output caps. If truncated, consume
only returned rows before advancing; reduce the page size when appropriate.
Separate calls have separate snapshots, so restart if concurrent changes would
make partial enumeration unacceptable. There is no opaque SQL cursor or automatic
SQL rewriting. Use a unique `ORDER BY` whenever order matters.

Human output defaults to a pipe-separated table. Control characters are escaped;
cells longer than 160 characters are visibly abbreviated. JSON retains full
returned values.

## Enforcement

The runner opens a dedicated connection in read-only mode, creates temporary
views in memory, and enables `query_only`. An authorizer permits SELECTs, reads,
recursive queries, and known computational functions. It rejects writes, schema
changes, transaction commands, PRAGMAs, attachments, vacuum, extension loading,
and file/shell functions. It never borrows a writer connection, takes a Catabolic
writer lock, initializes a database, or runs a migration.

The implementation uses SQLite's [authorizer actions](https://www.sqlite.org/c3ref/c_alter_table.html)
and [query-only setting](https://www.sqlite.org/pragma.html#pragma_query_only), plus
Python's [single-statement execution](https://docs.python.org/3/library/sqlite3.html#sqlite3.Cursor.execute)
and [progress handler](https://docs.python.org/3/library/sqlite3.html#sqlite3.Connection.set_progress_handler).
This is an application read-only interface, not a general-purpose OS sandbox.

## Query-driven output folders

SQL returning `item_id`, `file_id`, or `association_id` can be saved as a layout
selection and refreshed into a symlink catalog. See [QUERY_FOLDERS.md](QUERY_FOLDERS.md)
for the result contract, CLI workflow, and safeguards against incomplete results.

## Tagging

See [TAGGING.md](TAGGING.md) (`catabolic docs tags`) for schema 4 tag tables,
CLI commands, SQL views, GraphQL fields, boolean/descendant filters and generated
tag-selected folders. Tags apply explicitly to items or files and are shared
across profiles. Manifest v2 carries their vocabulary and attributed assertions.

`catalog_outputs`, `catalog_hardlinks` and `catalog_retained_hardlinks` describe
output link modes and regular-file ownership; see [HARDLINKS.md](HARDLINKS.md).

Schema 6 adds `catalog_jobs`, `catalog_facts`, `catalog_checksums`,
`catalog_proposals`, `catalog_decisions`, `catalog_expected`, `catalog_text`,
and `catalog_refresh`. Facts distinguish recorded-current revisions from stale
results; they do not imply a live source check. See [ENRICHMENT.md](ENRICHMENT.md).

Schema 7 adds `catalog_job_attempts`: attempt states, errors, transient retry
eligibility and deadlines, joined with profile/file identifiers. Filter by profile.
