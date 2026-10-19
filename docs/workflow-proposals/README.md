# Inactive GitHub Actions proposals

These files are source proposals for a local agent to review and apply. Nothing in this directory is an active GitHub Actions workflow. The publication commit keeps every `.github/workflows/` file byte-for-byte and mode-for-mode identical to remote commit `448cdc5b0958b320d2c7dfebd514624baa7a4fc3`.

The user requested excluding workflow changes from the push, then preserving them as ordinary files so a local agent can work on them. The only changed workflow in the complete tested handoff was:

- Proposal: `docs/workflow-proposals/native-qualification.yml`
- Original destination: `.github/workflows/native-qualification.yml`
- Source commit: `28c0a23f055b9ec21050c55a51823844ce1a1171`
- SHA-256: `738767842e3c063c3dc63a2517edaeebf8f6150fcf42979ecd5d0c960928fa20`
- Git blob: `135d67cf3e1ec2f16c7d4cc06565611a5c0a440c`
- Size: 17874 bytes

The proposal is a verbatim copy, with the same executable mode as its source. Do not treat its presence as CI execution or approval.

## Local review and application

1. Read `docs/native-source-adoption-2026-10-16.md` and `docs/github-handoff-publication-2026-10-16.md`. The migration and release qualification are incomplete.
2. Review the proposal, its commands, and permissions against the current active workflow.
3. When ready, copy the proposal to its original destination:

   ```sh
   cp docs/workflow-proposals/native-qualification.yml .github/workflows/native-qualification.yml
   git diff -- .github/workflows/native-qualification.yml
   ```

4. Validate the workflow locally and commit it separately. Publishing changes under `.github/workflows/` requires suitable GitHub authorization. No extra workflow authorization was obtained for this handoff.
