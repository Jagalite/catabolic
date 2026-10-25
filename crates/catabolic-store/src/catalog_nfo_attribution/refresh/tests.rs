//! Candidate private read-capability denial tests. No receipt, source history or
//! successful write authority is fabricated. Full batch success uses actual
//! observation/native-owner fixtures in the companion tests.
use super::*;
use crate::catalog_nfo_attribution::candidate_tests::Fixture;
use catabolic_core::catalog_command::CommandKey;

fn command(store: &Store) -> CommandEnvelope {
    CommandEnvelope::new(
        1,
        CommandKey::new(&store.database_id, "nfo-candidate-tests", "pending-refresh").unwrap(),
        store.catalog_incarnation.as_deref().unwrap(),
        current_generation(store).unwrap(),
        r#"{"operation":"nfo_refresh","negative_capability_test":true}"#,
    )
    .unwrap()
}
#[test]
fn pending_read_requires_complete_barriers_actual_written_subject_and_exact_generation() {
    let fixture = Fixture::ready();
    let store = &fixture.store;
    let before = crate::migration::snapshot(&store.db).unwrap();
    let command = command(store);
    let result: Result<()> = store.transaction_detailed::<_, Error>(|db| {
        let mut token = RefreshReadPermit {
            outer: command.clone(),
            expected_generation: current_generation(store)?,
            subjects: ["nfo-subject".into()].into(),
            written: BTreeMap::new(),
        };
        let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
        assert!(
            token
                .validate_in(store, token.expected_generation, &mut budget)
                .is_err()
        );
        db.execute(
            "INSERT INTO catalog_metadata_permits(item_id,incarnation) VALUES (?,?)",
            params!["nfo-subject", command.admission_incarnation()],
        )?;
        token.expected_generation = current_generation(store)?;
        token.validate_in(store, token.expected_generation, &mut budget)?;
        // No actual owner has written a revision: neither old nor invented
        // revision numbers can be authorized by the pending capability.
        assert!(
            token
                .authorize_source_read_in(
                    store,
                    "nfo-subject",
                    1,
                    token.expected_generation,
                    &mut budget
                )
                .is_err()
        );
        assert!(
            token
                .authorize_state_read_in(
                    store,
                    "nfo-subject",
                    1,
                    1,
                    token.expected_generation,
                    &mut budget
                )
                .is_err()
        );
        assert!(
            token
                .authorize_source_read_in(
                    store,
                    "other-subject",
                    1,
                    token.expected_generation,
                    &mut budget
                )
                .is_err()
        );
        assert!(
            token
                .validate_in(store, token.expected_generation + 1, &mut budget)
                .is_err()
        );
        // Adding a second existing subject to the promised complete set without
        // its barrier is not a subset-capability or a permission to proceed.
        token.subjects.insert("other-subject".into());
        assert!(
            token
                .validate_in(store, token.expected_generation, &mut budget)
                .is_err()
        );
        drop(token);
        assert!(
            db.execute_batch("COMMIT").is_err(),
            "dropping pending capability must retain deferred barrier"
        );
        Err(Error(
            "expected rollback after actual COMMIT refusal".into(),
        ))
    });
    assert!(result.is_err());
    assert_eq!(crate::migration::snapshot(&store.db).unwrap(), before);
}
