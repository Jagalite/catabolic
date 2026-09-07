# Catabolic parity and improvement roadmap

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Status: roadmap accepted on 2026-09-06. Schema 6 implements the first workflows
across these areas, including the lightweight processing pipeline.
[ENRICHMENT.md](ENRICHMENT.md) is the authoritative implemented-command guide.
The targets below remain acceptance criteria, not claims of full application
compatibility: live server/provider certification, rich book/EXIF extraction,
and event-driven watching remain outside the implemented scope.

Schema 8 adds [saved processing artifacts](ARTIFACTS.md): immutable recipes,
separate generated locations and recoverable publication for a small preset set.
Application ingestion adapters and streaming packages remain deferred.

## Objective and current foundation

Complete useful media-catalog workflows demonstrated by FileBot's public scripts,
beets, and Library. Improve their fit for Catabolic through evidence, repeatable
operations, and safe refreshes. Feature count is not a release criterion.

Catabolic already provides multi-source inventory, explicit identities and file
roles, item relationships, tags with assertion provenance, SQL and GraphQL,
query-based projections, layouts, manifests, and recoverable symlink/hardlink
outputs. These are foundations to extend, not replace. Filesystem observations
currently record size, modification time, device, inode, and availability;
optional probing and identification proposals are now implemented as described
in ENRICHMENT.md.

The agent supplies media judgment; the core supplies validated catalog operations
and filesystem safety. Identification can also be performed by a person. New
capabilities must be accessible through the CLI without requiring an agent,
service, or graphical application.

## Useful parity and targeted improvements

### 1. Media probing

- Parity: read technical video/audio properties, streams, languages, chapters,
  embedded audio tags, and image metadata. Library's [AV extractor][library-av]
  and [file metadata extraction][library-metadata] are references.
- Improvement: preserve structured streams and raw observations alongside
  normalized query fields. Record extractor name/version, options, observed file
  revision, and failures. Separate extracted facts from accepted descriptive
  metadata; refreshing a probe cannot overwrite curation.
- Completion: real fixture files produce expected SQL/GraphQL results and layout
  values; changed files invalidate prior results; unchanged eligible results are
  reused. An unavailable optional tool or malformed file is an explicit per-file
  result, not a failed inventory scan. Add book/comic-specific extraction when a
  supported workflow needs it, using the same result contract.

### 2. Content identity and integrity

- Parity: find duplicate bytes and recheck recorded checksums. FileBot's
  [duplicate detector][filebot-duplicates] uses staged filtering; Library's
  [media checker][library-check] offers sampled and full decoding checks.
- Improvement: retain separate file occurrences, byte-content identities, and
  logical media items. Group same-inode references before reading duplicate
  data. Size and sampled hashes narrow candidates; full cryptographic hashes
  establish content groups. Label sampled checks explicitly; they cannot certify
  whole-file integrity or authorize deletion. Decoding and checksum checks answer
  different questions and must have separate results.
- Completion: identify copies across sources without merging occurrences or
  editions; detect changed bytes; reject results if the observed file changes
  during processing. A failed integrity check preserves the previous baseline.
  Content equality can support relocation proposals, but applying a proposal must
  preserve old occurrence history and revalidate current source/output state.
  This work adds reporting and repair proposals, not source deletion.

### 3. Identification and metadata review

- Parity: obtain candidate identities and compare their evidence. Beets'
  [matcher][beets-match] demonstrates recommendation thresholds and ambiguity.
- Improvement: persist candidates, evidence, conflicts, actor, and accepted or
  rejected decisions. Record field provenance and preserve user corrections on
  refresh. Scores are ranking signals, not calibrated probabilities or permission
  to mutate. Start with agent-supplied candidates and one provider adapter for a
  concrete workflow; avoid a general plugin framework initially.
- Completion: CLI users and agents can submit, inspect, accept, or reject a
  proposal; stale acceptance fails clearly; repeat acceptance is idempotent;
  authoritative identity conflicts are blocked. A provider outage preserves
  existing decisions. Decision history is sufficient to explain changes and
  propose a reversal, subject to current-state validation.

### 4. Choosing among copies

- Parity: rank alternate copies by explicit preferences. FileBot's
  [duplicate selection][filebot-duplicates] distinguishes binary and logical
  duplicates and supports quality ordering.
- Improvement: apply catalog-specific selection before layout generation, using
  the existing SQL selection machinery where practical. Separate mandatory
  requirements from ranked preferences. Explain selected and rejected candidates;
  require an explicit tie-break policy for equal candidates. Editions remain
  distinct unless the user chooses to group them.
- Completion: one catalog can prefer a compatible 1080p copy and another a 4K
  copy, with stable repeat results. Missing probe values mean unknown, not false.
  An unavailable source produces a blocked or explicitly configured fallback
  decision; it never weakens reconciliation's current safety requirements.

### 5. Sidecars and compound media

- Parity: associate subtitles and artwork with their media. FileBot's
  [automation script][filebot-amc] checks subtitle relationships and embedded
  subtitle languages.
