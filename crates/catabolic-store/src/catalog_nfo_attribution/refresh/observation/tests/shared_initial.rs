use super::*;

#[test]
fn same_context_files_share_initial_owner_without_changing_fingerprints() {
    let fixture = Fixture::new(false).install_candidate();
    let expected = fixture
        .read(|store, budget| {
            let a =
                capture_current_in(store, &fixture.scope(), &fixture.selection_part(1), budget)?;
            let b =
                capture_current_in(store, &fixture.scope(), &fixture.selection_part(2), budget)?;
            Ok((
                a.current,
                a.full_proof_digest,
                b.current,
                b.full_proof_digest,
            ))
        })
        .unwrap();
    fixture
        .read(|store, budget| {
            let scope = fixture.scope();
            let mut pass = InitialStructurePass::new(store, &scope, budget)?;
            let a = capture_current_with_pass(&mut pass, &fixture.selection_part(1), budget)?;
            let b = capture_current_with_pass(&mut pass, &fixture.selection_part(2), budget)?;
            assert!(Arc::ptr_eq(&a.structure, &b.structure));
            assert_eq!(pass.records.len(), 1);
            assert_eq!(
                (
                    a.current.clone(),
                    a.full_proof_digest.clone(),
                    b.current.clone(),
                    b.full_proof_digest.clone()
                ),
                expected
            );
            validate_live(store, &a, budget)?;
            validate_live(store, &b, budget)?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn shared_initial_owner_refuses_other_authority_and_intervening_write() {
    let fixture = Fixture::new(false).install_candidate();
    fixture.read(|store, budget| {
        let scope = fixture.scope();
        let mut pass = InitialStructurePass::new(store, &scope, budget)?;
        capture_current_with_pass(&mut pass, &fixture.selection(), budget)?;
        let mut other = fixture.selection_part(2);
        other.lineage.work_item_id = "another-work".into();
        assert!(capture_current_with_pass(&mut pass, &other, budget).is_err());
        assert_eq!(pass.records.len(), 1);
        // A real SQL write, even rolled back, ends this read-only pass. It cannot
        // become authority to skip later changed-head/away-and-back validation.
        let head: i64 = store.db.query_row("SELECT revision FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key='[\"work\"]'", [], |r| r.get(0))?;
        store.db.execute_batch("SAVEPOINT changed_initial_pass; UPDATE items SET metadata=json_object('title','changed head') WHERE id='work';")?;
        let changed: i64 = store.db.query_row("SELECT revision FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key='[\"work\"]'", [], |r| r.get(0))?;
        assert!(changed > head);
        assert!(capture_current_with_pass(&mut pass, &fixture.selection_part(2), budget).is_err());
        store.db.execute_batch("ROLLBACK TO changed_initial_pass; RELEASE changed_initial_pass;")?;
        assert!(capture_current_with_pass(&mut pass, &fixture.selection_part(2), budget).is_err());
        Ok(())
    }).unwrap();
    fixture
        .read(|store, budget| {
            let mut scope = fixture.scope();
            scope.generation += 1;
            let mut pass = InitialStructurePass::new(store, &scope, budget)?;
            assert!(capture_current_with_pass(&mut pass, &fixture.selection(), budget).is_err());
            Ok(())
        })
        .unwrap();
}
