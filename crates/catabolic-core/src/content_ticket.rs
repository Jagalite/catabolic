// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Ticket allowance decisions from evidence captured in the owning transaction.
//! Authentication, source authorization, and atomic SQL revalidation remain in
//! the adapter. No transfer is completed merely by admitting a debit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DebitEvidence {
    pub remaining: i64,
    pub revoked: bool,
    pub expires: f64,
    pub now: f64,
    pub requested: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebitEffect {
    Debit { bytes: i64, remaining: i64 },
    Refuse,
}
pub fn debit(e: DebitEvidence) -> DebitEffect {
    let Ok(bytes) = i64::try_from(e.requested) else {
        return DebitEffect::Refuse;
    };
    if e.revoked
        || e.expires <= e.now
        || e.expires.is_nan()
        || e.now.is_nan()
        || e.remaining < bytes
    {
        return DebitEffect::Refuse;
    }
    DebitEffect::Debit {
        bytes,
        remaining: e.remaining - bytes,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allowance_expiry_and_revocation_fence_debits() {
        let base = DebitEvidence {
            remaining: 10,
            revoked: false,
            expires: 5.0,
            now: 1.0,
            requested: 6,
        };
        assert_eq!(
            debit(base),
            DebitEffect::Debit {
                bytes: 6,
                remaining: 4
            }
        );
        assert_eq!(
            debit(DebitEvidence {
                requested: 11,
                ..base
            }),
            DebitEffect::Refuse
        );
        assert_eq!(
            debit(DebitEvidence {
                requested: u64::MAX,
                ..base
            }),
            DebitEffect::Refuse
        );
        assert_eq!(
            debit(DebitEvidence {
                revoked: true,
                ..base
            }),
            DebitEffect::Refuse
        );
        assert_eq!(
            debit(DebitEvidence { now: 5.0, ..base }),
            DebitEffect::Refuse
        );
        assert_eq!(
            debit(DebitEvidence {
                expires: f64::NAN,
                ..base
            }),
            DebitEffect::Refuse
        );
    }
}
