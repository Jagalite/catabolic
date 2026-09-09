# Owner trust and validation evidence

Trust permits an operation to proceed. Evidence says what was checked. Transactions,
journals, revision comparisons and worker leases keep state coherent regardless of
trust. There is no switch that disables all three.

## Local source policy

Policies belong to a source **in a machine profile**. Strict remains the default.
Schema 20 extends the local schema-19 source policy with individual checks,
monotonic revisions and an append-only change history. Upgrading preserves existing
strict/path choices, recording existing rows as migration baselines; it does not
assert that their historical checks ran. Existing catalogs require explicit upgrade.

```sh
catabolic location trust seed1
catabolic --machine location trust seed1
# Preview, then persist only a UUID exception:
catabolic location trust seed1 --uuid skip
catabolic location trust seed1 --uuid skip --apply
# Reset to strict, then explicitly exempt device renumbering:
catabolic location trust seed1 --identity strict --device skip --apply
# Existing path preset: skip UUID, device and inode for this source only:
catabolic location trust seed1 --identity path --apply
catabolic location trust seed1 --identity strict --apply
```

An omitted preset retains current settings. An explicit preset resets all three
checks before explicit check settings are applied. Each check accepts `check` or
`skip`; unknown names/values fail before mutation. A no-op does not advance the
revision. Inspection includes expanded settings, revision, precedence and history.
Generated artifact sources cannot acquire identity exceptions. These commands do
not change output policy or file-version checks. One-time replacement adoption
remains `remount --trust-source NAME`, previewed before applying; it records the
replacement in the existing remount audit without enabling persistent trust.

Policies do not create missing directories, tolerate unavailable media, or authorize
source writes. Even path mode checks regular files, recorded size/mtime/device/inode,
path traversal and output ownership. A changed root during inventory still aborts
publication of that inventory. Re-scan a replacement source before using its files.

## Evidence and pending work

Scan results include `validation`: the effective policy/revision, `allowed`,
`availability`, individual check outcomes and `identity_verified`. Skipped checks
say `skipped_owner_policy`; unavailable or mismatched identities never become
matched. A UUID without an enrolled baseline says `not_enrolled`. A completed scan
can have `identity_verified: false`. `complete` describes inventory completion,
not proof of storage provenance. Its validation is stored under
`meta[scan:<id>:validation]` in the same transaction as the scan observations.
Policy edits cannot rewrite this historical record. Read it through the registered
`catalog_scan_validation` SQL view: `complete=1 AND identity_verified=1` selects
completed scans with full root identity evidence; NULL remains unknown, not true.

Symlink verification reports source validation separately from `healthy` and
`verified_links`. Healthy means the selected links and current file revisions
passed publication checks under the effective policy. It does not establish every
source's storage identity. Consumer attempts retain the publication validation in
their captured snapshot; scan acceptance continues to mean request acceptance,
not indexed content. Existing hardlink reports do not supply this expanded identity
evidence; absence must be treated as unknown.

New processing snapshots retain source policy/revision and enrolled UUID alongside
the existing exact file revision. Reads enforce those captured checks. Existing
inventory/snapshot comparisons reject a result if policy changed before commit;
queue a new job for the new revision. Old jobs retain their original strict snapshot
checks. This does not weaken worker lease fencing or rewrite completed facts.
Read-only queries report recorded inventory, not live health. File/job completion
gates still require actual version-matching facts; identity permission supplies no
hash, decode result, rendition validation or indexing evidence.

## Check classification and retained boundaries

| Check | Responsibility and owner control |
| --- | --- |
| Source root UUID/device/inode | Trust assumption; individually configurable above. Descriptor and file revision checks remain separate. |
| Source availability and freshness | Evidence: required live reads for processing/publication. Queries may read offline inventory, without refreshing its age or health. No new freshness shortcut is provided. |
| Output marker, ownership ledger, replacement checks | Ownership permission plus reconciliation integrity. Strict; source policy cannot authorize replacing unrelated files. |
| Relative paths, symlink traversal and bound subtrees | Authorized scope plus filesystem integrity. Strict and component-aware. |
| Plex/Jellyfin server and library identities | Local binding permission and delivery destination evidence. Explicit repair/rebinding, never name-only adoption; no identity-ignore setting. |
| Credentials, TLS, redirects | Authentication and transport policy. Credential references/redaction, verified TLS and redirect refusal remain independent. This change adds no TLS bypass. Configure a trusted certificate or an explicitly selected HTTP endpoint where appropriate. |
| Queries and imports | Read-only query execution, bounded results and explicit local binding. Imported definitions cannot set source trust, credentials or consumer authorization. Frozen manifests remain unchanged and convey no new identity-verification guarantee. |
| Transactions, foreign keys, journal recovery, leases | Internal correctness, always enforced. Trust cannot make a stale worker current. |
| Processing limits, removal budgets | Explicit operation permissions/limits through existing command settings; no identity policy changes these limits. |
| Original media | Read-only. Any source mutation needs a separate explicitly authorized operation. |

Policy edits require recovered state and reject active consumer delivery leases.
Pending deliveries recheck current publication before claiming work; disabled or
invalid bindings remain blocked. Policy edits do not cancel already-sent remote
requests. Consumer failures and human notification retries remain separate from
publication health. Notification text continues to report publication or scan
request outcomes, not storage-identity or remote-indexing proof.

## Scope and limitations

This refinement intentionally adds no global trust defaults, offline publication,
configurable TLS exception, weaker content-validation mode, or correctness bypass.
Existing operation budgets remain their own CLI settings. Policy history and scan
validation are local catalog records, not new frozen interchange fields. Legacy
facts without validation records have unknown historical root evidence. Expanded
hardlink identity reporting and a dedicated completion gate for storage provenance remain
future work; do not infer those guarantees from existing `complete`/`healthy` fields.
