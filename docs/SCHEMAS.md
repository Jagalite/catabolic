# Schemas and offline documentation

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

The installed CLI includes its guides and examples. Reading or searching them
requires no database, network connection, or repository checkout:

```sh
catabolic docs
catabolic docs query
catabolic docs native
catabolic docs --search 'manifest version'
catabolic --json docs
catabolic --json docs graphql
catabolic docs --search 'pagination' --json
```

Without a topic, `docs` lists the available topic IDs, titles and summaries.
A topic prints its full Markdown guide. Search matches all whitespace-separated
words, case-insensitively, anywhere in each guide or its index entry. It uses
literal text, not regular expressions. Results follow index order and contain up
to three matching lines per topic, with one-based line numbers and snippets
limited to 240 characters. A search with no results succeeds with an empty list.

`--json` works before `docs` or after it. JSON responses have
`documentation_version: 1`. Index responses contain `topics`; search responses
also contain `query`, with `matches` in each returned topic. Topic responses
contain `topic`, `title`, `summary`, `source`, and the complete `markdown` string.
`source` is a guide filename, not a required local path. Unknown topics, empty
searches, or combining a topic with search exit 2; with `--json`, errors are JSON
on stderr and stdout is empty. Successful requests exit 0.

Guides describe the installed release. Markdown links to repository code are
references; use the topic index to navigate bundled guides. Runtime discovery
commands below provide the current schemas and vocabularies.

## SQL views and stored database schema

```sh
catabolic --db catalog.sqlite3 --json query --schema
catabolic --db catalog.sqlite3 db status
catabolic docs query
catabolic docs migrations
```

`query --schema` describes query views and their row meanings, view and table
columns, supported SQL functions, parameters and query limits. It requires an
existing database. It is a discovery response, not a complete SQL DDL dump.
Prefer the documented `catalog_*` views for queries. The database schema version
and migration history are available from `db status`; migrations and the SQL
query interface have separate versioning.

```sh
catabolic --db catalog.sqlite3 --json query \
  'SELECT item_id,title,year FROM catalog_items
   WHERE kind=:kind ORDER BY item_id LIMIT 20' \
  --params '{"kind":"movie"}'
```

## GraphQL schema

```sh
catabolic graphql --schema
catabolic docs graphql
```

The schema command works without a database. The guide supplies query documents,
variables, relationship traversal, pagination and error examples.

## Metadata manifest contract

```sh
catabolic spec schema
catabolic spec docs
catabolic spec check
catabolic spec validate --file catalog.json
catabolic docs manifest
catabolic docs spec
```

`spec schema` emits the generated JSON Schema; `spec docs` emits its generated
field reference. These describe `catabolic.catalog-manifest`, not every CLI
command's JSON response. `docs manifest` describes export and refresh workflows;
`docs spec` explains semantic constraints and compatibility rules. Unknown JSON
fields survive validation and round trips. Validation does not import a manifest
into the database.

## Symlink layouts and media vocabulary

```sh
catabolic layout presets
catabolic item types
catabolic docs native
catabolic docs layouts
catabolic docs query-folders
catabolic docs media
```

These discovery commands work without a database. Presets contain executable
JSON layout definitions. `catabolic.native` v1 names symlinks using kind, item ID,
role, file ID and sanitized source filename. Its profile version is separate from
the layout language version and manifest format version. Custom and Plex layouts
can export the same manifest format without adopting native naming rules.

For options on any command, append `--help`, for example
`catabolic layout put --help` or `catabolic manifest --help`.

## Tagging

See [TAGGING.md](TAGGING.md) (`catabolic docs tags`) for schema 4 tag tables,
CLI commands, SQL views, GraphQL fields, boolean/descendant filters and generated
tag-selected folders. Tags apply explicitly to items or files and are shared
across profiles. Manifest v2 carries their vocabulary and attributed assertions.

For output link modes, hardlink ownership, retained data and manifest v3, see
[HARDLINKS.md](HARDLINKS.md) (`catabolic docs hardlinks`).
