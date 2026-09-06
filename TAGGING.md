# Tags and curation

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Tags are catalog-wide data, independent of media kind, machine profile and output
folder. They apply explicitly to logical items or inventoried files. Tagging does
not modify source filenames, embedded media tags, sidecars or symlinks. Layout
changes still require an explicit plan, apply and sync.

SQLite schema **4** introduces tags. Existing databases require the normal verified
backup and migration rehearsal:

```sh
catabolic --db catalog.sqlite3 db upgrade --dry-run
catabolic --db catalog.sqlite3 db upgrade
export CATABOLIC_DB=/absolute/path/catalog.sqlite3
```

## Vocabulary, aliases and hierarchy

Names can be simple (`favorite`) or namespaced (`genre:science-fiction`,
`mood:hopeful`, `language:ja`, `workflow:needs-review`, `collection:family`). These
namespaces are examples, not a closed vocabulary. There is no exclusive-value
constraint: an item can have several genres, languages or workflow tags.

Names use Unicode NFC and casefold normalization; surrounding whitespace is
removed, including around the first colon. Namespace prefixes use lowercase
letters, digits, dots, underscores and hyphens. Values may include spaces and
Unicode. Names are at most 255 characters; control characters are rejected.
A colon names a namespace, not a hierarchy relationship.

```sh
catabolic tag put genre:fiction --description 'Fictional works'
catabolic tag put genre:science-fiction
catabolic tag put genre:space-opera
catabolic tag alias genre:science-fiction genre:sci-fi
catabolic tag parent genre:science-fiction genre:fiction
catabolic tag parent genre:space-opera genre:science-fiction
catabolic tag list --namespace genre
catabolic tag list --search sci-fi
catabolic tag list --parent genre:fiction
catabolic tag list --child genre:space-opera
```

`tag put` is idempotent and resolves aliases. Omitting `--description` preserves
an existing description. Tags have stable database-scoped IDs. Canonical names
and aliases share one collision domain, so an alias cannot shadow another tag.

```sh
catabolic tag rename genre:space-opera genre:space-adventure
catabolic tag alias genre:science-fiction genre:sci-fi --remove
catabolic tag parent genre:space-adventure genre:science-fiction --remove
```

Renaming retains the old canonical name as an alias, preserving saved name-based
queries. Removing an alias intentionally makes queries using it fail. A tag may
have multiple parents, with cycles rejected transactionally. Limits are 100 aliases
and 100 direct parents per tag. There is no hard-delete or merge command.

## Explicit assignments and provenance

Create vocabulary first, then assign existing tags. Unknown names are errors,
which prevents typos from silently creating new vocabulary.

```sh
catabolic tag put workflow:needs-review
catabolic tag add genre:sci-fi --item ITEM_ID
catabolic tag add workflow:needs-review --file FILE_ID
catabolic --json tag add genre:sci-fi --item ITEM_ID \
  --source agent:curator --confidence 0.85 --note 'Based on synopsis'
catabolic tag assignments --item ITEM_ID
catabolic tag assignments --tag genre:sci-fi --source agent:curator --active all
```

Each `(subject, tag, source)` is one assertion. `source` defaults to `manual` and
is an exact, case-sensitive provenance label, not authenticated identity. Confidence
is optional, finite, between 0 and 1, and does not automatically accept or reject
an assertion. Notes hold evidence; descriptions and notes are limited to 4,000
characters, sources to 200. Control characters are rejected in these fields.

Repeat tags, `--item` and `--file` for bulk work. Every tag is assigned to every
selected subject. A batch is limited to 1,000 assertions and commits atomically:
unknown tags/subjects or invalid confidence roll back the whole batch. Agents can
invoke the same commands with `--json`; no interactive prompts are used.

```sh
catabolic --json tag add workflow:needs-review \
  --item ITEM_A --item ITEM_B --file FILE_C --source agent:curator
catabolic tag remove workflow:needs-review --item ITEM_A --source agent:curator
```

Removal withdraws only the chosen source's assertion, preserving its ID, evidence
and creation timestamp. Another source's active assertion still makes the tag
match. Re-adding reactivates the same ID; add replaces that source's confidence
and note with the supplied values (defaults: null and empty text). An identical
repeat does not change timestamps. Creation/latest-update timestamps and withdrawn
state are retained; this is not a complete edit history.

Item tags do not implicitly propagate to associated files, editions, episodes or
related items. File tags do not propagate to items. This avoids confusing a work's
genre with a particular copy's quality or review status. Use SQL joins when a
selection should combine those scopes.

## Boolean queries and pagination