- Improvement: discover association proposals using Catabolic's existing roles
  and relationships. Preserve language, forced/SDH flags, parts, and competing
  associations. Include selected sidecars consistently in a projection.
- Completion: multilingual subtitles, orphaned sidecars, ambiguous basenames,
  multiple editions, and changed sidecars have deterministic fixture results.
  Ambiguity remains reviewable; discovery never rewrites source names or tags.

### 6. Collection completeness

- Parity: report missing episodes and album tracks, as demonstrated by FileBot's
  [missing-episode script][filebot-miss] and beets' [missing plugin][beets-missing].
- Improvement: compare catalog contents with a versioned expected-members list
  supplied by a provider or user. The same comparison supports volumes or other
  ordered collections without media-specific hardcoding. Preserve ordering scheme,
  edition, source, and retrieval time.
- Completion: distinguish absent members, unavailable files, unidentified files,
  and unknown expectations. Handle specials, multi-episode files, multi-disc
  releases, and provider revisions without declaring an unavailable provider's
  empty response to be a complete collection.

### 7. Resumable processing and watching

- Parity: resume interrupted work and avoid reprocessing completed inputs. Beets'
  [import state][beets-state] separates in-progress work from incremental history.
- Improvement: use a small SQLite-backed job runner invoked from the CLI. Key work
  by file revision, operation, tool version, and options. Store checkpoints and
  per-file errors; bound worker counts and outstanding work. Run external tools
  outside write transactions and revalidate before publishing results.
- Completion: interruption, retry, cancellation, and multiple runner attempts do
  not silently skip work or publish stale results. Inventory remains fast without
  launching probes. Add watching later as a job trigger with debouncing, a file
  stability window, and periodic reconciliation to recover missed events.

### 8. Search within content

- Parity: search subtitle dialogue, chapter titles, and document text. Library's
  [AV extractor][library-av] and [text extraction][library-metadata] provide examples.
- Improvement: return the originating file and stream/page/time location, preserve
  extraction provenance, and invalidate stale text. Expose search through the
  existing bounded CLI/SQL/GraphQL interfaces, with rebuildable derived indexes.
- Completion: hits resolve to their recorded origin, changed content is refreshed,
  and query limits still hold. Detect optional SQLite full-text capabilities
  explicitly. Start with already extracted text; defer OCR and transcription until
  users need them and resource costs have been measured.

### 9. Application refresh after output changes

- Parity: notify a media server of completed changes. FileBot's
  [automation script][filebot-amc] supports Plex, Jellyfin, and Kodi refreshes.
- Improvement: use a small explicit adapter with a durable notification record,
  bounded retries, and credential redaction. Keep delivery status separate from
  filesystem success. Design for at-least-once delivery: a crash after sending can
  result in a repeated refresh.
- Completion: failed notifications can be retried without repeating link changes;
  empty or failed sync runs do not trigger false success notifications. Publish
  compatibility claims only for tested application versions. Begin with the app
  actually used for the integration test, then add others as needed.

## Lightweight, scalable probe mechanism

This mechanism is part of the accepted first milestone, not an additional service.
The filesystem walker now streams metadata without opening media contents, using
bounded buffers and disposable SQLite staging. Technical probing is a separate
optional job. ENRICHMENT.md documents the enforced limits and remaining platform
and format boundaries.

- **Separate costs:** inventory reads filesystem metadata; optional type detection
  reads a bounded signature; technical probing reads container/stream metadata;
  hashing and decoding are explicit deeper jobs. An ordinary scan or query must
  not silently trigger expensive processing.
- **Bound discovery memory:** stream traversal and stage observations in database
  batches, including bounded directory enumeration and error reporting. Publish
  the completed scan generation atomically. Failed scans retain diagnostics but
  must not publish partial presence/absence conclusions. Durable job claims and
  query pagination must also avoid loading the entire pending set into memory.
- **Probe selected work:** accept existing query selections or explicit IDs;
  process missing/stale results by default, with explicit refresh. Include profile,
  binding identity, file revision, extractor version, and options in cache
  eligibility. Treat reuse as a cache decision, not an integrity guarantee.
- **Bound execution:** use a configurable small worker pool, a global limit, and
  limits shared by sources on the same storage device or configured NAS group.
  Cap in-flight work, per-job elapsed time, captured output, and parsed record
  sizes. Use extractor analysis budgets where available; these are not universal
  hard limits on bytes read or process memory. Measure resource use and document
  which limits are actually enforced on each supported platform.
- **Keep persistence short:** run tools outside write transactions, use one
  coordinated result writer with bounded batches, and revalidate the source
  snapshot before commit. Interrupted jobs remain retryable; queries read stored
  results without launching tools or contacting sources.
- **Represent incomplete knowledge:** retain per-extractor outcomes such as
  complete, partial, unsupported, timeout, changed-during-read, and failed, with
  field-level availability where needed. A cheap probe may omit expensive fields;
  deeper inspection requires explicit policy. Do not promise header-only reads
  for every container or interpret a missing field as a negative fact.
