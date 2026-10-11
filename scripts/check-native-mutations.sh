#!/bin/sh
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
# Local-only qualification. Never opens a user catalog or contacts live adapters.
set -eu
cd "$(dirname "$0")/.."
python="${CATABOLIC_QUALIFICATION_PYTHON:-.venv/bin/python}"
case "$python" in
    /*) export CATABOLIC_PYTHON_ORACLE="$python" ;;
    *) export CATABOLIC_PYTHON_ORACLE="$(pwd)/$python" ;;
esac
export CATABOLIC_PYTHON_REFERENCE="${CATABOLIC_PYTHON_REFERENCE:-$(pwd)/.local-tests/rust-migration/reference/src}"
test -d "$CATABOLIC_PYTHON_REFERENCE/catabolic" || { echo "Pinned Python reference source is missing" >&2; exit 1; }
for tool in ffmpeg ffprobe pdftotext tesseract; do
    command -v "$tool" >/dev/null || { echo "Required acceptance tool is missing: $tool" >&2; exit 1; }
done
fixtures=.local-tests/rust-migration/m0-completion/legacy-fixtures
if [ ! -f "$fixtures/schema-32/before.sqlite3" ]; then
    echo "Historical fixtures are missing. Generate them with scripts/migration_fixtures.py from the pinned reference before qualification." >&2
    exit 1
fi
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build --locked -p catabolic-cli -p catabolic-verification -p catabolic-http --bins
evidence=.local-tests/rust-migration/m4-plus
mkdir -p "$evidence/artifacts"
cp target/debug/catabolic-native "$evidence/artifacts/qualification-native"
cp target/debug/catabolic-http-experimental "$evidence/artifacts/qualification-http"
export CATABOLIC_NATIVE_BINARY="$(pwd)/$evidence/artifacts/qualification-native"
export CATABOLIC_HTTP_BINARY="$(pwd)/$evidence/artifacts/qualification-http"
cargo test --workspace --locked
cargo test --locked -p catabolic-runtime document::tests -- --include-ignored
cargo test --locked -p catabolic-store python_native_publication_intents_recover_in_both_directions -- --include-ignored
"$python" scripts/qualify_native_mutations.py --binary "$CATABOLIC_NATIVE_BINARY" --http-binary "$CATABOLIC_HTTP_BINARY" --report "$evidence/native-results.json"
target/debug/catabolic-verification check
target/debug/catabolic-verification check-playback
cargo build --locked --release -p catabolic-cli
cp target/release/catabolic-native "$evidence/artifacts/qualification-native-release"
"$python" scripts/check_native_output_gate.py "$(pwd)/$evidence/artifacts/qualification-native-release" > "$evidence/release-output-gate.json"
"$python" scripts/sync_docs.py --check
"$python" -m catabolic spec check
# Separate gates: installed wheel/platform, real mounts/consumers, full HTTP
# contract/client coverage and release evidence. This script does not close them.
