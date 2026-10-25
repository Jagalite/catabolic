//! Fixed restored SELECT emission. No owned CTE/replacement/join intermediates.
use super::*;

// Fixed closed grammar, including at most 33 origins and 13 resource views.
// A checked append refuses future grammar growth rather than growing scratch.
const MAX_SEGMENTS: usize = 384;
#[derive(Clone, Copy)]
enum Segment<'a> {
    Text(&'a str),
    Decimal { value: usize, digits: usize },
}
// Fixed requested Vec capacity, its owner header, and the simultaneously live
// five-byte decimal buffer. This is admitted scratch, not a total process-RSS guarantee.
const SCRATCH_BYTES: usize = std::mem::size_of::<[Segment<'static>; MAX_SEGMENTS]>()
    + std::mem::size_of::<Vec<Segment<'static>>>()
    + std::mem::size_of::<&mut ()>()
    + std::mem::size_of::<usize>()
    + 5;
struct Plan<'a, 'b, B: super::super::StructuralReadBudget> {
    budget: &'b mut B,
    segments: Vec<Segment<'a>>,
    length: usize,
}
impl<'a, 'b, B: super::super::StructuralReadBudget> Plan<'a, 'b, B> {
    fn new(budget: &'b mut B) -> crate::Result<Self> {
        let bytes = SCRATCH_BYTES;
        // Reserve the fixed requested allocation and bounded owner construction.
        // Only its small Vec owner moves; there is no by-value inline-array copy.
        budget.charge(crate::operation_budget::BudgetCharge {
            bytes,
            work: 1 + bytes.div_ceil(64),
            ..Default::default()
        })?;
        Ok(Self {
            budget,
            segments: Vec::with_capacity(MAX_SEGMENTS),
            length: 0,
        })
    }
    fn append(&mut self, segment: Segment<'a>, length: usize) -> crate::Result<()> {
        self.budget.charge(crate::operation_budget::BudgetCharge {
            values: 1,
            // Checked metadata arithmetic plus the actual fixed-size slot move.
            work: 1 + std::mem::size_of::<Segment<'a>>().div_ceil(64),
            ..Default::default()
        })?;
        if self.segments.len() == MAX_SEGMENTS {
            return Err(crate::Error(
                "restored SQL segment grammar exceeded bound".into(),
            ));
        }
        self.length = self
            .length
            .checked_add(length)
            .ok_or_else(|| crate::Error("restored SQL length overflow".into()))?;
        // The fixed capacity was admitted before allocation. This bound means
        // push cannot request growth, including after a future grammar change.
        self.segments.push(segment);
        Ok(())
    }
    fn text(&mut self, text: &'a str) -> crate::Result<()> {
        // Borrowed length is metadata, not a scan or an owned byte copy.
        self.append(Segment::Text(text), text.len())
    }
    fn decimal(&mut self, value: usize) -> crate::Result<()> {
        if value > 16_385 {
            return Err(crate::Error("restored SQL decimal exceeded bound".into()));
        }
        let mut rest = value;
        let mut digits = 0;
        loop {
            self.budget.charge(crate::operation_budget::BudgetCharge {
                work: 1,
                ..Default::default()
            })?;
            digits += 1;
            rest /= 10;
            if rest == 0 {
                break;
            }
        }
        self.append(Segment::Decimal { value, digits }, digits)
    }
    fn definition(&mut self, text: &'a str) -> crate::Result<()> {
        self.text(",\n")?;
        self.text(text)
    }
    fn finish(&mut self) -> crate::Result<String> {
        self.budget.charge(crate::operation_budget::BudgetCharge {
            bytes: self.length,
            work: 1,
            ..Default::default()
        })?;
        let mut output = String::with_capacity(self.length);
        for segment in &self.segments {
            let mut decimal;
            let text = match segment {
                Segment::Text(text) => *text,
                Segment::Decimal { value, digits } => {
                    // Five-byte stack initialization, then one bounded division
                    // and ASCII store per digit, before performing that work.
                    self.budget.charge(crate::operation_budget::BudgetCharge {
                        work: 1 + *digits,
                        ..Default::default()
                    })?;
                    decimal = [0u8; 5];
                    let mut value = *value;
                    for index in (0..*digits).rev() {
                        decimal[index] = b'0' + (value % 10) as u8;
                        value /= 10;
                    }
                    self.budget.charge(crate::operation_budget::BudgetCharge {
                        work: 1,
                        ..Default::default()
                    })?;
                    std::str::from_utf8(&decimal[..*digits])
                        .map_err(|_| crate::Error("restored SQL decimal encoding failed".into()))?
                }
            };
            self.budget.charge(crate::operation_budget::BudgetCharge {
                work: 1 + text.len().div_ceil(64),
                ..Default::default()
            })?;
            let next = output
                .len()
                .checked_add(text.len())
                .ok_or_else(|| crate::Error("restored SQL length overflow".into()))?;
            if next > self.length {
                return Err(crate::Error(
                    "restored SQL exceeded admitted capacity".into(),
                ));
            }
            output.push_str(text);
        }
        if output.len() != self.length {
            return Err(crate::Error("restored SQL counted length changed".into()));
        }
        Ok(output)
    }
}
enum Selection<'a> {
    #[cfg_attr(not(test), allow(dead_code))]
    Single {
        relation: &'a str,
        columns: &'a [&'a str],
        order: &'a str,
    },
    Both,
}
fn emit<'a, B: super::super::StructuralReadBudget>(
    sink: &mut Plan<'a, '_, B>,
    selection: &Selection<'a>,
    limit: usize,
    count: usize,
    seed: &ReadSeed,
) -> crate::Result<()> {
    let registry_predicate = match seed {
        ReadSeed::Location => "f.location=request.selector",
        ReadSeed::File => "f.id=request.selector",
    };
    sink.text("SELECT ")?;
    match selection {
        Selection::Single { columns, .. } => {
            for (index, column) in columns.iter().enumerate() {
                if index != 0 {
                    sink.text(",")?;
                }
                sink.text(column)?;
            }
        }
        Selection::Both => sink.text("*")?,
    }
    sink.text(" FROM (WITH source_read_request(profile,selector) AS (VALUES (?,?)),\nsource_read_origins(rank,incarnation) AS (VALUES ")?;
    for rank in 0..count {
        if rank != 0 {
            sink.text(",")?;
        }
        sink.text("(")?;
        sink.decimal(rank)?;
        sink.text(",?)")?;
    }
    sink.text(")")?;
    sink.definition("source_read_files AS (SELECT f.id,f.location FROM main.files f JOIN source_read_request request ON ")?;
    sink.text(registry_predicate)?;
    sink.text(")")?;
    sink.definition("source_read_seed AS MATERIALIZED (SELECT b.profile,b.owner AS source_id FROM main.bindings b JOIN source_read_request request ON request.profile=b.profile WHERE b.kind='source' AND EXISTS(SELECT 1 FROM source_read_files f WHERE f.location=b.owner))")?;
    let tables = [
        "catalog_source_successions",
        "catalog_source_succession_files",
        "catalog_source_succession_contexts",
        "catalog_source_succession_heads",
        "catalog_source_succession_context_heads",
        "catalog_source_succession_scopes",
        "catalog_source_succession_evidence",
    ];
    for name in tables {
        if name == "catalog_source_successions" {
            // Closed-schema primary key (profile, source_id, revision). Keep
            // the selected seed outside the indexed parent loop: otherwise
            // SQLite can charge scalar predicates on unrelated catalog parents
            // before applying the logically equivalent seed join. No full-row
            // materialization is introduced ahead of history admission.
            sink.definition(
                "catalog_source_successions AS NOT MATERIALIZED (SELECT base.* FROM source_read_seed seed CROSS JOIN main.catalog_source_successions base INDEXED BY sqlite_autoindex_catalog_source_successions_2 ON seed.profile=base.profile AND seed.source_id=base.source_id)",
            )?;
        } else {
            sink.definition(name)?;
            sink.text(" AS NOT MATERIALIZED (SELECT base.* FROM main.")?;
            sink.text(name)?;
            sink.text(" base JOIN source_read_seed seed ON seed.profile=base.profile AND seed.source_id=base.source_id)")?;
        }
    }
    for name in ["file_facts", "media_header_checks"] {
        sink.definition(name)?;
        sink.text(" AS NOT MATERIALIZED (SELECT base.* FROM main.")?;
        sink.text(name)?;
        sink.text(" base JOIN source_read_request request ON request.profile=base.profile WHERE EXISTS(SELECT 1 FROM source_read_files f WHERE f.id=base.file_id))")?;
    }

    for name in crate::native41_resources::SOURCE_VIEWS {
        // Names come only from the reviewed 13-name list. Charge length-aware
        // lookup/framing comparisons; avoid constructing a formatted prefix.
        let lookup_bytes = name
            .len()
            .checked_mul(14)
            .and_then(|n| n.checked_add(16))
            .ok_or_else(|| crate::Error("restored SQL resource length overflow".into()))?;
        sink.budget.charge(crate::operation_budget::BudgetCharge {
            work: 1 + lookup_bytes.div_ceil(64),
            ..Default::default()
        })?;
        let body = crate::native41_resources::source_view_sql(name)?
            .strip_prefix("CREATE VIEW ")
            .and_then(|sql| sql.strip_prefix(*name))
            .and_then(|sql| sql.strip_prefix(" AS\n"))
            .ok_or_else(|| crate::Error("unexpected closed source view framing".into()))?;
        let materialization = if *name == "catalog_source_live_transfers" {
            "MATERIALIZED"
        } else {
            "NOT MATERIALIZED"
        };
        sink.definition(name)?;
        sink.text(" AS ")?;
        sink.text(materialization)?;
        sink.text(" (")?;
        if *name == "catalog_source_live_transfers" {
            emit_restored_body(sink, body)?;
        } else {
            sink.text(body)?;
        }
        sink.text(")")?;
    }
    match selection {
        Selection::Single {
            relation, order, ..
        } => {
            sink.text(" SELECT * FROM ")?;
            sink.text(relation)?;
            sink.text(") ORDER BY ")?;
            sink.text(order)?;
            sink.text(" LIMIT ")?;
            sink.decimal(limit + 1)
        }
        Selection::Both => {
            sink.text(" ")?;
            sink.text(super::super::restored_evidence::COMBINED_SELECT)?;
            sink.text(") ORDER BY evidence_kind,file_id,operation LIMIT ")?;
            sink.decimal(limit + 1)
        }
    }
}

