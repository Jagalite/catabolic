# Proving the complete cataloging experience

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Status: reference corpus, executable journey and independent mechanical evaluator
implemented on 2026-09-07. Isolated agent trials and transcript adjudication remain
pending. Reference results do not establish agent identification quality.

Prove one understandable journey through a messy collection: inventory →
identification review → version selection → publication → repeat maintenance.
Fix existing commands and documentation where this journey reveals friction.
This phase does not require another processing subsystem or a schema change.

## Run and inspect the reference journey

From a checkout, use the Python interpreter of an installed Catabolic wheel
(see [release verification](RELEASE_TESTING.md)). FFmpeg must be on PATH. The
runner rejects an existing output directory and leaves its fixture for inspection:

```sh
CATABOLIC_EXPERIENCE_ROOT="$(mktemp -d)/run"
python scripts/experience_acceptance.py \
  --python /tmp/catabolic-release-env/bin/python \
  --root "$CATABOLIC_EXPERIENCE_ROOT"
python scripts/experience_evaluator.py --root "$CATABOLIC_EXPERIENCE_ROOT"
```

The runner creates two source roots, inventories 12 occurrences using four-row
pages, reviews nine supported occurrence identities and explicitly defers three.
A movie and its duplicate remain one logical title; the subtitle shares its
episode identity. An additional supported occurrence arrives during maintenance,
bringing final resolvable coverage to ten out of ten, with three deferrals.
These counts describe fixture occurrences, not ten distinct titles.
The ambiguous movie retains two unaccepted proposals in SQLite; the review file
records the missing evidence for every deferred occurrence. Repeat snapshots
include pending proposals, curation status and the worklog as well as links.

The originals catalog prefers larger copies, then source `a` to break the exact
duplicate tie. The separate smaller-only catalog selects an exact rendition
definition and starts empty. One render is interrupted after completion commits;
the durable refresh worker publishes it after restart. A second render publishes
its link immediately through the normal producer path. Both generated videos
must be smaller than their input. Each source keeps its original bytes.

Publication is also interrupted after a journaled filesystem operation. Recovery
explicitly targets the `originals` catalog. A simulated source-root replacement
must block scan/sync safely; restoring the original root restores healthy links.
Two unchanged maintenance cycles before and after the new arrival must retain
identities, metadata, relationships, link targets and link inodes, with zero sync
operations. The deliberate fault injection lives only in child test processes;
the shipped CLI gains no failure switches.

The fixture retains these artifacts:

| Artifact | What to inspect |
| --- | --- |
| `a/`, `b/` | Messy source filenames, duplicates, subtitles and damaged media |
| `originals/`, `mobile/` | Actual native-layout links to selected sources and renditions |
| `generated/` | Two generated smaller videos and the ownership marker |
| `catalog.sqlite3` | Persisted identities, proposals, episode relationships and publication state |
| `transcript.jsonl` | Every CLI invocation, stdout, stderr and exit code, including both forced exits |
| `checkpoints.json` | Semantic and filesystem snapshots around unchanged cycles, plus recovery observations |
| `review.json` | Scripted acceptance/deferral decisions with reasons, explicitly labeled `reference` |
| `source-hashes.json` | Source byte hashes for independent rechecking |
| `report.json` | Scorecard, package installation information, tool versions and harness/corpus hashes |
| `answer-key.json` | Reference expectations; never expose this to a measured agent |

The evaluator reopens SQLite read-only, inspects actual link targets and checks
source bytes rather than trusting a saved `passed` field. Its negative tests cover
wrong identities (including corrected accepted proposals), unsupported confident
claims, deferring everything, omitted cases, broken/extra links, absent recovery
evidence and repeat churn. The reference lane runs in Linux/macOS release CI.

This initial corpus is public and synthetic. Its `evidence.json` deliberately
contains the reference evidence; neither that file nor this transcript constitutes
a held-out agent trial. General transcript claim classification, independently
observed human intervention timing, and a blinded agent-operated recovery journey
are still required before the full milestone can close. The evaluator refuses
to turn an agent-authored review file into a passing agent verdict.

## Representative collection

