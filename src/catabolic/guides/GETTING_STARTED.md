# Your first catalog

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

This walkthrough creates a disposable document, inventories it, identifies it,
and builds a native Catabolic symlink folder with a metadata manifest. It needs
an [installed CLI](INSTALLATION.md), Python, and a POSIX shell. It does not need
FFmpeg, a media server, or an API key. Run the blocks in the same shell.

## 1. Create an isolated workspace

```sh
CATABOLIC_DEMO="$(mktemp -d "${TMPDIR:-/tmp}/catabolic-demo.XXXXXX")"
cd "$CATABOLIC_DEMO"
mkdir source state
printf '# Library notes\nA small document for the Catabolic walkthrough.\n' > source/Notes.md
export CATABOLIC_DB="$PWD/state/catalog.sqlite3"
export CATABOLIC_PROFILE=default

catabolic init
catabolic location bind documents --root "$PWD/source"
catabolic catalog bind native
catabolic scan
catabolic --json files --unidentified --limit 20
```

The workspace contains separate `source/`, `state/`, and `catabolic/native/`
directories. `init` creates the database; binding `native` creates the default
output directory. Scanning records the source file without modifying it.
The output still has no generated media links.

A **location** is a named source tree. A **file** is an inventoried occurrence
within it. An **item** describes logical media, while an **association** says what
role a particular file has for that item. A **catalog** is an output collection,
and a **layout** generates the desired relative paths for that collection.

## 2. Identify the document

The following extracts the only file ID through a read-only SQL query, then
creates an item and captures its returned ID. Python handles JSON, so jq is not
required:

```sh
FILE_ID="$(catabolic --json query \
  'SELECT file_id FROM catalog_files WHERE profile=:profile ORDER BY file_id LIMIT 1' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["rows"][0][0])')"

ITEM_ID="$(catabolic --json item put --kind document \
  --identity 'local.document=walkthrough-notes' \
  --metadata '{"title":"Library notes","author":"Demo"}' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"

catabolic association put --file "$FILE_ID" --item "$ITEM_ID" --role primary
catabolic --json item show "$ITEM_ID"
```

Use namespace-qualified identities when you have them, such as `tmdb.movie=...`
or `local.document=...`. They are supplied facts, not lookups performed by
`item put`. Reusing an identity resolves the same logical item; conflicts fail.
Associations are independent of output placement, so an identified file can
participate in several catalogs.

## 3. Generate paths and links

```sh
catabolic layout put native --preset catabolic
catabolic layout preview native --catalog native
catabolic layout apply native --catalog native
catabolic sync --catalog native --dry-run
catabolic sync --catalog native
catabolic verify --catalog native
```

`layout preview` computes names. `layout apply` writes mappings to SQLite.
`sync --dry-run` checks the actual filesystem without claiming or changing it.
`sync` recomputes and validates the plan, then creates owned links. `verify`
independently checks the result.

The visible file follows this shape:

```text
catabolic/native/document/ITEM_ID/primary/FILE_ID/Notes.md
```

The real IDs replace the placeholders. Native naming preserves the source
basename subject to portable sanitization. Running `sync` again should leave
correct links untouched. The source remains `source/Notes.md`.

## 4. Add metadata and a query folder

```sh
catabolic tag put collection:favorite
catabolic tag add collection:favorite --item "$ITEM_ID"
catabolic --json item list --tag collection:favorite
catabolic manifest --catalog native --in-catalog \
  --extra '{"description":"My first Catabolic catalog"}'
catabolic spec validate --file catabolic/native/.catabolic-manifest.json
```

Now make another folder from a saved SQL selection. This particular selection
includes all document items; see [tagging](TAGGING.md) for tag-selected SQL.

```sh
cat > documents.sql <<'SQL'
SELECT item_id FROM catalog_items WHERE kind='document' ORDER BY item_id;
SQL
catabolic catalog bind reading
catabolic layout put reading --preset catabolic --select-sql documents.sql
catabolic layout preview reading --catalog reading
catabolic layout apply reading --catalog reading
catabolic sync --catalog reading --dry-run
catabolic sync --catalog reading
catabolic verify --catalog reading
```

Both output folders reference the same source. Changing `documents.sql` does not
change the saved selection until another `layout put`. New matches appear after
another preview/apply/sync. [Query folders](QUERY_FOLDERS.md) explains completeness,
empty-result protection, and GraphQL selections.

## 5. Inspect and keep the fixture

```sh
catabolic --json query \
  'SELECT catalog,catalog_path FROM catalog_entries WHERE profile=:profile AND active=1 ORDER BY catalog,catalog_path'
catabolic graphql '{ items(first:10) { nodes { id title kind } pageInfo { hasNextPage endCursor } } }'
catabolic status
printf 'Walkthrough directory: %s\n' "$CATABOLIC_DEMO"
```

The walkthrough leaves the fixture for inspection. It does not clean up or touch
a real library. Its small document demonstrates catalog mechanics, not video
probing or media-server compatibility.

## Move to real media

Create a new database/workspace outside your source trees, then bind your actual
source directories. Register multiple locations in the same database to combine
drives. Use `files --unidentified` to find files needing curation; supply verified
media identities manually or use proposals and review them explicitly.

Choose a layout appropriate to the media. Movies generally need title/year;
episodes need season/series relationships; tracks need album/artist relationships.
`catabolic target show NAME` describes a target's rules.
[The media model](MEDIA_MODEL.md) explains these relationships and file roles.

Keep preview, mapping application, sync, and verification as separate steps until
you understand the results. For custom roots, source replacement, recovery, and
routine runs, continue with [operations](OPERATIONS.md).