fn emit_restored_body<'a, B: super::super::StructuralReadBudget>(
    sink: &mut Plan<'a, '_, B>,
    body: &'a str,
) -> crate::Result<()> {
    const CURRENT: &str = "JOIN catalog_state identity ON identity.id=1 AND identity.catalog_id=s.catalog_id AND identity.incarnation=s.incarnation";
    const HISTORY: &str = "x.catalog_id<>s.catalog_id OR x.incarnation<>s.incarnation";
    const CURRENT_REPLACEMENT: &str = "JOIN catalog_state identity ON identity.id=1 AND identity.catalog_id=s.catalog_id AND EXISTS(SELECT 1 FROM source_read_origins origin WHERE origin.incarnation=s.incarnation AND s.committed_generation<=identity.generation)";
    const HISTORY_REPLACEMENT: &str = "x.catalog_id<>s.catalog_id OR NOT EXISTS(SELECT 1 FROM source_read_origins origin WHERE origin.incarnation=x.incarnation) OR x.committed_generation>identity.generation OR (x.revision>1 AND (SELECT rank FROM source_read_origins WHERE incarnation=prior.incarnation)>(SELECT rank FROM source_read_origins WHERE incarnation=x.incarnation))";
    // Validate both exact unique patterns once, before retaining borrowed pieces.
    sink.budget.charge(crate::operation_budget::BudgetCharge {
        work: 2 + 2 * body.len().div_ceil(64),
        ..Default::default()
    })?;
    let mut current = body.match_indices(CURRENT);
    let mut history = body.match_indices(HISTORY);
    let positions = (
        current.next(),
        history.next(),
        current.next(),
        history.next(),
    );
    let (Some((a, _)), Some((b, _)), None, None) = positions else {
        return Err(crate::Error(
            "reviewed restored source predicate framing changed".into(),
        ));
    };
    if a + CURRENT.len() > b {
        return Err(crate::Error(
            "reviewed restored source predicate order changed".into(),
        ));
    }
    sink.text(&body[..a])?;
    sink.text(CURRENT_REPLACEMENT)?;
    sink.text(&body[a + CURRENT.len()..b])?;
    sink.text(HISTORY_REPLACEMENT)?;
    sink.text(&body[b + HISTORY.len()..])
}

