# Final independent v1 destination-owner review

Date: 2026-10-12. This is documentation-only provenance for the independent
reviewer's final acceptance **after** the arbitrary-integer removal-budget
change. It does not add test executions or widen the review scope.

- Implementation commit: `c1cd4622048b44b751f36cff8e73069abcc1ea68`
- Reviewer task: `__external_review_destination_mapping_owner`
- Final native debug artifact SHA256:
  `bb4a8952322fce97c3992865178a4469d21804b4ed55732c7467de73413f4867`
- Scope: the new v1 owner, adapters and narrow CLI integration
- Evidence origin: the reviewer's conversation reports and tool outputs. The
  reviewer produced no separate filesystem replay logs or review receipt. The
  results below are **reviewer-reported**, not independently reconstructed logs.

## Final acceptance report

The following is the reviewer's final report, preserved verbatim:

> Narrow re-review accepted; no remaining findings
>
> Final binary: bb4a8952322fce97c3992865178a4469d21804b4ed55732c7467de73413f4867
>
> Independent replay passed:
> - Plex and Jellyfin validation, including 10**100 removal budgets
> - An actual owned-genre removal with that oversized budget, preserving manual values
> - Python 3.11.15 scalar preview/apply/no-op ownership using the dependency-equipped reference interpreter and verified frozen worker origin
>
> Budget saturation preserves bounded-plan comparisons; invalid types remain rejected. git diff --check is clean. Previous review limits and closed gates remain unchanged

## Evidence-storage clarification

After that acceptance, the reviewer was asked for retained evidence paths. This
is the response, preserved verbatim:

> No standalone log or receipt files were created; results are retained in this review conversation’s tool outputs
>
> For artifact bb4a8952322fce97c3992865178a4469d21804b4ed55732c7467de73413f4867:
> - Paired Plex/Jellyfin validation: 2 passed, 23.496s
> - Independent actual-removal oversized-budget probe: 1 passed, 10.892s
> - Dependency-equipped Python 3.11 scalar replay: 1 passed, 18.597s
> - Final git diff --check: passed
>
> Please cite the preceding acceptance report as the independent review receipt; there’s no filesystem path to cite

## Relationship to the paired implementation receipt

The implementation's separate source-bound 36-method runs on Python 3.14 and
Python 3.11, 29 existing regressions and 27 native controls have retained local
receipts/logs whose hashes appear in
[`RUST_MIGRATION_DESTINATION_MAPPINGS_CHECKPOINT.json`](RUST_MIGRATION_DESTINATION_MAPPINGS_CHECKPOINT.json).
Those are distinct from the independent reviewer's four final replay cases.
Neither evidence set claims installed-release, live-consumer, platform-matrix or
full M9 qualification. The implementation document preserves the remaining
partial qualification and conservative exceptions.

## Reviewer-reported exact commands and results

On request, the reviewer copied these exact commands from its existing tool-call
history. No tests were rerun for this documentation. All commands used working
directory `/workspace/scratch/9c9a5a8c3955/catabolic-destination-owner`.

### 1. Paired validation and the initial incomplete-environment attempt

```bash
source /workspace/scratch/9c9a5a8c3955/catabolic/.local-tests/rust-migration/env.sh; export CARGO_TARGET_DIR="$PWD/.local-target" CARGO_BUILD_JOBS=1 PYTHONPATH="$PWD" CATABOLIC_LEGACY_BINARY="$PWD/.local-target/debug/catabolic-legacy-native"; sha256sum "$CATABOLIC_LEGACY_BINARY"; python -m unittest -v tests.test_rust_legacy_destination_mappings.NativeDestinationMappingTest.test_stale_plan_validation_and_identity tests.test_rust_legacy_destination_mappings.NativeJellyfinDestinationMappingTest.test_stale_plan_validation_and_identity && CATABOLIC_PYTHON_ORACLE="$CATABOLIC_PYTHON311" "$CATABOLIC_PYTHON311" -m unittest -v tests.test_rust_legacy_destination_mappings.NativeDestinationMappingTest.test_scalar_plan_apply_noop_and_ownership
```

