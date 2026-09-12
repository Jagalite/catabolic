<!-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors -->
<!-- SPDX-License-Identifier: MIT -->

# Media components

A movie can have English audio inside an MKV and English forced subtitles in a
separate SRT. Both are component occurrences, so the same saved query can select
either storage form. Selecting them does not change the MKV or start FFmpeg.

## Identity and evidence

Schema 25 adds an index over retained probe jobs and file associations. It does
not replace probe JSON, accepted proposals, file IDs, catalog items, or existing
selection contracts. Music `track` items remain catalog items, separate from
audio stream components. The upgrade adopts existing probe and external-association
evidence without source I/O and preserves existing table contents and IDs.

- A **component** identifies a particular mix, translation, commentary, or video.
  Matching languages or titles do not establish identity. Different translations
  retain separate IDs. Accepted identity proposals and verified extraction/mux
  lineage can relate representations of the same component.
- An **occurrence** records a component representation in an exact file revision.
  Embedded occurrences use `ffprobe_absolute_stream_index`: the absolute `index`
  from `ffprobe -show_streams`, not an audio-relative or subtitle-relative index.
  External occurrences use that convention when probed, or `whole_file` when
  known only through an association. `subtitle`, `custom:audio`, and `custom:video`
  associations supply the external kinds.
- **Compatibility** identifies the exact video revision, edition, part, timeline,
  coverage and accepted offset. Streams in the same container accompany its
  timeline. Separate files and separate containers require accepted compatibility
  evidence; a shared movie identity or similar filename is insufficient.

A changed source revision makes its old occurrences stale. Remuxed stream indexes
are new occurrences; only verified lineage or accepted evidence carries logical
identity across the change. Historical occurrences remain queryable with their
old probe evidence and assertions. `current` means current catalog evidence;
resolution additionally performs the existing bounded live source checks.
For current occurrences, container format and stream count come from the latest
probe fact, even when unchanged streams retain their occurrence IDs and original
stream evidence. Historical occurrences retain their original container evidence.

`observed` contains meaningful container attributes; `asserted` contains accepted
association, curation, or generated-lineage attributes. `technical` preserves the
raw stream evidence. Missing language and disposition flags stay null. Conflicts
remain visible and make the effective attribute null; conflicted occurrences are
ineligible for package resolution. Raw text subtitle demuxers' synthetic zero
dispositions do not establish forced/default designations. Playback preferences
belong in selection queries; they do not rewrite observed flags.

An unprobed external association is selectable with `technically_verified=0`.
A stale probe also has `technically_verified=0`, while its historical raw facts
remain inspectable. No filename-based synchronization or language inference occurs.

## Shared queries

SQL exposes `catalog_components`, `catalog_component_occurrences` and
`catalog_component_lineage`. Logical rows aggregate their associated `item_ids`;
occurrence rows have exact `item_id`, `file_id`, `association_id` and `revision`.
Use a stream kind alongside language, rather than a file-wide language summary:

```sql
SELECT occurrence_id
FROM catalog_component_occurrences
WHERE profile=:profile AND current=1
  AND kind='audio' AND language='en';
```

```sql
SELECT occurrence_id
FROM catalog_component_occurrences
WHERE profile=:profile AND current=1
  AND kind='subtitle' AND language='en' AND forced=1;
```

GraphQL offers the same normalized evidence and pagination:

```graphql
query EnglishForced($after: String) {
  componentOccurrences(kind: "subtitle", language: "en", forced: true,
                       current: true, first: 100, after: $after) {
    nodes { id componentId kind language forced storage revision technicallyVerified }
    pageInfo { hasNextPage endCursor }
  }
}
```

Save these through `query save` with `entity: "occurrence_id"` (or select logical
`component_id` values). Composition, completeness checks, pagination, cancellation,
and evaluation time/ID/byte budgets are shared with existing queries. An
occurrence selection never implicitly becomes a whole-file or association selection.

## Accepted compatibility and identity