#[cfg(test)]
pub(super) fn build<B: super::super::StructuralReadBudget>(
    relation: &str,
    columns: &[&str],
    order: &str,
    limit: usize,
    count: usize,
    seed: ReadSeed,
    budget: &mut B,
) -> crate::Result<String> {
    if !(1..=33).contains(&count)
        || limit > 16_384
        || !matches!(
            relation,
            "catalog_source_effective_headers" | "catalog_source_effective_file_facts"
        )
    {
        return Err(crate::Error("unsupported restored SELECT framing".into()));
    }
    let (expected_columns, expected_order) = if relation == "catalog_source_effective_headers" {
        (super::super::restored_evidence::HEADERS, "file_id")
    } else {
        (super::super::restored_evidence::FACTS, "file_id,operation")
    };
    if columns != expected_columns || order != expected_order {
        return Err(crate::Error(
            "unsupported restored SELECT projection".into(),
        ));
    }
    build_selection(
        &Selection::Single {
            relation,
            columns,
            order,
        },
        limit,
        count,
        seed,
        budget,
    )
}

pub(super) fn build_combined(
    limit: usize,
    count: usize,
    seed: ReadSeed,
    budget: &mut impl super::super::StructuralReadBudget,
) -> crate::Result<String> {
    if !(1..=33).contains(&count) || limit > 16_384 {
        return Err(crate::Error(
            "unsupported restored combined SELECT framing".into(),
        ));
    }
    build_selection(&Selection::Both, limit, count, seed, budget)
}

