// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! The captured generation must still be current when SQL acknowledges it.
use catabolic_core::publication_progress::{self, Effect, Evidence};
use rusqlite::{Connection, params};
#[test]
fn newer_generation_remains_pending_after_older_verification() {
    let mut db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE publication(generation INTEGER NOT NULL, verified INTEGER NOT NULL); INSERT INTO publication VALUES(3,2);").unwrap();
    let observed: i64 = db
        .query_row("SELECT generation FROM publication", [], |r| r.get(0))
        .unwrap();
    // A competing publication commits after the verification snapshot.
    db.execute("UPDATE publication SET generation=4", [])
        .unwrap();
    let tx = db.transaction().unwrap();
    let (current, verified): (i64, i64) = tx
        .query_row("SELECT generation,verified FROM publication", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    let effect = publication_progress::decide(Evidence {
        observed_generation: Some(observed),
        current_generation: current,
        verified_generation: verified,
        binding_matches: true,
        healthy: true,
    });
    assert_eq!(effect, Effect::RetainPending);
    tx.commit().unwrap();
    assert_eq!(
        db.query_row("SELECT verified FROM publication", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    let effect = publication_progress::decide(Evidence {
        observed_generation: Some(4),
        current_generation: 4,
        verified_generation: 2,
        binding_matches: true,
        healthy: true,
    });
    let Effect::Verify { generation } = effect else {
        panic!("fresh verification was not accepted")
    };
    assert_eq!(
        db.execute(
            "UPDATE publication SET verified=? WHERE generation=? AND verified<?",
            params![generation, generation, generation]
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT verified FROM publication", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        4
    );
}
