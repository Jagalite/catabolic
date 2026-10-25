#!/bin/sh
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
# Local-only qualification. Never opens a user catalog or contacts live adapters.
set -eu
# Keep the official helper within its unchanged 128 MiB packaging bound.
# An explicit caller override is retained and still checked by preflight.
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
cd "$(dirname "$0")/.."
python="${CATABOLIC_QUALIFICATION_PYTHON:-.venv/bin/python}"
# This pinned Python must also contain the exact frozen Catabolic package: the
# identify/refresh oracle launches its HTTP worker with PYTHONPATH cleared.
# The qualifier verifies it; this caller never installs or substitutes a runtime.
case "$python" in
    /*) export CATABOLIC_PYTHON_ORACLE="$python" ;;
    *) export CATABOLIC_PYTHON_ORACLE="$(pwd)/$python" ;;
esac
export CATABOLIC_PYTHON_REFERENCE="${CATABOLIC_PYTHON_REFERENCE:-$(pwd)/.local-tests/rust-migration/reference/src}"
test -d "$CATABOLIC_PYTHON_REFERENCE/catabolic" || { echo "Pinned Python reference source is missing" >&2; exit 1; }
reference_root=$(cd "$CATABOLIC_PYTHON_REFERENCE/.." && pwd)
export CATABOLIC_PYTHON_REFERENCE="$reference_root/src"
for tool in ffmpeg ffprobe pdftotext tesseract; do
    command -v "$tool" >/dev/null || { echo "Required acceptance tool is missing: $tool" >&2; exit 1; }
done
fixtures=.local-tests/rust-migration/m0-completion/legacy-fixtures
if [ ! -f "$fixtures/schema-32/before.sqlite3" ]; then
    echo "Historical fixtures are missing. Generate them with scripts/migration_fixtures.py from the pinned reference before qualification." >&2
    exit 1
fi
# Reproduce interpreter-dependent tables without mutating qualified source bytes.
"$python" scripts/generate_graphql_printable.py --check
PYTHONPATH="$CATABOLIC_PYTHON_REFERENCE" "$python" scripts/generate_plex_xml_encodings.py --check
# Cargo may use an explicit/shared target directory; bind the artifacts Cargo built.
target_dir=$(cargo metadata --locked --format-version=1 --no-deps | "$python" -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build --locked -p catabolic-cli -p catabolic-verification -p catabolic-http --bins
cargo build --locked -p catabolic-cli --example legacy_parse
cargo build --locked -p catabolic-python
evidence=.local-tests/rust-migration/m4-plus
mkdir -p "$evidence/artifacts"
cp "$target_dir"/debug/catabolic-native "$evidence/artifacts/qualification-native"
cp "$target_dir"/debug/catabolic-http-experimental "$evidence/artifacts/qualification-http"
cp "$target_dir"/debug/catabolic-legacy-native "$evidence/artifacts/qualification-legacy"
cp "$target_dir"/debug/examples/legacy_parse "$evidence/artifacts/qualification-parser"
case "$(uname -s)" in
    Darwin) extension="$target_dir/debug/lib_native.dylib" ;;
    *) extension="$target_dir/debug/lib_native.so" ;;
esac
cp "$extension" "$evidence/artifacts/qualification-extension.so"
export CATABOLIC_NATIVE_EXTENSION="$(pwd)/$evidence/artifacts/qualification-extension.so"
export CATABOLIC_NATIVE_BINARY="$(pwd)/$evidence/artifacts/qualification-native"
export CATABOLIC_HTTP_BINARY="$(pwd)/$evidence/artifacts/qualification-http"
export CATABOLIC_LEGACY_BINARY="$(pwd)/$evidence/artifacts/qualification-legacy"
export CATABOLIC_NATIVE_PARSER_BINARY="$(pwd)/$evidence/artifacts/qualification-parser"
# The four newer native oracle suites have assertions for missing
# prerequisites; they belong here rather than in the Python reference lane.
export CATABOLIC_NATIVE_CLI="$CATABOLIC_NATIVE_BINARY"
"$python" -m unittest \
    tests.test_native_mapping_runtime_v2 \
    tests.test_native_projection_execute \
    tests.test_native_publication_mapping_folder \
    tests.test_native_upgrade_rollback
# Mandatory private NFO provenance fixtures need actual closed-child ownership.
# This exact official artifact was built and copied above, never a test harness.
export CATABOLIC_HOST_TEST_HELPER="$CATABOLIC_NATIVE_BINARY"
"$python" scripts/verify_native_test_helper.py "$CATABOLIC_HOST_TEST_HELPER" > "$evidence/native-test-helper.json"
cargo test --workspace --locked -- --test-threads=1
# Cargo accepts an empty test filter. Require the exact ignored fixture to exist
# once, and require one executed success rather than trusting exit status alone.
required_ignored_test() {
    scenario=$1
    shift
    listing=$(cargo test --locked "$@" "$scenario" -- --ignored --exact --list)
    printf '%s\n' "$listing" | "$python" -c '
import sys
actual = [line.strip() for line in sys.stdin if line.rstrip().endswith(": test")]
expected = [sys.argv[1] + ": test"]
if actual != expected:
    raise SystemExit(f"required ignored fixture identity mismatch: {actual!r} != {expected!r}")
' "$scenario"
    output=$(cargo test --locked "$@" "$scenario" -- --ignored --exact)
    printf '%s\n' "$output"
    printf '%s\n' "$output" | "$python" -c '
import re, sys
summaries = re.findall(r"^test result: .*?$", sys.stdin.read(), re.MULTILINE)
if len(summaries) != 1 or not summaries[0].startswith("test result: ok. 1 passed; 0 failed; 0 ignored;"):
    raise SystemExit(f"required ignored fixture did not execute exactly once: {summaries!r}")
'
}
if [ "$(uname -s)" = Linux ]; then
    required_ignored_test entry_budget_exhaustion_cannot_restart_for_later_sources -p catabolic-runtime --test source_events_independent
fi
# The ordinary document cases already ran in the workspace gate above.
required_ignored_test document::tests::real_pdf_text_and_page_overflow -p catabolic-runtime --lib
required_ignored_test document::tests::real_single_image_ocr -p catabolic-runtime --lib
required_ignored_test artifact_mutation::tests::python_native_publication_intents_recover_in_both_directions -p catabolic-store --lib
# Local administration's frozen transitions and the explicitly divergent
# scalar-token refusal witness are mandatory, not silently ignored evidence.
required_ignored_test access_admin::tests::frozen_access_owner_differential -p catabolic-store --lib
required_ignored_test access_admin::tests::inherited_scalar_cache_confusion_remains_fail_closed_native -p catabolic-store --lib
# The two exact ASGI test files require an installed release wheel. Verify their
# fixed 21-scenario inventory before import, and record this mandatory separate
# prerequisite as pending rather than treating it as skipped or passed.
"$python" scripts/qualify_native_mutations.py --defer-installed-asgi --binary "$CATABOLIC_NATIVE_BINARY" --http-binary "$CATABOLIC_HTTP_BINARY" --legacy-binary "$CATABOLIC_LEGACY_BINARY" --parser-binary "$CATABOLIC_NATIVE_PARSER_BINARY" --extension-binary "$CATABOLIC_NATIVE_EXTENSION" --reference-root "$reference_root" --python-oracle "$CATABOLIC_PYTHON_ORACLE" --report "$evidence/native-results.json"
"$target_dir"/debug/catabolic-verification check
"$target_dir"/debug/catabolic-verification check-playback
cargo build --locked --release -p catabolic-cli
cp "$target_dir"/release/catabolic-native "$evidence/artifacts/qualification-native-release"
"$python" scripts/check_native_output_gate.py "$(pwd)/$evidence/artifacts/qualification-native-release" > "$evidence/release-output-gate.json"
"$python" scripts/sync_docs.py --check
"$python" -m catabolic spec check
# Separate gates: installed wheel/platform, real mounts/consumers, full HTTP
# contract/client coverage and release evidence. This script does not close them.