fn build_selection(
    selection: &Selection<'_>,
    limit: usize,
    count: usize,
    seed: ReadSeed,
    budget: &mut impl super::super::StructuralReadBudget,
) -> crate::Result<String> {
    let mut plan = Plan::new(budget)?;
    emit(&mut plan, selection, limit, count, &seed)?;
    plan.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation_budget::BudgetCharge;
    struct Budget {
        work: usize,
        bytes: usize,
        limit: usize,
        byte_limit: usize,
        failed: bool,
    }
    impl Budget {
        fn new(limit: usize) -> Self {
            Self {
                work: 0,
                bytes: 0,
                limit,
                byte_limit: usize::MAX,
                failed: false,
            }
        }
    }
    impl crate::catalog_identity_mutation::StructuralReadBudget for Budget {
        fn charge(&mut self, charge: BudgetCharge) -> crate::Result<()> {
            if self.failed
                || charge.work > self.limit.saturating_sub(self.work)
                || charge.bytes > self.byte_limit.saturating_sub(self.bytes)
            {
                self.failed = true;
                return Err(crate::Error("test sticky work refusal".into()));
            }
            self.work += charge.work;
            self.bytes += charge.bytes;
            Ok(())
        }
        fn raw_json(&mut self, _: &str) -> crate::Result<()> {
            Err(crate::Error("SQL emitter must not parse JSON".into()))
        }
    }
    #[test]
    fn combined_select_shares_exact_old_ctes_for_every_origin_and_seed() {
        for count in 1..=33 {
            for file in [false, true] {
                let old = super::super::seeded_relation_inner(
                    "catalog_source_effective_headers",
                    if file {
                        ReadSeed::File
                    } else {
                        ReadSeed::Location
                    },
                    Some(count),
                )
                .unwrap();
                let prefix = old
                    .strip_suffix(" SELECT * FROM catalog_source_effective_headers)")
                    .unwrap();
                let expected = format!(
                    "SELECT * FROM {prefix} {}) ORDER BY evidence_kind,file_id,operation LIMIT 16385",
                    super::super::super::restored_evidence::COMBINED_SELECT
                );
                let mut budget = Budget::new(usize::MAX);
                let actual = build_combined(
                    16384,
                    count,
                    if file {
                        ReadSeed::File
                    } else {
                        ReadSeed::Location
                    },
                    &mut budget,
                )
                .unwrap();
                assert_eq!(actual, expected);
                assert_eq!(
                    actual.matches("source_read_seed AS MATERIALIZED (").count(),
                    1
                );
                assert_eq!(
                    actual
                        .matches("catalog_source_live_transfers AS MATERIALIZED (")
                        .count(),
                    1
                );
                assert_eq!(actual.matches('?').count(), count + 2);
                assert_eq!(budget.bytes, actual.len() + SCRATCH_BYTES);
                for name in crate::native41_resources::SOURCE_VIEWS {
                    assert_eq!(actual.matches(&format!(",\n{name} AS ")).count(), 1);
                }
            }
        }
    }
    #[test]
    fn combined_select_refuses_invalid_bounds_and_preserves_sticky_counting() {
        for (limit, count) in [(0, 0), (16385, 1), (16384, 34)] {
            let mut budget = Budget::new(usize::MAX);
            assert!(build_combined(limit, count, ReadSeed::Location, &mut budget).is_err());
            assert_eq!(budget.bytes, 0);
        }
        let mut measured = Budget::new(usize::MAX);
        build_combined(16384, 2, ReadSeed::Location, &mut measured).unwrap();
        let mut exact = Budget::new(measured.work);
        build_combined(16384, 2, ReadSeed::Location, &mut exact).unwrap();
        assert!(build_combined(16384, 2, ReadSeed::Location, &mut exact).is_err());
        let mut short = Budget::new(measured.work - 1);
        assert!(build_combined(16384, 2, ReadSeed::Location, &mut short).is_err());
        assert!(short.failed);
        let mut initial = Budget::new(0);
        assert!(build_combined(0, 1, ReadSeed::Location, &mut initial).is_err());
        assert_eq!(initial.bytes, 0);
    }
    #[test]
    fn final_select_matches_previous_complete_sql_for_every_origin_count() {
        for count in 1..=33 {
            for location in [true, false] {
                for headers in [true, false] {
                    let (relation, columns, order) = if headers {
                        (
                            "catalog_source_effective_headers",
                            super::super::super::restored_evidence::HEADERS,
                            "file_id",
                        )
                    } else {
                        (
                            "catalog_source_effective_file_facts",
                            super::super::super::restored_evidence::FACTS,
                            "file_id,operation",
                        )
                    };
                    let old = super::super::seeded_relation_inner(
                        relation,
                        if location {
                            ReadSeed::Location
                        } else {
                            ReadSeed::File
                        },
                        Some(count),
                    )
                    .unwrap();
                    let expected = format!(
                        "SELECT {} FROM {old} ORDER BY {order} LIMIT 16385",
                        columns.join(",")
                    );
                    let mut budget = Budget::new(usize::MAX);
                    let actual = build(
                        relation,
                        columns,
                        order,
                        16384,
                        count,
                        if location {
                            ReadSeed::Location
                        } else {
                            ReadSeed::File
                        },
                        &mut budget,
                    )
                    .unwrap();
                    assert_eq!(actual, expected);
                    assert_eq!(
                        budget.bytes,
                        actual.len() + SCRATCH_BYTES,
                        "only fixed segment scratch and the final SQL buffer are owned"
                    );
                    assert_eq!(actual.matches('?').count(), count + 2);
                }
            }
        }
    }
    #[test]
    fn final_select_rejects_framing_and_keeps_cumulative_failure_sticky() {
        let columns = super::super::super::restored_evidence::HEADERS;
        let run = |count, budget: &mut Budget| {
            build(
                "catalog_source_effective_headers",
                columns,
                "file_id",
                16384,
                count,
                ReadSeed::Location,
                budget,
            )
        };
        for count in [0, 34] {
            assert!(run(count, &mut Budget::new(usize::MAX)).is_err());
        }
        assert!(
            build(
                "unknown",
                columns,
                "file_id",
                1,
                1,
                ReadSeed::Location,
                &mut Budget::new(usize::MAX)
            )
            .is_err()
        );
        assert!(
            build(
                "catalog_source_effective_headers",
                columns,
                "other",
                1,
                1,
                ReadSeed::Location,
                &mut Budget::new(usize::MAX)
            )
            .is_err()
        );
        let mut measure = Budget::new(usize::MAX);
        run(2, &mut measure).unwrap();
        let mut exact = Budget::new(measure.work);
        run(2, &mut exact).unwrap();
        assert!(run(2, &mut exact).is_err());
        assert!(run(2, &mut exact).is_err());
        let mut short = Budget::new(measure.work - 1);
        assert!(run(2, &mut short).is_err());
        assert!(short.failed);
        let mut initial = Budget::new(0);
        assert!(run(2, &mut initial).is_err());
        assert_eq!(initial.bytes, 0, "counting refusal precedes allocation");
    }
    #[test]
    fn borrowed_replacements_reject_missing_duplicate_and_drifted_templates() {
        let original =
            crate::native41_resources::source_view_select("catalog_source_live_transfers").unwrap();
        for text in [
            String::new(),
            format!("{original}{original}"),
            original.replace(
                "identity.incarnation=s.incarnation",
                "identity.incarnation = s.incarnation",
            ),
        ] {
            let mut budget = Budget::new(usize::MAX);
            let mut plan = Plan::new(&mut budget).unwrap();
            assert!(emit_restored_body(&mut plan, &text).is_err());
        }
    }
    #[test]
    fn segment_plan_bounds_scratch_decimal_and_maximum_grammar() {
        let scratch = SCRATCH_BYTES;
        assert_eq!(
            scratch,
            MAX_SEGMENTS * std::mem::size_of::<Segment>() + std::mem::size_of::<Plan<Budget>>() + 5
        );
        for count in [1, 33] {
            for limit in [0, 16_384] {
                for file in [false, true] {
                    let mut budget = Budget::new(usize::MAX);
                    let mut plan = Plan::new(&mut budget).unwrap();
                    emit(
                        &mut plan,
                        &Selection::Both,
                        limit,
                        count,
                        &if file {
                            ReadSeed::File
                        } else {
                            ReadSeed::Location
                        },
                    )
                    .unwrap();
                    // Current closed grammar:150 fixed pieces plus4 per origin.
                    // Keep this exact assertion so template drift is reviewed.
                    assert_eq!(plan.segments.len(), 150 + 4 * count);
                    assert_eq!(plan.segments.capacity(), MAX_SEGMENTS);
                    let length = plan.length;
                    let actual = plan.finish().unwrap();
                    assert_eq!(actual.len(), length);
                    assert!(actual.ends_with(&format!("LIMIT {}", limit + 1)));
                    assert_eq!(
                        budget.bytes,
                        scratch + length,
                        "scratch and final SQL coexist and are both admitted"
                    );
                }
            }
        }
        let mut budget = Budget::new(usize::MAX);
        let mut plan = Plan::new(&mut budget).unwrap();
        for value in [0, 9, 10, 32, 99, 100, 16_385] {
            plan.decimal(value).unwrap();
            plan.text(",").unwrap();
        }
        assert_eq!(plan.finish().unwrap(), "0,9,10,32,99,100,16385,");
        let mut budget = Budget::new(usize::MAX);
        let mut plan = Plan::new(&mut budget).unwrap();
        assert!(plan.decimal(16_386).is_err());
        for _ in 0..MAX_SEGMENTS {
            plan.text("").unwrap();
        }
        assert!(plan.text("").is_err());
    }
    #[test]
    fn segment_plan_admits_scratch_and_output_before_ownership() {
        let scratch = SCRATCH_BYTES;
        let mut short = Budget::new(usize::MAX);
        short.byte_limit = scratch - 1;
        assert!(build_combined(16_384, 2, ReadSeed::Location, &mut short).is_err());
        assert_eq!(short.bytes, 0, "no scratch admitted");
        assert!(short.failed);
        let mut measure = Budget::new(usize::MAX);
        let sql = build_combined(16_384, 2, ReadSeed::Location, &mut measure).unwrap();
        assert_eq!(measure.bytes, scratch + sql.len());
        let mut short = Budget::new(usize::MAX);
        short.byte_limit = measure.bytes - 1;
        assert!(build_combined(16_384, 2, ReadSeed::Location, &mut short).is_err());
        assert_eq!(
            short.bytes, scratch,
            "final SQL allocation was not admitted"
        );
        assert!(short.failed);
        assert!(build_combined(16_384, 2, ReadSeed::Location, &mut short).is_err());
        let mut exact = Budget::new(usize::MAX);
        exact.byte_limit = measure.bytes;
        assert_eq!(
            build_combined(16_384, 2, ReadSeed::Location, &mut exact).unwrap(),
            sql
        );
        assert!(build_combined(16_384, 2, ReadSeed::Location, &mut exact).is_err());
    }
    /// Actual SQLite evaluation of both complete old SELECTs and the complete
    /// combined SELECT. This is a schema/query oracle, not restore admission:
    /// no succession rows, restore token, receipt or physical grant is fabricated.
    /// The reducer oracle separately covers duplicate/nullable transferred rows
    /// that the constrained baseline tables cannot represent.
    #[test]
    fn complete_generated_queries_match_actual_sqlite_results_and_shared_limits() {
        use super::super::super::restored_evidence::{FACTS, HEADERS};
        use rusqlite::{Connection, params_from_iter, types::Value as SqlValue};
        use serde_json::Value;

        fn query(db: &Connection, sql: &str, args: &[SqlValue]) -> Vec<Vec<Value>> {
            let mut statement = db.prepare(sql).unwrap();
            let width = statement.column_count();
            let mut rows = statement.query(params_from_iter(args)).unwrap();
            let mut result = Vec::new();
            while let Some(row) = rows.next().unwrap() {
                result.push(
                    (0..width)
                        .map(|i| crate::migration::value(row.get_ref(i).unwrap()))
                        .collect(),
                );
            }
            result
        }
        fn old_sql(
            relation: &str,
            columns: &[&str],
            order: &str,
            limit: usize,
            origins: usize,
        ) -> String {
            let relation =
                super::super::seeded_relation_inner(relation, ReadSeed::Location, Some(origins))
                    .unwrap();
            format!(
                "SELECT {} FROM {relation} ORDER BY {order} LIMIT {}",
                columns.join(","),
                limit + 1
            )
        }
        fn compare(db: &Connection, selector: &str, limit: usize, origins: usize) {
            let mut args = vec![
                SqlValue::Text("oracle".into()),
                SqlValue::Text(selector.into()),
            ];
            args.extend((0..origins).map(|i| SqlValue::Text(format!("{i:032x}"))));
            let headers = query(
                db,
                &old_sql(
                    "catalog_source_effective_headers",
                    HEADERS,
                    "file_id",
                    limit,
                    origins,
                ),
                &args,
            );
            let expected = if headers.len() > limit {
                None
            } else {
                let remaining = limit - headers.len();
                let facts = query(
                    db,
                    &old_sql(
                        "catalog_source_effective_file_facts",
                        FACTS,
                        "file_id,operation",
                        remaining,
                        origins,
                    ),
                    &args,
                );
                (facts.len() <= remaining).then_some((headers, facts))
            };
            let sql = build_combined(
                limit,
                origins,
                ReadSeed::Location,
                &mut Budget::new(usize::MAX),
            )
            .unwrap();
            let rows = query(db, &sql, &args);
            let actual = if rows.len() > limit {
                None
            } else {
                let (mut headers, mut facts): (Vec<Vec<Value>>, Vec<Vec<Value>>) =
                    (Vec::new(), Vec::new());
                let mut seen_fact = false;
                for row in rows {
                    assert_eq!(row.len(), 15);
                    match row[0].as_i64().unwrap() {
                        0 => {
                            assert!(!seen_fact);
                            assert!(row[11..].iter().all(Value::is_null));
                            headers.push(row[1..11].to_vec());
                        }
                        1 => {
                            seen_fact = true;
                            assert!([3, 4, 5, 6, 7, 9, 10].into_iter().all(|i| row[i].is_null()));
                            facts.push(
                                [1, 2, 11, 12, 13, 14, 8]
                                    .into_iter()
                                    .map(|i| row[i].clone())
                                    .collect(),
                            );
                        }
                        _ => panic!("unexpected evidence discriminator"),
                    }
                }
                Some((headers, facts))
            };
            assert_eq!(
                actual, expected,
                "selector={selector}, limit={limit}, origins={origins}"
            );
            let expected_counts = match selector {
                "selected" => (3, 3),
                "headers-only" => (1, 0),
                "facts-only" => (0, 1),
                "empty" | "absent" => (0, 0),
                _ => panic!("unreviewed oracle selector"),
            };
            assert_eq!(
                actual.is_some(),
                expected_counts.0 + expected_counts.1 <= limit
            );
            if let Some((headers, facts)) = &actual {
                assert_eq!((headers.len(), facts.len()), expected_counts);
            }
            if selector == "selected" && limit >= 6 {
                let (headers, facts) = actual.unwrap();
                assert_eq!((headers.len(), facts.len()), (3, 3));
                assert_eq!(
                    headers
                        .iter()
                        .map(|r| r[7].as_str().unwrap())
                        .collect::<Vec<_>>(),
                    ["invalid", "unknown", "not_detected"]
                );
                assert!(headers[1][8].is_null());
                assert_eq!(
                    facts
                        .iter()
                        .map(|r| r[2].as_str().unwrap())
                        .collect::<Vec<_>>(),
                    ["alpha", "zeta", "probe"]
                );
                assert_eq!(
                    facts
                        .iter()
                        .map(|r| r[6].as_str().unwrap())
                        .collect::<Vec<_>>(),
                    ["done", "failed", "unknown"]
                );
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("query-oracle.sqlite3");
        crate::migration::initialize_native_at(&path, 40).unwrap();
        let db = crate::connect(&path, true).unwrap();
        db.execute_batch(r#"
            INSERT INTO profiles(id) VALUES('oracle');
            INSERT INTO locations(id) VALUES('selected'),('facts-only'),('headers-only'),('empty'),('unrelated');
            INSERT INTO bindings(profile,kind,owner,root,device,inode) VALUES
              ('oracle','source','selected','/oracle/selected',1,1),
              ('oracle','source','facts-only','/oracle/facts',1,2),
              ('oracle','source','headers-only','/oracle/headers',1,3),
              ('oracle','source','empty','/oracle/empty',1,4),
              ('oracle','source','unrelated','/oracle/unrelated',1,5);
            INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES
              ('scan','oracle','selected',1,3,'[]'),
              ('scan-header','oracle','headers-only',1,1,'[]'),
              ('scan-unrelated','oracle','unrelated',1,1,'[]');
            INSERT INTO files(id,location,path) VALUES
              ('a','selected','a'),('m','selected','m'),('z','selected','z'),
              ('f','facts-only','f'),('h','headers-only','h'),('u','unrelated','u');
            INSERT INTO media_header_checks(profile,file_id,scan_id,size,mtime_ns,device,inode,status,reason,bytes_read) VALUES
              ('oracle','a','scan',10,11,1,12,'invalid','quoted "雪"',4),
              ('oracle','m','scan',20,21,1,22,'unknown',NULL,0),
              ('oracle','z','scan',30,31,1,32,'not_detected',NULL,8),
              ('oracle','h','scan-header',40,41,1,42,'unknown',NULL,0),
              ('oracle','u','scan-unrelated',50,51,1,52,'invalid','unrelated',4);
            INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state) VALUES
              ('job-a','oracle','a','alpha','selected','{}','{}','a','done'),
              ('job-z','oracle','a','zeta','selected','{}','{}','z','done'),
              ('job-p','oracle','z','probe','selected','{}','{}','p','done'),
              ('job-f','oracle','f','probe','facts-only','{}','{}','f','done'),
              ('job-u','oracle','u','probe','unrelated','{}','{}','u','done');
            INSERT INTO file_facts(profile,file_id,operation,job_id,snapshot,data,status) VALUES
              ('oracle','a','alpha','job-a','{}','{"value":"雪"}','done'),
              ('oracle','a','zeta','job-z','{}','null','failed'),
              ('oracle','z','probe','job-p','{}','[]','unknown'),
              ('oracle','f','probe','job-f','{}','{}','done'),
              ('oracle','u','probe','job-u','{}','{}','done');
        "#).unwrap();
        // Exact compiled relation resources, not hand-written substitutes for
        // their joins, predicates, source seeds, indexes or UDF dependencies.
        for &(_, resource) in crate::native41_resources::parts() {
            db.execute_batch(resource).unwrap();
        }
        // This SQL-only oracle compares many independent old/new requests in
        // one test. Its single test owner is not a production admission or a
        // timing acceptance claim; genuine one/two-hop budgets stay unchanged.
        let operation = crate::operation_budget::OperationBudget::new(
            std::time::Duration::from_secs(60),
            crate::operation_budget::BudgetLimits {
                items: 16384,
                values: 8192,
                bytes: 2 * 1024 * 1024,
                work: 500_000,
            },
        )
        .unwrap();
        let _operation = operation.enter().unwrap();
        db.execute_batch("BEGIN; SELECT count(*) FROM files;")
            .unwrap();
        for limit in 0..=7 {
            compare(&db, "selected", limit, 2);
        }
        for selector in ["facts-only", "headers-only", "empty", "absent"] {
            for limit in 0..=2 {
                compare(&db, selector, limit, 2);
            }
        }
        for origins in [1, 33] {
            compare(&db, "selected", 6, origins);
        }
        db.execute_batch("ROLLBACK").unwrap();
    }
}