Build a small, reproducible, fictional collection on two disposable source roots.
Generate playable media with known contents and preserve source hashes. Use local
evidence packets to keep identity evaluation independent of changing providers.
Keep source occurrences, logical identities, editions and renditions distinct.

| Required case | Expected behavior |
| --- | --- |
| Clear title/year with a supporting identity record | Accept the supported identity |
| Same title in two years | Keep the releases separate |
| Misleading filename conflicting with reliable supplied evidence | Explain the conflict and use the stronger evidence |
| Duplicate bytes across roots | Retain both occurrences without inventing another title or deleting sources |
| Explicitly different edition or cut | Preserve the edition distinction |
| Larger and smaller copies of one title | Select using each catalog's explicit policy |
| Smaller rendition becomes ready later | Leave it absent from the smaller-only catalog until ready, then add its link automatically |
| Episode and subtitle sidecar | Preserve series/season/episode identity and the subtitle role |
| Unicode titles, spaces and irregular paths | Generate readable valid paths without losing identity |
| Two equally plausible identities | Preserve alternatives and missing evidence; publish neither as certain |
| File without reliable identity evidence | Leave explicitly unresolved |
| Truncated or empty media | Report analysis failure and exclude from the playable catalog |
| Interrupted publication | Recover owned output without repeating completed generation |
| Temporarily unavailable fixture root | Preserve curation; do not treat an incomplete scan as deletion authority |

Give every case a stable ID. Review an answer key before agent trials: allowed
identities, editions, roles, catalog membership, link targets and unresolved
outcomes. Several decisions may be valid. Do not derive expected answers from
Catabolic's output or filenames alone.

Separate the agent's evidence packet from the evaluator's answer key and the
reference transcript. The measured agent receives the task, permitted evidence,
catalog policy and CLI documentation in an environment where it cannot read the
answers. Invalidate any trial with answer leakage. The public walkthrough can
show answers; measured trials require a separate held-out case variant.

## One observable workflow

1. **Inventory:** bind both roots, require complete scans, enumerate every page
   and explain the unidentified backlog. Save occurrence counts and source hashes.
2. **Identification review:** inspect evidence, review proposals and record why
   identities are accepted. Deferred cases must name alternatives or missing
   evidence. Do not create guessed identities simply to attach completion status.
3. **Version selection:** configure originals and smaller-only catalogs with
   explicit output-definition and copy policies. Review membership, editions and
   exclusions before changing mappings.
4. **Publication:** preview, apply, sync and independently verify paths and
   targets. Enable automatic updates for the smaller-only catalog. Show a
   missing rendition becoming ready and its link appearing without a manual
   layout/sync command. No Plex scan or consumer notification is involved.
5. **Recovery:** force a process stop after durable publication intent, and
   separately after rendition completion commits but before refresh finishes.
   Retain interruption evidence, reopen the CLI, inspect and recover. Verify
   completed renditions were not regenerated. Exercise the controlled root outage.
6. **Repeat maintenance:** run two unchanged cycles, introduce one newly
   identifiable fixture occurrence, then run two more cycles. Only intended new
   membership may appear; unresolved decisions and human corrections must survive.

Show the expected visible state after each stage and the next action for every
blocker. Keep the normal path readable, with branches for ambiguity, unavailable
storage and interruption. Run every walkthrough command against the retained
fixture; illustrative commands alone are not acceptance evidence.

## Agent benchmark protocol

Use the same workflow and independent evaluator for a scripted reference run and
agent trials. Label them separately: supplied reference decisions prove the CLI
path, not an agent's identification ability.

Freeze the corpus revision, task, evidence, human-answer policy, tool access,
time limit and intervention budget before comparing agents. Record the package
commit/wheel hash, model identifier, prompt, agent/tool configuration, platform
and FFmpeg version. Unrecorded external evidence is excluded from this lane;
provider-backed or real-library trials need separately labeled results.

Retain tool arguments, both output streams, exit codes, decisions, questions,
answers and checkpoints. The evaluator inspects the database read-only, decision
history, worklogs and actual filesystem links against the answer key. Agent
claims alone cannot establish correct identification, recovery or publication.