```sh
# Both tags must match. Aliases are accepted.
catabolic item list --tag genre:sci-fi --tag mood:hopeful

# At least one genre, excluding anything awaiting review.
catabolic item list --any-tag genre:sci-fi --any-tag genre:fantasy \
  --not-tag workflow:needs-review

# Match fiction and any descendant, without storing inferred assertions.
catabolic item list --tag genre:fiction --descendants
catabolic files --tag workflow:needs-review
```

Create `mood:hopeful` and `genre:fantasy` before using those examples. Repeated
`--tag` filters use AND; `--any-tag` is one OR group; `--not-tag` excludes any
listed tag. The groups combine with AND and existing kind/year/search filters.
Only active assertions match. Unknown tags fail explicitly rather than silently
returning an empty collection. `--descendants` expands all three filter groups.

Lists support `--limit` (1–1,000) and `--cursor`. Cursors are bound to the query
filters, including descendant matching. Up to 100 tag filters are accepted.

```sh
catabolic graphql 'query {
  items(tags:["genre:fiction"], descendants:true, first:20) {
    nodes { id title taggings { nodes { tagName source confidence note active } } }
    pageInfo { endCursor hasNextPage }
  }
  tags(namespace:"genre") { nodes { id name description aliases parentIds } }
  taggings(source:"agent:curator", active:ALL) { nodes { subjectType subjectId tagName active } }
}'
```

GraphQL `items` and `files` accept `tags`, `anyTags`, `notTags`, `descendants`.
Root `tags` and `taggings`, plus nested item/file `taggings`, are paginated.
Existing GraphQL execution, output, depth and record limits apply.

## SQL, facets and generated folders

`query --schema` exposes four additional views:

| View | Contents |
| --- | --- |
| `catalog_tags` | Canonical vocabulary (`tag_id`, `name`, `description`) |
| `catalog_tag_names` | Names/aliases with `tag_id` and a `canonical` flag |
| `catalog_tag_parents` | Direct `child_id`, `parent_id` edges |
| `catalog_taggings` | Item/file assertions with provenance and active state |

Tag rows are shared across profiles. Filter `active=1`; use `DISTINCT` when several
sources assert the same tag. This query counts tagged items, not assertions:

```sql
SELECT tag_name, count(DISTINCT subject_id) AS items
FROM catalog_taggings
WHERE active=1 AND subject_type='item'
GROUP BY tag_id, tag_name
ORDER BY items DESC, tag_name;
```

Save this SQL as `favorites.sql` to select items by a canonical name or alias:

```sql
SELECT DISTINCT a.subject_id AS item_id
FROM catalog_taggings a
JOIN catalog_tag_names n ON n.tag_id=a.tag_id
WHERE a.active=1 AND a.subject_type='item' AND n.name=:tag;
```

```sh
catabolic layout put favorites --preset catabolic \
  --select-sql favorites.sql --params '{"tag":"genre:sci-fi"}'
catabolic catalog bind favorites
catabolic layout preview favorites --catalog favorites
catabolic layout apply favorites --catalog favorites
catabolic sync --catalog favorites --dry-run
catabolic sync --catalog favorites
```

After tag changes, repeat plan/apply/sync to refresh the collection. Empty
selections retain the existing explicit `--allow-empty` safeguard. GraphQL saved
selections must declare `$after: String` and forward `after: $after`, as described
in [QUERY_FOLDERS.md](QUERY_FOLDERS.md). Raw SQL names are normalized stored text;
the CLI and GraphQL normalize name filters for you.

Assignment lookups are indexed by tag, active state and subject. Descendant filters
use recursive queries anchored at the requested tags rather than expanding the
entire vocabulary. Full facet counts and complex joins can still scan many rows;
SQL keeps its existing timeout, row and output limits.

## Manifests and compatibility

Manifest **v2** introduced tagging; current exports use **v3**, adding hardlink
output metadata while retaining the tag model. Checksummed content includes `tags`, `tag_names`,
`tag_parents`, `taggings` and matching counts. It includes active and withdrawn
assertions for exported items/files, all their referenced tags, ancestors, and
aliases. Unrelated vocabulary and unrelated subjects are excluded. This remains
a catalog projection snapshot, not a full database backup.

```sh
catabolic spec schema                 # v3
catabolic spec schema --format-version 1
catabolic spec docs --format-version 2
catabolic spec validate --file old-v1.json
catabolic spec roundtrip --file tagged-v2.json
catabolic spec check                  # checks all three frozen releases
```

The frozen v1 models/artifacts are unchanged; v1, v2 and v3 validation and lossless
JSON-value round trips are supported. Unknown extension fields survive in every
version. No implicit downgrade discards tagging, and the producer does not emit
v1 documents. Consumers must support v3 before reading new exports. Native folder
layout version 1 is independent of this manifest version and remains unchanged.
