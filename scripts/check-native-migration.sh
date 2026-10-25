#!/bin/sh
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
# Requires the retained sanitized M0 historical fixtures; never uses live catalogs.
set -eu
cd "$(dirname "$0")/.."
qualification_python="${CATABOLIC_QUALIFICATION_PYTHON:-.venv/bin/python}"
cargo build --locked --release -p catabolic-cli -p catabolic-verification
target_dir=$(cargo metadata --locked --format-version=1 --no-deps | "$qualification_python" -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
export CATABOLIC_NATIVE_BINARY="$target_dir/release/catabolic-native"
export CATABOLIC_HOST_TEST_HELPER="$CATABOLIC_NATIVE_BINARY"
"$qualification_python" scripts/verify_native_test_helper.py "$CATABOLIC_HOST_TEST_HELPER"
"$qualification_python" -m unittest tests.test_rust_migration tests.test_rust_populated_migration tests.test_rust_queries tests.test_rust_graphql tests.test_rust_selection tests.test_rust_layouts tests.test_rust_components tests.test_rust_tags tests.test_rust_existing_reads
cargo test --locked -p catabolic-core -p catabolic-store -- --test-threads=1
cargo clippy --locked --workspace --all-targets -- -D warnings
"$target_dir/release/catabolic-verification" check
