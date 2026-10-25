//! Projection/reducer oracles only, not synthetic restore admission. Genuine
//! token/history/one-and-two-renewal coverage remains in the completed fixture.
use super::*;

struct Budget {
    charges: Vec<BudgetCharge>,
    work: usize,
    limit: usize,
    failed: bool,
}
impl Budget {
    fn new(limit: usize) -> Self {
        Self {
            charges: Vec::new(),
            work: 0,
            limit,
            failed: false,
        }
    }
    fn vector_admitted(&self, rows: usize) -> bool {
        self.charges.iter().any(|c| {
            c.items == 0
                && c.values == 0
                && c.bytes == rows * std::mem::size_of::<Value>()
                && c.work == 2
        })
    }
}
impl StructuralReadBudget for Budget {
    fn charge(&mut self, charge: BudgetCharge) -> Result<()> {
        if self.failed || charge.work > self.limit.saturating_sub(self.work) {
            self.failed = true;
            return Err(refuse("test sticky combined reader refusal"));
        }
        self.work += charge.work;
        self.charges.push(charge);
        Ok(())
    }
    fn raw_json(&mut self, _: &str) -> Result<()> {
        Err(refuse("combined row projection does not parse JSON"))
    }
}
fn fixture() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(r#"CREATE TABLE catalog_source_effective_headers(profile,file_id,scan_id,size,mtime_ns,device,inode,status,reason,bytes_read);
CREATE TABLE catalog_source_effective_file_facts(profile,file_id,operation,job_id,snapshot,data,status);
INSERT INTO catalog_source_effective_headers VALUES
 ('p','z',9,42,11,12,13,'rejected','escaped "reason"',0),
 ('p','a',NULL,42,NULL,NULL,NULL,'ok',NULL,42),
 ('p','m',NULL,0,1,2,3,NULL,'é',NULL);
INSERT INTO catalog_source_effective_file_facts VALUES
 ('p','a','z_probe','job-z','{}','{"name":"é"}','done'),
 ('p','z','probe',NULL,NULL,NULL,'failed'),
 ('p','a','a_probe','job-a','{"x":1}','[]',NULL);"#).unwrap();
    db
}
fn combined_sql(limit: usize) -> String {
    format!(
        "{COMBINED_SELECT} ORDER BY evidence_kind,file_id,operation LIMIT {}",
        limit + 1
    )
}
fn old_rows(
    db: &Connection,
    relation: &str,
    columns: &[&str],
    order: &str,
    limit: usize,
) -> Result<Vec<Value>> {
    let sql = format!(
        "SELECT {} FROM {relation} ORDER BY {order} LIMIT {}",
        columns.join(","),
        limit + 1
    );
    let mut statement = db.prepare(&sql)?;
    let mut rows = statement.query([])?;
    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        if result.len() == limit {
            return Err(refuse("old combined cohort sentinel"));
        }
        let mut object = serde_json::Map::new();
        for (index, key) in columns.iter().enumerate() {
            object.insert(
                (*key).to_owned(),
                crate::migration::value(row.get_ref(index)?),
            );
        }
        result.push(Value::Object(object));
    }
    Ok(result)
}
fn old(db: &Connection, limit: usize) -> Result<RestoredSourceEvidence> {
    let headers = old_rows(
        db,
        "catalog_source_effective_headers",
        HEADERS,
        "file_id",
        limit,
    )?;
    let facts = old_rows(
        db,
        "catalog_source_effective_file_facts",
        FACTS,
        "file_id,operation",
        limit - headers.len(),
    )?;
    Ok(RestoredSourceEvidence { headers, facts })
}
#[test]
fn combined_rows_match_separate_maps_statuses_nulls_and_operations() {
    let db = fixture();
    // Duplicate retained facts must survive UNION ALL; no distinct/coalescing.
    db.execute_batch("INSERT INTO catalog_source_effective_file_facts SELECT * FROM catalog_source_effective_file_facts WHERE operation='a_probe'").unwrap();
    for limit in [7, 8, MAX_ROWS] {
        let expected = old(&db, limit).unwrap();
        let mut budget = Budget::new(usize::MAX);
        let actual = combined_rows(&db, &combined_sql(limit), &[], limit, &mut budget).unwrap();
        assert_eq!(actual.headers(), expected.headers());
        assert_eq!(actual.facts(), expected.facts());
        assert!(
            actual
                .headers()
                .iter()
                .all(|v| v.as_object().unwrap().len() == 10)
        );
        assert!(
            actual
                .facts()
                .iter()
                .all(|v| v.as_object().unwrap().len() == 7)
        );
        assert!(
            actual
                .headers()
                .iter()
                .chain(actual.facts())
                .all(|v| v.get("evidence_kind").is_none())
        );
        assert!(budget.vector_admitted(7));
        let capacity = budget
            .charges
            .iter()
            .position(|c| {
                c.bytes == 7 * std::mem::size_of::<Value>() && c.work == 2 && c.values == 0
            })
            .unwrap();
        assert_eq!(
            budget.charges[..capacity]
                .iter()
                .map(|c| c.items)
                .sum::<usize>(),
            7,
            "entire batch admitted before either output vector"
        );
    }
}
#[test]
fn combined_rows_preserve_shared_boundary_sentinel_and_empty_relations() {
    let db = fixture();
    for limit in 0..=7 {
        let expected = old(&db, limit);
        let mut budget = Budget::new(usize::MAX);
        let actual = combined_rows(&db, &combined_sql(limit), &[], limit, &mut budget);
        assert_eq!(actual.is_ok(), expected.is_ok(), "shared row limit {limit}");
        if let (Ok(actual), Ok(expected)) = (actual, expected) {
            assert_eq!(actual.headers(), expected.headers());
            assert_eq!(actual.facts(), expected.facts());
        } else {
            assert!(
                !budget.vector_admitted(limit),
                "sentinel refusal precedes vector allocation"
            );
        }
    }
    db.execute_batch("DELETE FROM catalog_source_effective_headers")
        .unwrap();
    let actual =
        combined_rows(&db, &combined_sql(3), &[], 3, &mut Budget::new(usize::MAX)).unwrap();
    assert!(actual.headers().is_empty());
    assert_eq!(actual.facts(), old(&db, 3).unwrap().facts());
    db.execute_batch("DELETE FROM catalog_source_effective_file_facts")
        .unwrap();
    let actual =
        combined_rows(&db, &combined_sql(0), &[], 0, &mut Budget::new(usize::MAX)).unwrap();
    assert!(actual.headers().is_empty() && actual.facts().is_empty());
    assert!(
        combined_rows(
            &db,
            &combined_sql(MAX_ROWS + 1),
            &[],
            MAX_ROWS + 1,
            &mut Budget::new(usize::MAX)
        )
        .is_err()
    );
}
#[test]
fn combined_rows_reject_bad_late_scalars_before_output_and_keep_budget_sticky() {
    for value in ["X'00FF'", "CAST(X'80' AS TEXT)", "1.25"] {
        let db = fixture();
        db.execute_batch(&format!("INSERT INTO catalog_source_effective_file_facts VALUES('p','zz','probe','j','{{}}',{value},'done')")).unwrap();
        let mut budget = Budget::new(usize::MAX);
        assert!(combined_rows(&db, &combined_sql(7), &[], 7, &mut budget).is_err());
        assert!(
            !budget.vector_admitted(7),
            "complete preflight must reject malformed final row before output"
        );
    }
    let db = fixture();
    let sql = combined_sql(6);
    let mut measure = Budget::new(usize::MAX);
    combined_rows(&db, &sql, &[], 6, &mut measure).unwrap();
    let mut exact = Budget::new(measure.work);
    combined_rows(&db, &sql, &[], 6, &mut exact).unwrap();
    assert!(combined_rows(&db, &sql, &[], 6, &mut exact).is_err());
    assert!(combined_rows(&db, &sql, &[], 6, &mut exact).is_err());
    let mut short = Budget::new(measure.work - 1);
    assert!(combined_rows(&db, &sql, &[], 6, &mut short).is_err());
    assert!(short.failed);
    for bad in [
        sql.replace("0 AS evidence_kind", "2 AS evidence_kind"),
        sql.replace("NULL AS operation", "'bad' AS operation"),
    ] {
        let mut budget = Budget::new(usize::MAX);
        assert!(combined_rows(&db, &bad, &[], 6, &mut budget).is_err());
        assert!(!budget.vector_admitted(6));
    }
}
