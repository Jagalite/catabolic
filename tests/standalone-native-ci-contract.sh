#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
# Shell-only wiring tests. Fake cargo/rustc/preparer never build, fetch or execute Rust.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd -P)
driver="$repo/scripts/standalone-native-ci.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/catabolic-ci-contract.XXXXXX")
# The runner resolves paths physically (including macOS /var -> /private/var).
work=$(cd "$work" && pwd -P)
# Every run is left inspectable in this owned scratch directory; no data is deleted.
printf 'Contract scratch: %s\n' "$work"
# Conservative literal workflow contract: any YAML/control-flow change requires
# deliberate re-review of this digest. This is not a general YAML interpreter.
workflow_matches() {
    [ "$(shasum -a 256 "$1" | cut -d ' ' -f 1)" = 75f6c1552c067f44de4a8aa25b9368291c44ea97a0d16f648c108f0420c71b44 ]
}
workflow="$repo/.github/workflows/standalone-native.yml"
workflow_matches "$workflow"
# These witnesses keep the contract meaningful when workflow wiring is changed.
for mutation in skip-runner suppress-failure wrapper-dependency unpin-action drop-evidence drop-target-directory; do
    case "$mutation" in
        skip-runner) sed 's/run: bash scripts\/standalone-native-ci.sh/run: echo scripts\/standalone-native-ci.sh/' "$workflow";;
        suppress-failure) sed 's/    timeout-minutes: 90/    continue-on-error: true/' "$workflow";;
        wrapper-dependency) sed 's/    timeout-minutes: 90/    needs: installed/' "$workflow";;
        unpin-action) sed 's/actions\/checkout@d23441a48e516b6c34aea4fa41551a30e30af803/actions\/checkout@main/' "$workflow";;
        drop-evidence) sed 's/if: always()/if: success()/' "$workflow";;
        drop-target-directory) sed '/CARGO_TARGET_DIR=/d' "$workflow";;
    esac > "$work/workflow-$mutation.yml"
    if workflow_matches "$work/workflow-$mutation.yml"; then echo "workflow mutation accepted: $mutation" >&2; exit 1; fi
    printf 'passed workflow contract: %s\n' "$mutation"
done
mkdir "$work/bin" "$work/source" "$work/target" "$work/target/debug"
printf 'channel = "1.99.0"\n' > "$work/source/rust-toolchain.toml"
export FIXTURE_ROOT="$work"
export FIXTURE_LOG="$work/calls.log"
export PATH="$work/bin:$PATH"
export CARGO_TARGET_DIR="$work/target"
export FIXTURE_MODE=pass
cat > "$work/bin/git" <<'STUB'
#!/usr/bin/env bash
set -eu
printf 'git %s\n' "$*" >> "$FIXTURE_LOG"
case "$*" in
    *'rev-parse HEAD') printf '%040d\n' 1;;
    *'diff --exit-code HEAD -- .') [ "$FIXTURE_MODE" != dirty-source ];;
    *'ls-files --others --exclude-standard') [ "$FIXTURE_MODE" != git-list-failure ];;
    *) exit 81;;
esac
STUB
cat > "$work/bin/rustup" <<'STUB'
#!/usr/bin/env bash
[ "$FIXTURE_MODE" != missing-toolchain ] || exit 1
case "$*" in
    'which --toolchain 1.99.0 rustc'|'which --toolchain 1.99.0 cargo') printf '%s/bin/%s\n' "$FIXTURE_ROOT" "$4";;
    *) exit 85;;
