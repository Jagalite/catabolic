#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
# Bounded offline CI runner. Requires Bash, Git, jq, tar, shasum and pinned Rust.
set -euo pipefail
export LC_ALL=C
export RUSTUP_TOOLCHAIN=1.99.0

if [ "$#" -ne 2 ]; then
    echo 'usage: standalone-native-ci.sh CHECKOUT NEW_RUN_DIRECTORY' >&2
    exit 2
fi
root=$(cd "$1" && pwd -P)
case "$2" in /*) ;; *) echo 'run directory must be absolute' >&2; exit 2;; esac
case "$2/" in "$root/"*) echo 'run directory must be outside checkout' >&2; exit 2;; esac
# No reuse, deletion or overwrite of an earlier run (including stale successes).
mkdir "$2"
run=$(cd "$2" && pwd -P)
case "$run/" in "$root/"*) echo 'resolved run directory is inside checkout' >&2; exit 2;; esac
mkdir "$run/evidence"
evidence="$run/evidence"
candidate="$run/candidate"
result=failed
stage=preflight
source_head=''
anchor=''
finish() {
    code=$?
    trap - EXIT
    if [ "$code" -eq 0 ] && [ "$result" != passed ]; then code=1; fi
    if [ "$code" -ne 0 ]; then result=failed; fi
    # Even failed checks retain logs and a non-passing terminal receipt.
    jq -n --arg status "$result" --arg stage "$stage" --arg source "$source_head" \
        --arg manifest "$anchor" --argjson exit_code "$code" \
        '{format:1,status:$status,last_stage:$stage,exit_code:$exit_code,source_git_head:$source,candidate_manifest_sha256:$manifest,python_free_runtime_accepted:false,release_qualified:false,m4_m11_qualified:false,scope:"Bounded native packaging, eight safe smoke cases and eight staged loopback notification tests only"}' \
        > "$evidence/result.json"
    exit "$code"
}
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
# Fixtures must inherit a physical parent: macOS /var aliases otherwise fail
# intentional no-follow source/output admission before the behavior under test.
mkdir "$run/tmp"
export TMPDIR="$run/tmp"
printf '%s\n' "$TMPDIR" > "$evidence/tmpdir.txt"
step() {
    stage=$1
    shift
    printf '%s\tstarted\n' "$stage" >> "$evidence/steps.tsv"
    if "$@" > "$evidence/$stage.stdout" 2> "$evidence/$stage.stderr"; then
        printf '%s\tpassed\n' "$stage" >> "$evidence/steps.tsv"
    else
        code=$?
        printf '%s\tfailed\t%s\n' "$stage" "$code" >> "$evidence/steps.tsv"
        cat "$evidence/$stage.stderr" >&2
        return "$code"
    fi
}
require_file() { [ -s "$1" ] && [ -f "$1" ] && [ ! -L "$1" ]; }
hash() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
clean_source() {
    local untracked
    [ "$(git -C "$root" rev-parse HEAD)" = "$source_head" ] || return 1
    git -C "$root" diff --exit-code HEAD -- . || return 1
    untracked=$(git -C "$root" ls-files --others --exclude-standard) || return 1
    [ -z "$untracked" ]
}
verify_archive() {
    local actual expected
    require_file "$run/candidate.tar" || return 1
    require_file "$evidence/archive.sha256" || return 1
    actual=$(hash "$run/candidate.tar") || return 1
    expected=$(cat "$evidence/archive.sha256") || return 1
    [ "$actual" = "$expected" ]
}
one_passed() {
    # libtest may return success after filtering out every test: require actual execution.
    [ "$(grep -Fxc "test $2 ... ok" "$1" || :)" -eq 1 ] &&
        [ "$(grep -Ec '^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out;' "$1" || :)" -eq 1 ]
}
preflight() {
    case "${CARGO_TARGET_DIR:-}" in /*) ;; *) echo 'CARGO_TARGET_DIR must be absolute' >&2; return 1;; esac
    command -v jq && command -v shasum && command -v tar || return 1
    # The repository pin and actual compiler must both agree; never auto-install here.
    grep -Fx 'channel = "1.99.0"' "$root/rust-toolchain.toml" || return 1
    # Explicit installed-toolchain lookup cannot bootstrap a missing toolchain.
    rustup which --toolchain 1.99.0 rustc || return 1
    rustup which --toolchain 1.99.0 cargo || return 1
    rustc -vV | grep '^rustc 1\.99\.0 ' || return 1
    cargo --version | grep '^cargo 1\.99\.0 ' || return 1
    clean_source
}
cd "$root"
source_head=$(git rev-parse HEAD)
step preflight preflight
printf '%s\n' "$source_head" > "$evidence/source-git-head.txt"
# Explicit packages keep PyO3, Python runtime and wheel build prerequisites out.
packages=(-p catabolic-core -p catabolic-store -p catabolic-runtime -p catabolic-http -p catabolic-cli -p catabolic-verification -p catabolic-dist)
step format cargo fmt --check "${packages[@]}"
step clippy cargo clippy --offline --locked --jobs 1 "${packages[@]}" --all-targets -- -D warnings
step dist-tests cargo test --offline --locked --jobs 1 -p catabolic-dist -- --test-threads=1
step dist-tests-executed grep -E '^test result: ok\. [1-9][0-9]* passed; 0 failed; 1 ignored; 0 measured; 0 filtered out;' "$evidence/dist-tests.stdout"
# Discovery plus an exact executed test is mandatory, not an optional ignored test.
notice=supplemental::tests::stage_five_notices_from_existing_offline_cache
step notice-discovery cargo test --offline --locked --jobs 1 -p catabolic-dist -- --list
step notice-present grep -Fx "$notice: test" "$evidence/notice-discovery.stdout"
step five-notices cargo test --offline --locked --jobs 1 -p catabolic-dist "$notice" -- --ignored --exact --test-threads=1
step five-notices-executed one_passed "$evidence/five-notices.stdout" "$notice"
step dist-build cargo build --offline --locked --jobs 1 -p catabolic-dist
preparer="$CARGO_TARGET_DIR/debug/catabolic-dist"
step preparer-present test -x "$preparer"
step prepare "$preparer" prepare "$root" "$candidate"
step manifest-present require_file "$candidate/candidate.json"
stage=read-external-anchor
anchor=$(jq -er '.manifest_sha256 | select(type == "string" and test("^[0-9a-f]{64}$"))' "$evidence/prepare.stdout")
printf '%s\n' "$anchor" > "$evidence/manifest.sha256"
step anchor-matches test "$(hash "$candidate/candidate.json")" = "$anchor"
step verify-source "$preparer" verify "$candidate" "$anchor" "$root"
# upload-artifact resets filesystem modes; a tar preserves the read-only payload
# and executable bits. The external manifest anchor remains separate from it.
step archive tar -cf "$run/candidate.tar" -C "$run" candidate
step archive-present require_file "$run/candidate.tar"
hash "$run/candidate.tar" > "$evidence/archive.sha256"
chmod a-w "$run/candidate.tar" "$evidence/manifest.sha256" "$evidence/archive.sha256"
# Preserve this verified payload even if a later fixture fails.
step safe-smoke "$preparer" smoke "$candidate" "$anchor" "$evidence/safe-smoke.json"
step smoke-present require_file "$evidence/safe-smoke.json"
step smoke-receipt jq -e --arg anchor "$anchor" \
    '.status == "passed-bounded-safe-smoke-only" and .candidate_manifest_sha256 == $anchor and .python_path_denied == true and .python_attempts == 0 and .python_free_runtime_accepted == false and (.cases | length) == 8' \
    "$evidence/safe-smoke.json"
# Reuse actual existing Rust fixtures. The overrides pin both immutable staged binaries.
export CATABOLIC_NOTIFICATION_TEST_NATIVE_BIN="$candidate/bin/catabolic-native"
export CATABOLIC_NOTIFICATION_TEST_LEGACY_BIN="$candidate/bin/catabolic"
provider_tests=(
    both_native_executables_dispatch_bounded_provider_protocol
    cancellation_kills_stalled_http_and_prevents_later_fanout
    cancellation_kills_stalled_smtp_banner
    outer_deadline_reaps_native_http_worker_and_all_its_resources
    invalid_input_and_budget_never_disclose_credentials
    standalone_and_companion_drains_keep_durable_partial_receipts_without_python
    configured_companion_failures_never_fall_back_to_the_standalone_worker
    replaced_standalone_fallback_is_rejected_before_a_second_delivery
)
step provider-discovery cargo test --offline --locked --jobs 1 -p catabolic-cli --test native_notification_providers -- --list
for name in "${provider_tests[@]}"; do
    step "present-$name" grep -Fx "$name: test" "$evidence/provider-discovery.stdout"
    step "$name" cargo test --offline --locked --jobs 1 -p catabolic-cli --test native_notification_providers "$name" -- --exact --test-threads=1
    step "executed-$name" one_passed "$evidence/$name.stdout" "$name"
    step "integrity-$name" "$preparer" verify "$candidate" "$anchor" "$root"
done
step final-source clean_source
step final-integrity "$preparer" verify "$candidate" "$anchor" "$root"
step final-archive verify_archive
result=passed
stage=complete
printf '%s\n' 'Bounded standalone candidate checks passed; all qualification flags remain false.'
