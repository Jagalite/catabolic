# GitHub takeover publication

The full locally committed checkpoint is `28c0a23f055b9ec21050c55a51823844ce1a1171`. Its complete history is retained locally on `codex/rust-migration-m4-plus-full-local-28c0a23` and in the verified `catabolic-28c0a23f055b9ec21050c55a51823844ce1a1171-full-history.bundle` recovery artifact. The public feature branch uses a new publication commit whose single parent is the prior remote tip `448cdc5b0958b320d2c7dfebd514624baa7a4fc3`. This preserves the existing remote history while publishing the final source snapshot. The publication commit hash is the checked-out `git rev-parse HEAD` and is also recorded in the external publication receipt.

## Exact differences from the full checkpoint

- All production code, tests, existing documentation, manifests, and other non-workflow files are identical to `28c0a23f055b9ec21050c55a51823844ce1a1171`.
- Every active `.github/workflows/` file is restored exactly to `448cdc5b0958b320d2c7dfebd514624baa7a4fc3`. The sole changed proposal, `native-qualification.yml`, is retained verbatim under `docs/workflow-proposals/` with its original destination and SHA-256 in that directory's README.
- The only additional files are this disclosure and the inactive proposal/README.

The user explicitly requested leaving remote workflow files unchanged and preserving the proposed changes as ordinary files for a local agent. The pending OAuth request for additional workflow scope was cancelled. The earlier push rejection did not update the remote branch.

## Qualification and remaining work

Read `docs/native-source-adoption-2026-10-16.md` for the passed test cohorts, evidence, local commands, remaining implementation, restricted/platform-specific qualification, and pending decisions. Those results apply to the source cohorts identified there. This publication repackages that source without a new runtime test claim; its byte/tree comparison is recorded separately. Historical full native/reference/canonical results are not a fresh full qualification of the final source.

There has been no default switch, production migration, release publication, or claim that all R0–R7 / M0–M11 work is complete. Development stopped at this requested takeover checkpoint. Review and apply the inactive workflow only as a separate local change.