Use `proposal put`, then the existing explicit proposal acceptance workflow.
`component schema` describes the assertion contract. A component proposal has only
`item_id` and `component` in its payload; it cannot also mutate an association:

```json
{
  "item_id": "ITEM_ID",
  "component": {
    "occurrence_id": "EXTERNAL_OCCURRENCE_ID",
    "compatibility": {
      "edition_id": "ITEM_ID",
      "video_file_id": "VIDEO_FILE_ID",
      "video_revision": "EXACT_VIDEO_REVISION",
      "part": null,
      "timeline_id": "container:EXACT_VIDEO_REVISION",
      "coverage": "full",
      "offset_seconds": 0.0,
      "synchronization": "declared"
    }
  }
}
```

The primary association can explicitly name `metadata.edition_id` and
`metadata.timeline_id`; otherwise its item ID and `container:REVISION` identify
those scopes. Compatibility must match both. `declared` and `verified` are retained
evidence labels supplied by accepted curation; accepting a declaration does not
measure synchronization. No offset is calculated automatically.

Optional `component_id` relates the occurrence to an existing same-kind component.
`attributes` can assert language, title, forced/default/commentary or accessibility
designations. Conflicting accepted assertions remain visible. `supersedes` lists
specific older assertion proposal IDs to retire while retaining their history.

Dependencies name exact `file_id`, `revision`, and `purpose` (`font`,
`subtitle_index`, `subtitle_data`, or `support`). Set `dependencies_complete` only
when the dependency set has been reviewed. ASS/SSA and paired bitmap subtitles
require that assertion for publication outside their existing container.

## Package resolution

Add `package` to an existing fallback policy. Its primary tiers still select
items/files/associations, or explicitly selected video components. Requirements
reuse ordered saved queries; watchers use the same query/fallback/projection plans
and have no component-specific filters:

```json
{
  "fallbacks": [{"name": "preferred", "query_id": "VIDEO_QUERY_ID"}],
  "package": {
    "publication": "selected_only",
    "operation_id": "APPROVED_COMPONENT_MUX_RECIPE_ID",
    "requirements": [
      {"name": "audio", "fallbacks": [{"name": "English", "query_id": "AUDIO_QUERY_ID"}]},
      {"name": "subtitles", "fallbacks": [{"name": "English forced", "query_id": "SUBTITLE_QUERY_ID"}]}
    ]
  }
}
```

An unavailable component or supporting file permits the next saved-query tier
for that requirement. These live checks share the resolver's probe budget,
timeouts and epoch validation. Missing required components after exhausting those
tiers disqualify that video and permit another video/tier.
Multiple eligible logical translations in one requirement tier block as ambiguous;
a file-ID tie break cannot silently pick a translation. Equivalent occurrences can
be chosen deterministically. Partial coverage, wrong editions/parts/timelines,
stale revisions and unknown external synchronization do not satisfy compatibility.
Existing multipart representation-group checks still apply.

The report includes selected occurrences, compatibility evidence, dependencies,
exact revisions and a packaging decision:

| Action | Meaning |
| --- | --- |
| `use_container` | Publish the existing container. The report states whether unselected components are exposed. |
| `publish_sidecars` | Publish the video and compatible external assets as one projection plan. |
| `extract` | Explicit extraction is required before sidecar publication. |
| `mux` | An approved component-mux job must create a new container. |
| `convert` | Representation conversion is required; see the limits below. |
| `block` | Requirements or compatibility cannot be established. |

`publication: "container"` permits the whole original container; `sidecars`
additionally permits compatible accompanying files. `selected_only` uses the
original only when its complete stream set exactly equals the selected set;
otherwise it requires muxing. A symlink never hides unselected streams. Pending
processing has `ready: false` and no content URL. Resolution never queues it.

## Explicit processing and publication

Create an immutable operation with
`artifact recipe selected --preset component-mux`, and explicitly bind a generated
location with `artifact bind`. Put its recipe ID in the policy. Preview using
`fallback resolve ITEM_ID --policy POLICY_ID`, then admit that exact result:

```sh
catabolic --db catalog.db component package ITEM_ID --policy POLICY_ID \
  --location GENERATED_LOCATION --expected-plan REVIEWED_PLAN_ID
catabolic --db catalog.db artifact run
```

Extract one occurrence with an existing approved recipe. Recipe `stream` options
remain kind-relative; the preview checks that they resolve to the pinned absolute
locator. A copy-only `remux-mkv` recipe must specify exactly that stream index.

```sh
catabolic --db catalog.db component extract OCCURRENCE_ID \
  --recipe RECIPE_ID --location GENERATED_LOCATION
# Review the returned plan, then explicitly admit it:
catabolic --db catalog.db component extract OCCURRENCE_ID \
  --recipe RECIPE_ID --location GENERATED_LOCATION --apply --expected-plan PLAN_ID
catabolic --db catalog.db artifact run
```

These use existing artifact claims, immutable operations, generated-location
ownership, validation, publication journals and recovery. Jobs pin component
evidence and dependencies in addition to their file inputs. Mux validation checks
stream counts/codecs, metadata, flags, every selected packet's payload hash,
timestamps against the explicitly accepted offset, and attached font bytes.
Timestamp rounding is limited to 2 ms. Each packet-evidence capture has a 64 MiB
limit and the operation retains its elapsed-time/output-byte bounds; exceeding a
bound fails the job rather than registering an unverified output.

Generated output streams inherit the recorded logical identities and retained
attributes. Extraction alone does not assert compatibility with another video.
A later resolution reuses a ready artifact only for the exact package signature.
Fallback projections publish sidecar groups or the resulting container through
existing ownership checks, journals, verification and consumer delivery. Changes
invalidate semantic watcher baselines; no-op evaluation leaves symlinks and verified
publication history unchanged. An incomplete group retains the previous publication.

## Authorization and current limits

HTTP/GraphQL contract 1.3.0 adds the query surface while retaining released 1.2.0
artifacts and exact file/revision endpoint semantics. `component:read` grants can
scope metadata to `occurrence_ids`; they do not grant parent-container bytes.
Non-operators receive only permitted component fields. Exact artifact delivery
still requires its own existing file/rendition authorization.

Packaging admission is currently a local-owner CLI/API operation. Logical HTTP
rendition requests reject package policies instead of passing the parent file to
an unrelated rendition operation. There is no direct stream endpoint or automatic
extraction. Component-only HTTP clients must obtain separately authorized artifacts.

Other intentional limits:

- Only full-coverage external compatibility is supported. Partial timelines need
  a future interval model; offsets require explicit accepted evidence.
- Multi-component conversion pipelines are not implemented. `supported_codecs`
  can report `convert`, but cannot launch a conversion/mux chain. Explicit
  single-component audio/subtitle conversion uses existing recipes; accept its
  compatibility before selecting it for a package.
- Sidecar publication supports whole-file associations and probed single-stream
  text/audio formats. Components in another multi-stream container first require
  explicit extraction and accepted compatibility for the result.
- Paired subtitle files can be published as revision-pinned sidecar groups;
  their muxing is blocked. Required dependencies of embedded components also
  require packaging; publishing the original container cannot deliver those files.
  Standalone font files can be attached during muxing;
  font sidecar layouts and extraction of embedded font dependencies are unsupported.
  Byte preservation does not certify font validity or renderer behavior.
- No automatic translation matching, attachment dependency discovery, or
  synchronization correction is performed. Muxes that cannot preserve packet
  payloads/timing within the validation bounds fail rather than claiming success.

The component tests use disposable catalogs and FFmpeg-generated media. They cover
embedded/external parity, unknown/conflicting flags, translation identity,
compatibility rejection, remux revision invalidation, fallback, dependencies,
authorization, watcher changes, grouped no-op publication, extraction/mux lineage,
packet timing and recovery. Run `python -m unittest discover -s tests` for the full
regression suite; real media tests require FFmpeg and ffprobe.