esac
STUB
cat > "$work/bin/rustc" <<'STUB'
#!/usr/bin/env bash
if [ "$FIXTURE_MODE" = wrong-rust ]; then echo 'rustc 1.98.0 (fake)'; else echo 'rustc 1.99.0 (fake)'; fi
STUB
cat > "$work/bin/cargo" <<'STUB'
#!/usr/bin/env bash
set -eu
printf 'cargo %s\n' "$*" >> "$FIXTURE_LOG"
notice=supplemental::tests::stage_five_notices_from_existing_offline_cache
providers=(
    both_native_executables_dispatch_bounded_provider_protocol
    cancellation_kills_stalled_http_and_prevents_later_fanout
    cancellation_kills_stalled_smtp_banner
    outer_deadline_reaps_native_http_worker_and_all_its_resources
    invalid_input_and_budget_never_disclose_credentials
    standalone_and_companion_drains_keep_durable_partial_receipts_without_python
    configured_companion_failures_never_fall_back_to_the_standalone_worker
    replaced_standalone_fallback_is_rejected_before_a_second_delivery
)
case "$*" in
    --version) echo 'cargo 1.99.0 (fake)';;
    'fmt --check '*) :;;
    'clippy --offline --locked --jobs 1 '*) [ "$FIXTURE_MODE" != clippy-failure ];;
    'build --offline --locked --jobs 1 -p catabolic-dist')
        [ "$FIXTURE_MODE" != missing-preparer ] || chmod -x "$CARGO_TARGET_DIR/debug/catabolic-dist";;
    'test --offline --locked --jobs 1 -p catabolic-dist -- --test-threads=1')
        if [ "$FIXTURE_MODE" = skipped-dist ]; then echo 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 21 filtered out;'
        else echo 'test result: ok. 20 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out;'; fi;;
    'test --offline --locked --jobs 1 -p catabolic-dist -- --list')
        [ "$FIXTURE_MODE" = missing-notice ] || echo "$notice: test";;
    "test --offline --locked --jobs 1 -p catabolic-dist $notice -- --ignored --exact --test-threads=1")
        if [ "$FIXTURE_MODE" = notice-failure ]; then exit 87; fi
        if [ "$FIXTURE_MODE" = skipped-notice ]; then
            echo 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 20 filtered out;'
        else
            echo "test $notice ... ok"
            echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 20 filtered out;'
        fi;;
    'test --offline --locked --jobs 1 -p catabolic-cli --test native_notification_providers -- --list')
        for name in "${providers[@]}"; do
            if [ "$FIXTURE_MODE" != missing-provider ] || [ "$name" != "${providers[0]}" ]; then echo "$name: test"; fi
        done;;
    'test --offline --locked --jobs 1 -p catabolic-cli --test native_notification_providers '*)
        # Index 10 is the exact test filter in this fixed command form.
        name=${10}
        [ "${CATABOLIC_NOTIFICATION_TEST_NATIVE_BIN:-}" = "$FIXTURE_RUN/candidate/bin/catabolic-native" ]
        [ "${CATABOLIC_NOTIFICATION_TEST_LEGACY_BIN:-}" = "$FIXTURE_RUN/candidate/bin/catabolic" ]
        case " ${providers[*]} " in *" $name "*) ;; *) exit 82;; esac
        if [ "$FIXTURE_MODE" = provider-failure ]; then exit 88; fi
        if [ "$FIXTURE_MODE" = skipped-provider ]; then
            echo 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 8 filtered out;'
        else
            echo "test $name ... ok"
            echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out;'
        fi;;
    *) echo "unexpected Cargo operation: $*" >&2; exit 83;;
esac
STUB
cat > "$work/target/debug/catabolic-dist" <<'STUB'
#!/usr/bin/env bash
set -eu
printf 'dist %s\n' "$*" >> "$FIXTURE_LOG"
case "$1" in
    prepare)
        [ "$FIXTURE_MODE" != prepare-failure ] || exit 89
        mkdir "$3" "$3/bin"
        printf '#!/bin/sh\nexit 0\n' > "$3/bin/catabolic"
        cp "$3/bin/catabolic" "$3/bin/catabolic-native"
        chmod 555 "$3/bin/"*
        printf '{"fixture":"not-a-real-candidate"}\n' > "$3/candidate.json"
        anchor=$(shasum -a 256 "$3/candidate.json" | cut -d ' ' -f 1)
        [ "$FIXTURE_MODE" != missing-manifest ] || mv "$3/candidate.json" "$3/missing-manifest"
        if [ "$FIXTURE_MODE" = wrong-anchor ]; then anchor=$(printf '%064d' 0); fi
        printf '{"manifest_sha256":"%s"}\n' "$anchor"
        chmod a-w "$3/candidate.json" 2>/dev/null || :;;
    verify)
        [ "$FIXTURE_MODE" != verify-failure ] || exit 90
        count=0
        if [ -f "$FIXTURE_RUN/verify-count" ]; then count=$(cat "$FIXTURE_RUN/verify-count"); fi
        count=$((count + 1))
        printf '%s\n' "$count" > "$FIXTURE_RUN/verify-count"
        if [ "$FIXTURE_MODE" = missing-final-archive ] && [ "$count" -eq 10 ]; then
            mv "$FIXTURE_RUN/candidate.tar" "$FIXTURE_RUN/withheld-candidate.tar"
            mv "$FIXTURE_RUN/evidence/archive.sha256" "$FIXTURE_RUN/withheld-archive.sha256"
        fi
        [ "$#" -eq 4 ] && [ "$4" = "$FIXTURE_ROOT/source" ]
        [ "$(shasum -a 256 "$2/candidate.json" | cut -d ' ' -f 1)" = "$3" ];;
    smoke)
        [ "$FIXTURE_MODE" != smoke-failure ] || exit 91
        [ ! -e "$4" ]
        if [ "$FIXTURE_MODE" != missing-smoke ]; then
            accepted=false
            if [ "$FIXTURE_MODE" = invalid-smoke ]; then accepted=true; fi
            jq -n --arg anchor "$3" --argjson accepted "$accepted" \
                '{status:"passed-bounded-safe-smoke-only",candidate_manifest_sha256:$anchor,python_path_denied:true,python_attempts:0,python_free_runtime_accepted:$accepted,cases:[1,2,3,4,5,6,7,8]}' > "$4"
        fi;;
    *) exit 84;;