- **Measure scalability:** benchmark initial processing, unchanged repeats,
  small changed subsets, large directories, failures, and interruption/resume.
  Verify bounded queues and scan memory separately from external-tool memory;
  include local disk and NAS measurements before tuning worker defaults.

For video/audio, use an optional adapter around [ffprobe][ffprobe-docs], selecting
the required output fields. Its [format analysis options][ffmpeg-format-options]
allow a cost/completeness tradeoff, supplemented by runner-enforced limits. Keep
the same result/job interface for other media extractors. No background daemon is
required for the first implementation.

## Delivery sequence

1. **First useful enrichment workflow:** add revision-bound result storage and a
   minimal resumable CLI runner together with video/audio probing. Extend to image
   metadata; expose normalized values to queries. This is a vertical implementation,
   not an infrastructure-only release.
2. **Trust and identity:** add full hashing, duplicate reports, repeat integrity
   checks, and bounded work scheduling. Add optional decoding checks independently.
3. **Curation and projection:** persist identification/association proposals and
   acceptance history; add one provider, sidecar discovery, explicit copy selection,
   and expected-members completeness reporting in independently testable increments.
4. **Convenience where useful:** add content search from existing extracted text,
   watching, and a tested application refresh adapter. Each can ship independently;
   none is required to complete the first three milestones.

Audio fingerprint matching, OCR, transcription, automatic downloads, transcoding,
and broad plugin infrastructure are outside this initial sequence. Add them only
when they solve a demonstrated cataloging need. Source mutation is not required
for any workflow in this roadmap.

## Shared release criteria

- Use additive numbered SQL migrations with backup/rehearsal and preservation
  tests. Preserve existing occurrence IDs, curated data, and link ownership.
- Define internal schemas when implementing each slice. Publish stable portable
  results through a new manifest version when needed; keep machine-local job
  mechanics out of the interchange contract and retain frozen schema versions.
- Provide CLI help, bounded JSON results with actionable errors, query examples,
  and offline documentation with each implemented capability. Proposed command
  names are not compatibility promises.
- Test temporary real-media fixtures, unavailable sources, changing files, tool
  timeouts, crashes, retries, and existing hardlink final-reference protections.
  Validate migrations against populated historical database fixtures.
- Measure cold/warm runs, throughput, bytes read, peak memory, database size,
  cancellation latency, and foreground query latency. Use metadata-scale fixtures
  for 10k/100k/1M occurrences separately from actual probing/hashing datasets; do
  not infer media-processing times from synthetic metadata benchmarks.
- Establish current scan/query baselines before setting regression budgets.
  Reprocessing unchanged eligible inputs should launch no extractors or hash reads;
  integrity rechecks intentionally reread content. Fast cache eligibility is not
  proof of unchanged bytes, especially when external edits preserve timestamps.
- Application compatibility requires real consumer validation in addition to
  naming fixtures. Record what was tested; do not claim superiority or parity
  based solely on the presence of a similarly named feature.

## Source review scope

The references below pin the source snapshots reviewed. FileBot coverage is its
public Groovy scripts, not an audit of its application core. The roadmap proposes
Catabolic behavior; it does not claim these other projects lack similar safeguards.

[filebot-amc]: https://github.com/filebot/scripts/blob/859d2ad985e12e1f1514aa9f8c8f46be3008c439/amc.groovy
[filebot-duplicates]: https://github.com/filebot/scripts/blob/859d2ad985e12e1f1514aa9f8c8f46be3008c439/duplicates.groovy
[filebot-miss]: https://github.com/filebot/scripts/blob/859d2ad985e12e1f1514aa9f8c8f46be3008c439/miss.groovy
[beets-match]: https://github.com/beetbox/beets/blob/ba4787f5744d161a3ed5324f3fcbcd93f02d2698/beets/autotag/match.py
[beets-state]: https://github.com/beetbox/beets/blob/ba4787f5744d161a3ed5324f3fcbcd93f02d2698/beets/importer/state.py
[beets-missing]: https://github.com/beetbox/beets/blob/ba4787f5744d161a3ed5324f3fcbcd93f02d2698/beetsplug/missing.py
[library-av]: https://github.com/chapmanjacobd/library/blob/ed9005211d53757d43a39542f60c4be9d64c2938/library/createdb/av.py
[library-metadata]: https://github.com/chapmanjacobd/library/blob/ed9005211d53757d43a39542f60c4be9d64c2938/library/createdb/fs_add_metadata.py
[library-check]: https://github.com/chapmanjacobd/library/blob/ed9005211d53757d43a39542f60c4be9d64c2938/library/mediafiles/media_check.py
[ffprobe-docs]: https://ffmpeg.org/ffprobe.html
[ffmpeg-format-options]: https://ffmpeg.org/ffmpeg-formats.html#Format-Options

## Release hardening follow-up

Schema 7 adds explicit whole-plan removal limits and opt-in bounded transient
retries with durable attempt history. RELEASE_TESTING.md defines the installed
CLI media, real-mount, and isolated Jellyfin acceptance lanes and their limits.
