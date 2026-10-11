# Native bounded Plex import checkpoint

Reference: `453fca983222c6665a48775eef67c489c96b527e`. This is an opt-in implementation checkpoint, not a full migration or release gate. Python remains the default. The frozen public-surface inventory describes the earlier missing-owner state; this document records the new import owner only.

## Interface and ownership

The companion operation is:

```sh
catabolic-native --db /disposable/catalog.db operation plex-import run --options \
  '{"connection":"home","library_id":"7","mappings":[{"remote_root":"/media","location":"seed"}],"offset":0,"limit":100}'
```

Apply requires `"apply": true` and the exact preview `"expected_plan"`. The native operation refetches all remote evidence on apply. It does not create a consumer binding, rewrite a mapping, alter media bytes, or send remote writes. This JSON operation is not the frozen `consumer import` argparse/renderer surface, and it does not claim a persistent public `catabolic.plex_import` Python facade.

- `catabolic-runtime/src/plex_import.rs` owns page reads, strict XML intake, bounded pagination, cancellation polling, and orchestration
- The existing managed `consumers::Adapter` owns HTTP, credentials, deadlines, byte/XML limits, redirect policy, and server/library validation; import does not use a Python network or business callback
- `catabolic-store/src/plex_import.rs` owns normalized mappings, source-bound candidate snapshots, complete review digests, deduplication/deferred rows, and per-file decisions
- Existing curation owners create proposals, preserve manually edited metadata, and accept decisions with fresh source and proposal checks
- `api::execute_plex_import_with_poll` is the cancellable operation boundary. The CLI signal bridge gives this operation exit 130 on interruption

## Preserved contract

Pages contain at most 100 records, 100 parts per record, and 1,000 parts in total. The request uses the rating-key wire sort `id:asc`, requests GUIDs, and rejects inconsistent offset/size/total values and duplicate rating keys. ASCII-only decimal parsing retains arbitrary integers, zero normalization, and the frozen interpreter's default 4,300-digit ceiling. Movie, episode, track, and photo fields, multipart slots, GUIDs, and deterministic server/library/rating-key identities are preserved.

Mappings are nonoverlapping normalized absolute remote roots, local locations, and relative subtrees. Candidate snapshots include inventory, bindings, source policy, existing items, identities, and associations. Live source validation, existing association/part conflicts, changed Plex GUIDs, kinds, duplicate files/parts, unavailable paths, and incomplete remote items remain explicit outcomes.

The SHA-256 plan covers database identity, profile, full connection evidence, library identity, normalized mappings, page parameters, complete parsed records, candidates including snapshots, and deferred rows. The encoded `[data, candidates, deferred]` review budget is 4 MiB. A changed local/remote review refuses apply before any proposal is written.

Each proposal put and decision is independently durable, as in the frozen owner. There is no batch-wide transaction. A decision error reports the file and stops the remaining candidates; earlier accepted files remain committed. Interruption propagates rather than becoming a deferred/error row. A pending proposal can be reused on continuation, and already accepted unchanged files do not produce additional decisions.

All HTTP waits run with the catalog detached and no Store writer lock. After reattachment, catalog identity and connection evidence are fenced again. Signal polling continues every 25 ms while a read is outstanding. Cancellation joins the managed read rather than leaving a background thread; its current request remains bounded by the existing 15-second HTTP deadline. Cancellation checks prevent local admission and occur between candidate/proposal/decision steps. No new retry behavior is introduced.

Independent differential review found that roxmltree string attribute lookup also accepts namespaced attributes by local name, unlike the frozen ElementTree parser. The import parser now explicitly requires unqualified attributes everywhere. Regression cases cover namespace-only attributes, foreign attributes preceding genuine attributes, namespaced child elements, and large decimal metadata. A second review finding separated the final server-identity and library checks with a cancellation checkpoint, preventing a new library request after interruption of the identity read. Regressions also verify catalog replacement refusal and no post-cancellation request.

## Executed evidence

The tests use fake credentials, disposable catalogs and files, and loopback HTTP servers only. No live Plex service, login, real token, output publication, push, or commit was used for this import qualification.

- `tests/test_rust_plex_import.py`: 32 subprocess cases. All 19 frozen import scenarios are retained, with thirteen additional regressions. Ordinary previews compare the complete frozen/native JSON, including plan IDs, and byte-for-byte database immutability. Applied catalog/provenance rows are compared as well
- Additional subprocess coverage includes writer progress during blocked reads, connection revision changes while detached, malformed responses, HTTP failures, record/part/plan budgets, actual SIGINT/exit 130, no subsequent request after cancellation during final validation, catalog replacement refusal, and continuation without duplicate items
- `catabolic-store/tests/plex_import.rs`: deterministic interruption after the second pending proposal proves the first decision remains committed and the pending proposal is reused; source change after proposal put proves errors stop the third candidate without rolling back the first; local/connection review changes and mapping/page bounds are covered
- Runtime unit coverage checks integer boundaries, strict pagination, multipart ordinals, GUID deduplication, namespaced container refusal, and exact unqualified XML attribute matching
- The focused suites, `cargo clippy -p catabolic-runtime -p catabolic-store --all-targets --offline -- -D warnings`, and formatting checks pass in the isolated Linux worktree

The pinned Python 3.14.6 interpreter imports the exact frozen source for differential tests. Native 3.11/3.14 wheel and broader aggregate/platform qualification are separate evidence; these source-level import tests do not replace them.

## Reusable boundary and remaining work

`plex_import::page` is crate-visible for the later Plex matching owner and returns records, total, and next_offset. Shared adapter methods and quoting/detachment are crate-visible; transport behavior remains owned in one place. Store intake receives one `RemotePage` carrying connection/library/mapping/page evidence.

Matching still needs its published-link, unique-association and source-revision snapshot owner, bounded complete-library scan, exact-path match, and accepted-association provenance. Metadata publication still needs whole-batch local/remote review, publication serialization, durable started/verified/uncertain events, one write/readback per item, and stop-on-uncertainty behavior. Those owners, legacy CLI parity, the public Python import facade, real-service acceptance, platform/rollback/performance evidence, and separately blocked independent reviews are outside this checkpoint.