esac
STUB
chmod +x "$work/bin/"* "$work/target/debug/catabolic-dist"
run_case() {
    export FIXTURE_MODE=$1
    export FIXTURE_RUN="$work/run-$1"
    chmod +x "$work/target/debug/catabolic-dist"
    if bash "$driver" "$work/source" "$FIXTURE_RUN" > "$work/$1.stdout" 2> "$work/$1.stderr"; then
        [ "$2" = pass ] || { echo "unexpected success: $1" >&2; exit 1; }
    else
        [ "$2" = fail ] || { cat "$work/$1.stderr" >&2; echo "unexpected failure: $1" >&2; exit 1; }
    fi
    jq -e --arg expected "$2" \
        '(.status == (if $expected == "pass" then "passed" else "failed" end)) and .python_free_runtime_accepted == false and .release_qualified == false and .m4_m11_qualified == false' \
        "$FIXTURE_RUN/evidence/result.json" > /dev/null
    printf 'passed contract case: %s\n' "$1"
}
run_case pass pass
[ -s "$FIXTURE_RUN/candidate.tar" ]
[ "$(grep -c 'cargo test .*native_notification_providers .* -- --exact --test-threads=1' "$FIXTURE_LOG")" -eq 8 ]
# A repeat must refuse the existing directory without changing its old receipt.
before=$(shasum -a 256 "$FIXTURE_RUN/evidence/result.json")
if bash "$driver" "$work/source" "$FIXTURE_RUN" > "$work/stale.stdout" 2> "$work/stale.stderr"; then exit 1; fi
[ "$(shasum -a 256 "$FIXTURE_RUN/evidence/result.json")" = "$before" ]
printf '%s\n' 'passed contract case: stale-run-refused'
for failure in missing-toolchain wrong-rust dirty-source git-list-failure clippy-failure skipped-dist missing-notice skipped-notice notice-failure missing-preparer prepare-failure missing-manifest wrong-anchor verify-failure smoke-failure missing-smoke invalid-smoke missing-provider skipped-provider provider-failure missing-final-archive; do
    run_case "$failure" fail
done
# A later failure must preserve the checked artifact and external anchors.
[ -s "$work/run-provider-failure/candidate.tar" ]
[ -s "$work/run-provider-failure/evidence/manifest.sha256" ]
[ -s "$work/run-provider-failure/evidence/archive.sha256" ]
# No network operation, Python build target, wheel or package installer was invoked.
if grep -Ei 'fetch|install|python|pyo3|maturin|pip|wheel' "$FIXTURE_LOG" | grep -v 'without_python'; then exit 1; fi

# A late failure cannot retain a passed receipt, even after the success flag is set.
mkdir "$work/late-failure"
set +e
(
    set -e
    result=passed
    stage=complete
    source_head=fixture
    anchor=''
    evidence="$work/late-failure"
    eval "$(sed -n '/^finish() {$/,/^}$/p' "$driver")"
    trap finish EXIT
    exit 92
)
late_code=$?
set -e
[ "$late_code" -eq 92 ]
jq -e '.status == "failed" and .exit_code == 92' "$work/late-failure/result.json" > /dev/null
printf '%s\n' 'passed contract case: late-nonzero-status'
printf '%s\n' 'All shell wiring contracts passed. These are fake-command checks, not native qualification.'
