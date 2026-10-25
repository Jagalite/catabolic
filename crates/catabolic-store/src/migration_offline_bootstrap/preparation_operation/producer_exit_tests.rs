//! Additive acceptance fixtures for successful inner work followed by real
//! inherited-deadline expiry at the fixed public wrapper exit checkpoint.
use super::*;

const HOST: &str = "preparation-fixture";

fn arm_exit(bootstrap: &mut OfflineBootstrap<'_>, stage: ProducerExitStage) -> Instant {
    let inherited = bootstrap.producer_deadline.unwrap();
    // Tighten the actual inherited Instant before entry. The fixed hook waits
    // for this exact Instant; no fake clock/result or replacement budget exists.
    let deadline = inherited.min(Instant::now() + Duration::from_secs(1));
    assert!(Instant::now() < deadline);
    bootstrap.producer_deadline = Some(deadline);
    bootstrap.producer_exit_test = Some(ProducerExitTest {
        stage,
        reached: std::cell::Cell::new(false),
    });
    deadline
}
fn assert_same_custody(
    bootstrap: &OfflineBootstrap<'_>,
    path: &Path,
    identity: (usize, usize, usize),
) {
    assert_eq!(bootstrap.session.completion_custody_for_test(), identity);
    assert!(
        bootstrap
            .session
            .completion_store_for_test()
            .owns_writer_lock()
    );
    assert!(crate::writer_lock(path).is_err());
    assert!(crate::native_admission_exclusive(path).is_err());
}
fn assert_after_work(bootstrap: &OfflineBootstrap<'_>, deadline: Instant, error: &crate::Error) {
    assert!(bootstrap.producer_exit_test.as_ref().unwrap().reached.get());
    assert_eq!(bootstrap.producer_deadline, Some(deadline));
    assert!(Instant::now() >= deadline);
    assert!(error.0.contains("deadline exhausted after work"));
    assert!(error.0.contains("committed receipt may remain"));
}
fn assert_expired_public_reentry(
    bootstrap: &OfflineBootstrap<'_>,
    command: &CommandEnvelope,
    path: &Path,
    identity: (usize, usize, usize),
) {
    let before = migration::snapshot(&bootstrap.session.completion_store_for_test().db).unwrap();
    // Reset only the per-fixture observation bit, not any deadline or budget.
    bootstrap
        .producer_exit_test
        .as_ref()
        .unwrap()
        .reached
        .set(false);
    let preparation = bootstrap.prepare(HOST, &[]).unwrap_err();
    let reconciliation = bootstrap.reconcile(command, HOST).unwrap_err();
    let execution = bootstrap.execute(command, HOST, &[], policy()).unwrap_err();
    for error in [&preparation, &reconciliation.error, &execution.error] {
        assert!(error.0.contains("inherited producer deadline exhausted"));
    }
    assert!(!bootstrap.producer_exit_test.as_ref().unwrap().reached.get());
    assert_eq!(
        migration::snapshot(&bootstrap.session.completion_store_for_test().db).unwrap(),
        before
    );
    assert_same_custody(bootstrap, path, identity);
}

#[test]
fn borrowed_producer_prepare_exit_expiry_keeps_successfully_read_state_and_original_owner() {
    let (_root, path, mut bootstrap) = marked_bootstrap();
    let command = command(&bootstrap);
    let expected = bootstrap.session.prepare_host(HOST, &[]).unwrap();
    let before = migration::snapshot(&bootstrap.session.completion_store_for_test().db).unwrap();
    let identity = bootstrap.session.completion_custody_for_test();
    let deadline = arm_exit(&mut bootstrap, ProducerExitStage::Prepare);
    let error = bootstrap.prepare(HOST, &[]).unwrap_err();
    assert_after_work(&bootstrap, deadline, &error);
    assert_same_custody(&bootstrap, &path, identity);
    assert_eq!(
        migration::snapshot(&bootstrap.session.completion_store_for_test().db).unwrap(),
        before
    );
    // Genuine private session evidence, not a public expired-work escape.
    assert_eq!(bootstrap.session.prepare_host(HOST, &[]).unwrap(), expected);
    assert_expired_public_reentry(&bootstrap, &command, &path, identity);
    drop(bootstrap);
    released(&path);
}

