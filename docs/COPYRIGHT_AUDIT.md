# Copyright and provenance audit

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Audit date: 2026-09-06. Main HEAD: `2d48eaef8de9b23dc07ab0ef534f43c338019c8f`.
Scope includes the working tree and its existing uncommitted safety fixes.

Follow-up, 2026-09-06: the user authorized licensing the project as MIT under
`2026 The Catabolic Contributors`. The project now includes `LICENSE`, SPDX
notices, and package license metadata. JSON and checksum-sensitive migrations
and released artifacts use adjacent `.license` files to preserve their bytes.
The missing-license finding below describes the original audited snapshot;
TMDB attribution remains a separate open item.

## Assessment

No confirmed copying from the compared upstream sources, or bundled third-party
media assets, was identified within this audit's scope. This is a bounded
provenance assessment, not proof of independent authorship or legal clearance.
Copyrighted dependencies are present as separately installed packages with their
own licenses; their presence is expected and does not itself indicate misuse.

Two release items remain: Catabolic has no project license file or package license
metadata, and its TMDB attribution implementation needs further review against
the provider's requirements.

## Scope and evidence

- Inspected 132 current tracked/unignored files before adding this report.
- Inspected 57 commits reachable through local refs: five main commits and 52
  development checkpoints. Enumerated 132 distinct historical paths and 270
  unique Git blobs, totaling 3,735,326 bytes.
- Checked reflogs and ran `git fsck --full --unreachable --no-reflogs`; no
  unreachable objects or object-integrity errors were reported. No Git remote
  was configured; unavailable external history was not inspected.
- Scanned working and historical content for copyright, SPDX, license grants,
  and copied/adapted/ported-source markers. No matching provenance headers were
  found. Absence of a header does not establish ownership or absence of copying.
- Examined the existing release wheel's 77 entries: 72 under `catabolic/` and
  five distribution metadata files. No vendored dependency packages, FFmpeg,
  ffprobe, native executables, or binary media assets were present.

The wheel examined was
`.local-tests/audit/wheels/catabolic-0.1.0-py3-none-any.whl`, SHA-256:
`b60045492afd210ca329f2bda037b29f6782633d7f79ea67e06ad8dd6988fd02`.
This assessment does not certify other or future distribution artifacts.

## Source comparisons

The nine upstream implementation files cited in `ROADMAP.md` were downloaded at
their recorded revisions, alongside the corresponding repository licenses.
Those files and local prototype reference files were compared against current and
historical Catabolic text, including packaged documentation.

