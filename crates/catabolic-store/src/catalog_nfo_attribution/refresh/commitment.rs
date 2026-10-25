//! Owner-produced commitments to fully admitted immutable values. Capturing a
//! value traverses and charges every byte/node before hashing. Reusing its hash
//! avoids a second traversal; it does not admit a caller-supplied digest or reset
//! any local/shared budget. Private candidate41 proof domains are versioned.
use super::*;

pub(super) struct AdmittedCommitment(String);
impl AdmittedCommitment {
    pub(super) fn capture(budget: &mut ProofBudget, value: &impl Serialize) -> Result<Self> {
        Ok(Self(budget.serialized(value, 64)?))
    }
    pub(super) fn digest(&self) -> &str {
        &self.0
    }
}

pub(super) fn step(
    budget: &mut ProofBudget,
    index: usize,
    effect: &impl Serialize,
    presence: &AdmittedCommitment,
    metadata_digest: &Option<String>,
    first: Option<(&str, &str)>,
    outer_digest: &str,
) -> Result<String> {
    budget.serialized(
        &(
            "catabolic.nfo.refresh.step.v2",
            index,
            effect,
            presence.digest(),
            metadata_digest,
            first,
            outer_digest,
        ),
        64,
    )
}
pub(super) fn batch(
    budget: &mut ProofBudget,
    outer_digest: &str,
    observation_digest: &str,
    plan: &AdmittedCommitment,
    steps: &[&str],
) -> Result<String> {
    budget.serialized(
        &(
            "catabolic.nfo.refresh.plan.v2",
            outer_digest,
            observation_digest,
            plan.digest(),
            steps,
        ),
        64,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation_budget::{BudgetLimits, OperationBudget};
    fn budget() -> ProofBudget {
        ProofBudget::new(
            OperationBudget::new(
                std::time::Duration::from_secs(30),
                BudgetLimits {
                    items: usize::MAX,
                    values: usize::MAX,
                    bytes: usize::MAX,
                    work: usize::MAX,
                },
            )
            .unwrap(),
        )
    }
    #[test]
    fn admitted_commitment_matches_exact_serialization_and_binds_nested_fields() {
        let value = json!({"subject":"work","source":{"revision":1,"fields":{"title":"original"}},"pin":null,"order":["a","b"]});
        let original = AdmittedCommitment::capture(&mut budget(), &value).unwrap();
        assert_eq!(
            original.digest(),
            catabolic_core::sha256(serde_json::to_vec(&value).unwrap())
        );
        for changed in [
            json!({"subject":"other","source":{"revision":1,"fields":{"title":"original"}},"pin":null,"order":["a","b"]}),
            json!({"subject":"work","source":{"revision":2,"fields":{"title":"original"}},"pin":null,"order":["a","b"]}),
            json!({"subject":"work","source":{"revision":1,"fields":{"title":"changed"}},"pin":null,"order":["a","b"]}),
            json!({"subject":"work","source":{"revision":1,"fields":{"title":"original"}},"pin":"manual","order":["a","b"]}),
            json!({"subject":"work","source":{"revision":1,"fields":{"title":"original"}},"pin":null,"order":["b","a"]}),
        ] {
            assert_ne!(
                original.digest(),
                AdmittedCommitment::capture(&mut budget(), &changed)
                    .unwrap()
                    .digest()
            );
        }
    }
    #[test]
    fn cached_step_and_batch_bind_order_effect_proof_and_operation() {
        let mut b = budget();
        let p = AdmittedCommitment::capture(&mut b, &json!({"presence":1})).unwrap();
        let effect = json!({"subject":"work","identity":"id"});
        let original = step(
            &mut b,
            0,
            &effect,
            &p,
            &Some("metadata".into()),
            Some(("absence", "audit")),
            "outer",
        )
        .unwrap();
        for changed in [
            step(
                &mut b,
                1,
                &effect,
                &p,
                &Some("metadata".into()),
                Some(("absence", "audit")),
                "outer",
            ),
            step(
                &mut b,
                0,
                &json!({"subject":"work","identity":"changed"}),
                &p,
                &Some("metadata".into()),
                Some(("absence", "audit")),
                "outer",
            ),
            step(
                &mut b,
                0,
                &effect,
                &p,
                &Some("other".into()),
                Some(("absence", "audit")),
                "outer",
            ),
            step(
                &mut b,
                0,
                &effect,
                &p,
                &Some("metadata".into()),
                Some(("absence", "other")),
                "outer",
            ),
            step(
                &mut b,
                0,
                &effect,
                &p,
                &Some("metadata".into()),
                Some(("absence", "audit")),
                "other",
            ),
        ] {
            assert_ne!(original, changed.unwrap());
        }
        let changed_presence =
            AdmittedCommitment::capture(&mut b, &json!({"presence": 2})).unwrap();
        assert_ne!(
            original,
            step(
                &mut b,
                0,
                &effect,
                &changed_presence,
                &Some("metadata".into()),
                Some(("absence", "audit")),
                "outer"
            )
            .unwrap()
        );
        let a = batch(&mut b, "outer", "observation", &p, &[&original, "second"]).unwrap();
        assert_ne!(
            a,
            batch(
                &mut b,
                "outer",
                "observation",
                &changed_presence,
                &[&original, "second"]
            )
            .unwrap()
        );
        assert_ne!(
            a,
            batch(&mut b, "outer", "observation", &p, &["second", &original]).unwrap()
        );
        assert_ne!(
            a,
            batch(&mut b, "outer", "changed", &p, &[&original, "second"]).unwrap()
        );
        assert_ne!(
            a,
            batch(&mut b, "changed", "observation", &p, &[&original, "second"]).unwrap()
        );
    }
    #[test]
    fn cached_commitments_do_not_reset_cumulative_admission() {
        let mut b = budget();
        let p = AdmittedCommitment::capture(&mut b, &vec![0; 4000]).unwrap();
        assert_eq!(p.digest().len(), 64);
        assert!(AdmittedCommitment::capture(&mut b, &vec![0; 4000]).is_err());
    }
}