#[test]
fn borrowed_producer_reconcile_exit_expiry_keeps_exact_committed_receipt_and_original_owner() {
    let (_root, path, mut bootstrap) = marked_bootstrap();
    let command = command(&bootstrap);
    assert!(matches!(
        bootstrap.execute(&command, HOST, &[], policy()).unwrap(),
        CommandOutcome::Committed { .. }
    ));
    let receipt = bootstrap
        .session
        .reconcile(&command, HOST)
        .unwrap()
        .unwrap();
    assert!(matches!(&receipt, CommandOutcome::Replay { .. }));
    let before = migration::snapshot(&bootstrap.session.completion_store_for_test().db).unwrap();
    let identity = bootstrap.session.completion_custody_for_test();
    let deadline = arm_exit(&mut bootstrap, ProducerExitStage::Reconcile);
    let error = bootstrap.reconcile(&command, HOST).unwrap_err();
    assert_after_work(&bootstrap, deadline, &error.error);
    assert_same_custody(&bootstrap, &path, identity);
    assert_eq!(
        migration::snapshot(&bootstrap.session.completion_store_for_test().db).unwrap(),
        before
    );
    assert_eq!(
        bootstrap.session.reconcile(&command, HOST).unwrap(),
        Some(receipt.clone())
    );
    assert_expired_public_reentry(&bootstrap, &command, &path, identity);
    assert_eq!(
        bootstrap.session.reconcile(&command, HOST).unwrap(),
        Some(receipt)
    );
    drop(bootstrap);
    released(&path);
}

#[test]
fn borrowed_producer_execute_exit_expiry_reports_error_after_real_commit_without_losing_custody() {
    let (_root, path, mut bootstrap) = marked_bootstrap();
    let command = command(&bootstrap);
    assert_eq!(bootstrap.session.reconcile(&command, HOST).unwrap(), None);
    let before = migration::snapshot(&bootstrap.session.completion_store_for_test().db).unwrap();
    let identity = bootstrap.session.completion_custody_for_test();
    let deadline = arm_exit(&mut bootstrap, ProducerExitStage::Execute);
    let error = bootstrap
        .execute(&command, HOST, &[], policy())
        .unwrap_err();
    assert_after_work(&bootstrap, deadline, &error.error);
    assert_same_custody(&bootstrap, &path, identity);
    assert!(
        bootstrap
            .session
            .completion_store_for_test()
            .db
            .is_autocommit()
    );
    assert_ne!(
        migration::snapshot(&bootstrap.session.completion_store_for_test().db).unwrap(),
        before
    );
    // The exact command now has a real replayable receipt on this SAME original
    // session. The public exit error did not undo, invent, or reopen its commit.
    let receipt = bootstrap
        .session
        .reconcile(&command, HOST)
        .unwrap()
        .unwrap();
    let committed_generation = match &receipt {
        CommandOutcome::Replay {
            committed_generation,
            ..
        } => *committed_generation,
        other => panic!("postcommit receipt was not replayable: {other:?}"),
    };
    let measured = bootstrap.session.complete(&command).unwrap();
    assert_eq!(measured.command_key(), command.key());
    assert_eq!(measured.payload_digest(), command.digest().as_str());
    assert_eq!(measured.committed_generation(), committed_generation);
    assert_eq!(
        bootstrap.session.generation().unwrap(),
        committed_generation
    );
    drop(measured);
    assert_expired_public_reentry(&bootstrap, &command, &path, identity);
    assert_eq!(
        bootstrap.session.reconcile(&command, HOST).unwrap(),
        Some(receipt)
    );
    assert_same_custody(&bootstrap, &path, identity);
    drop(bootstrap);
    released(&path);
}