Reviewer-reported results:

- Binary SHA256: `bb4a8952322fce97c3992865178a4469d21804b4ed55732c7467de73413f4867`
- Both validation tests passed: `Ran 2 tests in 23.496s`, `OK`
- Following 3.11 attempt failed during frozen CLI import:
  `ModuleNotFoundError: No module named 'pydantic'`
- Combined shell command exited 1. This initial 3.11 attempt is a failed
  environment preflight, not passing owner evidence

### 2. Independent oversized-budget probe with an actual owned removal

```bash
source /workspace/scratch/9c9a5a8c3955/catabolic/.local-tests/rust-migration/env.sh; export PYTHONPATH="$PWD" CATABOLIC_LEGACY_BINARY="$PWD/.local-target/debug/catabolic-legacy-native"; python - <<'PY'
import unittest
from tests.test_rust_legacy_destination_mappings import NativeDestinationMappingTest
class LargeBudgetRemovalReview(NativeDestinationMappingTest):
    def test_large_budget_covers_real_owned_removal(self):
        self.definition['fields']={'genres':{'constant':['Owned']}}
        self.assertTrue(self.apply_pair()['applied'])
        self.definition['fields']={'genres':{'constant':[]}}
        self.assertEqual(self.pair_preview()['removals'],1)
        self.assertTrue(self.apply_pair(maximum=10**100)['applied'])
        for e in (True,False):
            self.assertEqual(self.receiver.states[e]['genres'],['Manual'])
            self.assertEqual(self.events(e)[-1][1]['state'],'verified')
suite=unittest.TestSuite([LargeBudgetRemovalReview('test_large_budget_covers_real_owned_removal')])
result=unittest.TextTestRunner(verbosity=2).run(suite)
raise SystemExit(not result.wasSuccessful())
PY
```

Reviewer-reported result: `Ran 1 test in 10.892s`, `OK`; exit 0.

### 3. Successful dependency-equipped Python 3.11 replay

```bash
source /workspace/scratch/9c9a5a8c3955/catabolic/.local-tests/rust-migration/env.sh; export PYTHONPATH="$PWD" CATABOLIC_LEGACY_BINARY="$PWD/.local-target/debug/catabolic-legacy-native" CATABOLIC_PYTHON_ORACLE=/workspace/scratch/9c9a5a8c3955/catabolic/.local-tests/rust-migration/ci-harness/python311-reference/bin/python; "$CATABOLIC_PYTHON_ORACLE" -c 'import sys,pydantic; print(sys.version); print(pydantic.__version__)'; "$CATABOLIC_PYTHON_ORACLE" -m unittest -v tests.test_rust_legacy_destination_mappings.NativeDestinationMappingTest.test_scalar_plan_apply_noop_and_ownership
```

Reviewer-reported results:

- Python: 3.11.15
- Pydantic: 2.13.5
- `Ran 1 test in 18.597s`, `OK`; exit 0

### 4. Final whitespace and artifact/source verification

```bash
git diff --check && sha256sum .local-target/debug/catabolic-legacy-native crates/catabolic-store/src/destination_mappings.rs crates/catabolic-runtime/src/destination_mappings.rs tests/test_rust_legacy_destination_mappings.py
```

Reviewer-reported result: exit 0, with these hashes:

```text
bb4a8952322fce97c3992865178a4469d21804b4ed55732c7467de73413f4867  .local-target/debug/catabolic-legacy-native
239b25da2d8437086e111000a273ca33d32df7be709fff4ed9f3976a3828e835  crates/catabolic-store/src/destination_mappings.rs
e5c79b439994cc116979c6e15386333447dc19b83b73f61fd608e5e79cfe0567  crates/catabolic-runtime/src/destination_mappings.rs
4abc7acc53b79b97eb5b018ca2d4e058deabec0d6054dc205b68653c999686e5  tests/test_rust_legacy_destination_mappings.py
```