| Reference | Revision / location | Files compared | Observed license |
| --- | --- | --- | --- |
| FileBot scripts | `859d2ad985e12e1f1514aa9f8c8f46be3008c439` | `amc.groovy`, `duplicates.groovy`, `miss.groovy` | [GPL version 3 license text](https://raw.githubusercontent.com/filebot/scripts/859d2ad985e12e1f1514aa9f8c8f46be3008c439/LICENSE) |
| beets | `ba4787f5744d161a3ed5324f3fcbcd93f02d2698` | `beets/autotag/match.py`, `beets/importer/state.py`, `beetsplug/missing.py` | [MIT](https://raw.githubusercontent.com/beetbox/beets/ba4787f5744d161a3ed5324f3fcbcd93f02d2698/LICENSE) |
| Library | `ed9005211d53757d43a39542f60c4be9d64c2938` | `library/createdb/av.py`, `library/createdb/fs_add_metadata.py`, `library/mediafiles/media_check.py` | [BSD 3-Clause](https://raw.githubusercontent.com/chapmanjacobd/library/ed9005211d53757d43a39542f60c4be9d64c2938/LICENSE) |
| Local prototype | Local checkout and installed skill; detailed paths retained in the local audit artifacts | Python source/tests and skill references | Project `LICENSE` contains Apache 2.0; no Git history available in this directory |

The comparison covered **267 unique Catabolic text versions and 24 reference
files**. It found **zero matches** using these thresholds:

- Five consecutive nonblank lines, stripped of surrounding whitespace, containing
  at least 150 characters in total.
- Fifty consecutive exact Python tokens, excluding comments and formatting,
  containing at least 150 characters in total.

The Python token comparison applies to Python sources; the line comparison also
covers Groovy and documentation. These checks cannot rule out shorter excerpts,
renamed or substantially adapted code, translations between languages, or
copying from sources outside this comparison set. Entire upstream repositories
were not compared.

FileBot's script license deserves particular attention if future work imports
actual script code. A workflow reference alone does not demonstrate copying of
protected expression. Copyright distinguishes expression from ideas, methods,
and systems. See the [U.S. Copyright Office guidance](https://www.copyright.gov/help/faq/faq-protect.html).
Likewise, any actual reuse of Apache-licensed reference code would need to satisfy
its applicable license and notice conditions; labeling Catabolic MIT would not
erase those conditions. See [Apache 2.0, section 4](https://www.apache.org/licenses/LICENSE-2.0).

## Release findings

### 1. Project licensing is not yet declared

There is no root `LICENSE`. `pyproject.toml` declares no project license, and the
examined wheel has no license file or `License`, `License-Expression`, or
`License-File` metadata. An earlier discussion choosing MIT does not put its
grant into the distributed project.

Before publishing as MIT, establish the appropriate rights holder, add the MIT
license and package metadata, and verify their inclusion in both the wheel and
source distribution. All five main commits name the same Git author, but author
metadata does not prove ownership or permission for every contribution.

### 2. TMDB attribution needs a complete distribution approach

`src/catabolic/network_adapters.py` includes TMDB's attribution sentence in the
returned result. No corresponding approved logo or prominent About/Credits
presentation was identified in the inspected CLI and documentation.

TMDB's published guidance requires its approved logo and attribution notice,
and distinguishes free non-commercial API usage from commercial licensing.
Confirm an appropriate attribution presentation for a CLI and its accompanying
documentation before claiming compliance. The existing JSON sentence alone
does not demonstrate that all requirements are satisfied. See the
[TMDB API FAQ](https://developer.themoviedb.org/docs/faq).

This concerns provider terms and supplied data, separately from licensing
Catabolic's own source code. An MIT license would not grant unrestricted rights
to TMDB metadata or images. This audit did not call the TMDB API or retrieve
provider media into the project.

### 3. Runtime dependencies retain their own notices

Installed distribution license files were read and hashed, rather than relying
only on package classifiers:

| Package | Version | Declared license |
| --- | --- | --- |
| graphql-core | 3.2.12 | MIT |
| pydantic | 2.13.5 | MIT |
| pydantic-core | 2.46.5 | MIT |
| annotated-types | 0.8.0 | MIT |
| typing-extensions | 4.16.0 | PSF-2.0; distribution includes Python license history |
| typing-inspection | 0.4.4 | MIT |

These packages are installed separately and were not embedded in the Catabolic
wheel. Their installed distributions contain license files. Preserve these
licenses and notices when distributing those packages, including any future
bundled environment or standalone installer. The table covers runtime Python
dependencies, not a full transitive license audit of development tools,
operating-system packages, or native components inside dependency wheels.

### 4. Test media and external tools

No binary blobs were found in reachable history. Acceptance fixtures are
generated using synthetic FFmpeg video/audio inputs and constructed subtitle
and chapter text; corruption fixtures are generated deliberately. No downloaded
movie, music, artwork, or logo asset was identified in the inspected repository
or wheel. FFmpeg and ffprobe are external optional tools, not bundled files in
the examined artifact.

## Remaining provenance limits

The initial main commit introduced a substantial implementation at once. This
audit cannot reconstruct any unavailable earlier editing history or establish
the provenance of every line. No general internet-wide similarity search or
semantic plagiarism certification was performed. Product names and API names
also raise distinct trademark or contractual questions that this source
comparison does not resolve.

The useful release actions are to declare the intended project license, resolve
TMDB attribution, preserve dependency notices in distributed artifacts, and
record source and license details whenever third-party code or assets are
actually imported. No history rewrite is indicated by the evidence found here.

## Local audit artifacts

Reproducible scripts and detailed results are in the ignored directory
`.local-tests/copyright-audit/`:

- `inventory.py`, `inventory.json`, `corpus.json`, and `fsck.log` record Git,
  working-tree, and wheel inspection.
- `fetch_sources.py`, `upstream.json`, and `upstream/` preserve the exact public
  source revisions and license files used for comparison.
- `compare.py` and `comparison.json` record matching methods and results.
- `dependencies.json` records the installed runtime license texts and hashes.

Public reference sources were downloaded for local comparison; Catabolic source
was not uploaded to a third-party scanning service. Audit downloads remain
ignored and are not release contents. This audit adds this report and local
evidence only; existing implementation changes, source media, licenses, and Git
history were not modified by the audit.