Run at least three fresh trials per fixed configuration. Publish every result,
including failures and timeouts, with case outcomes and the median/range of
interventions and cost. This initial sample establishes a regression baseline,
not a statistical claim about arbitrary media collections.

## Scorecard and gates

Report counts and denominators, not one weighted score. Specify the counting unit
in the answer key so duplicate occurrences cannot inflate successful title
counts. A zero denominator is `N/A`.

| Measure | Definition | Initial gate |
| --- | --- | --- |
| Incorrect committed identifications | Cases assigned an identity, edition or episode outside the allowed answer set, even if later corrected | Zero |
| Incorrect confident claims | Wrong identities stated as established in the transcript or accepted decisions, distinct from tentative candidates | Zero |
| Resolvable coverage | Correctly resolved cases / cases resolvable with permitted evidence and the fixed human-answer policy | All initial fixture cases; publish the denominator |
| Appropriate unresolved ambiguity | Genuinely ambiguous cases explicitly deferred with alternatives or missing evidence / genuinely ambiguous cases | All; none published as established |
| Unnecessary deferrals | Resolvable cases left unresolved at the deadline | Zero for the initial fixture |
| Human interventions | Every operator reply or corrective action after the initial task, including repeated questions | Report total and per case; zero corrective rescues on the clear path |
| Recovery | Injected interruptions recovered to independently verified expected state / injected interruptions | All; no source damage or duplicate completed render |
| Repeat stability | Unexpected changes to identities, curation, membership or link targets across unchanged cycles | Zero |
| Publication correctness | Expected links present, unexpected links absent, actual targets verified | All expected membership; no ambiguous or damaged media admitted |
| Source preservation | Before/after hashes and occurrence checks, excluding declared harness-controlled changes | No agent or operator source mutation |

Count wrong accepted identities even when labeled low-confidence: acceptance or
publication is a consequential commitment. Inspect the transcript for confident
claims that never reached the database. Have a human adjudicate uncertain claim
classifications rather than inventing certainty.

Separate clarification requests, corrective interventions, recovery assistance
and policy approvals. Record operator time when measured, otherwise `not
measured`. The fixed human-answer packet resolves selected cases; others remain
ambiguous by design. Repeated requests are not free. Set the total intervention
budget after the reference journey exposes necessary questions, before the first
scored agent trial; do not adjust it after seeing agent results.

Normalize volatile timestamps, run IDs and attempt records for repeat comparison.
Compare semantic identities, accepted metadata, associations, membership, link
text/targets and completed rendition counts. Also report unnecessary filesystem
operations: equal final state can hide unlink/relink churn. Maintenance exit zero
does not mean curation is complete; zero unresolved cases is not inherently good.

## Deliverables and completion order

1. Build and review the corpus, private answer key and human-answer packet.
   Freeze case IDs, allowed decisions and catalog policies.
2. Produce the runnable reference journey and concise walkthrough. Retain the
   database, source hashes, decision trail, output tree and interruption logs.
3. Implement an independent evaluator and machine-readable scorecard. Prove it
   rejects wrong accepted identities, hidden ambiguity, omitted cases, unexpected
   links, fake recovery claims and repeat churn. Correct abstention must pass;
   deferring every case must fail coverage.
4. Run agent trials. Classify failures as judgment, unclear evidence, CLI friction,
   recovery failure or evaluator defects. Fix the smallest relevant existing path
   and repeat affected cases plus the complete journey.
5. Close the milestone only after every hard gate and the fixed intervention
   budget pass, with a walkthrough reproducible from a fresh installation.

Each retained run needs corpus/package identifiers, transcript, checkpoints,
scorecard, evaluator version and explicit limitations. Publish aggregate and
case-level outcomes while keeping answers inaccessible during measured trials.
A deterministic harness, test count or agent exit code alone cannot close this
milestone. No agent quality result has yet been established.

The existing [installed CLI acceptance](RELEASE_TESTING.md) remains the mechanical
baseline. [The operator workflow](WORKFLOW.md), [agent automation](AUTOMATION.md)
and [automatic catalog updates](CATALOG_REFRESH.md) document current commands.
