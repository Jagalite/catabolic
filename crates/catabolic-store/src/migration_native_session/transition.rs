//! Immutable historical transition only. No schema admission, completion proof,
//! restore permission, command receipt, or caller-asserted preservation API.
//! The parent migration owner mints the consumed capability only AFTER exact
//! locked-plan/backup/preservation/closed-resource checks, in that transaction.
use crate::{Error, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub(super) const SQL: &str = include_str!("schema41.sql");
const FUNCTION: &str = "catabolic_native_transition_40_41_insert";
const MAX_TUPLE_BYTES: usize = 2048;
const SELECT: &str = "SELECT json_array(catalog_id,source_incarnation,source_generation,approved_plan_sha256,source_full_ledger_sha256,source_tables_sha256,verified_backup_image_sha256,migration41_sql_sha256,closed41_reference_sha256,migrated_generation) FROM catalog_native_transition_40_41 WHERE id=1";
const INSERT: &str = "INSERT INTO catalog_native_transition_40_41(id,catalog_id,source_incarnation,source_generation,approved_plan_sha256,source_full_ledger_sha256,source_tables_sha256,verified_backup_image_sha256,migration41_sql_sha256,closed41_reference_sha256,migrated_generation) VALUES(1,?,?,?,?,?,?,?,?,?,?)";
fn fail(message: impl std::fmt::Display) -> Error {
    Error(format!("native40_to41_transition_refused: {message}"))
}
fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Scalar description, NOT authority. Only the parent migration owner can
/// construct it; parsing retained rows likewise never mints insertion custody.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Transition {
    pub(super) catalog_id: String,
    pub(super) source_incarnation: String,
    pub(super) source_generation: i64,
    pub(super) approved_plan_sha256: String,
    pub(super) source_full_ledger_sha256: String,
    pub(super) source_tables_sha256: String,
    pub(super) verified_backup_image_sha256: String,
    pub(super) migration41_sql_sha256: String,
    pub(super) closed41_reference_sha256: String,
    pub(super) migrated_generation: i64,
}
impl Transition {
    fn validate(&self) -> Result<()> {
        if !(1..=256).contains(&self.catalog_id.len())
            || !hex(&self.source_incarnation, 32)
            || self.source_generation < 0
            || self.migrated_generation < self.source_generation
            || [
                &self.approved_plan_sha256,
                &self.source_full_ledger_sha256,
                &self.source_tables_sha256,
                &self.verified_backup_image_sha256,
                &self.migration41_sql_sha256,
                &self.closed41_reference_sha256,
            ]
            .iter()
            .any(|s| !hex(s, 64))
        {
            return Err(fail("invalid fixed transition tuple"));
        }
        Ok(())
    }
    fn tuple(&self) -> Value {
        json!([
            self.catalog_id,
            self.source_incarnation,
            self.source_generation,
            self.approved_plan_sha256,
            self.source_full_ledger_sha256,
            self.source_tables_sha256,
            self.verified_backup_image_sha256,
            self.migration41_sql_sha256,
            self.closed41_reference_sha256,
            self.migrated_generation
        ])
    }
    pub(super) fn digest(&self) -> Result<String> {
        self.validate()?;
        // Domain and tuple ordering are immutable v1 encoding commitments.
        Ok(catabolic_core::sha256(catabolic_core::encode(&json!([
            "catabolic.native-transition.40-to-41.v1",
            self.tuple()
        ]))))
    }
    fn decode(raw: &str) -> Result<Self> {
        if raw.len() > MAX_TUPLE_BYTES {
            return Err(fail("retained tuple exceeds bound"));
        }
        let (
            catalog_id,
            source_incarnation,
            source_generation,
            approved_plan_sha256,
            source_full_ledger_sha256,
            source_tables_sha256,
            verified_backup_image_sha256,
            migration41_sql_sha256,
            closed41_reference_sha256,
            migrated_generation,
        ) = serde_json::from_str(raw)?;
        let row = Self {
            catalog_id,
            source_incarnation,
            source_generation,
            approved_plan_sha256,
            source_full_ledger_sha256,
            source_tables_sha256,
            verified_backup_image_sha256,
            migration41_sql_sha256,
            closed41_reference_sha256,
            migrated_generation,
        };
        row.validate()?;
        Ok(row)
    }
}

/// Bounded historical read only. Absence is neither fresh41 nor completion proof.
/// Incarnation ancestry and exact bootstrap receipt joins belong to the parent.
pub(super) fn read_in(db: &Connection) -> Result<Option<Transition>> {
    let raw = db
        .query_row(SELECT, [], |r| {
            let value = r.get_ref(0)?;
            let bytes = value.as_str().map_err(|_| rusqlite::Error::InvalidQuery)?;
            if bytes.len() > MAX_TUPLE_BYTES {
                return Err(rusqlite::Error::InvalidQuery);
            }
            Ok(bytes.to_owned())
        })
        .optional()?;
    raw.as_deref().map(Transition::decode).transpose()
}

/// Nonserde, nonclone and bound to the exact already-preserved connection.
/// This is crate-private migration integration, never an application grant.
pub(super) struct PreservedTransition<'a> {
    db: &'a Connection,
    row: Transition,
}
impl<'a> PreservedTransition<'a> {
    /// Called ONLY by the consuming migration hook after strict preservation,
    /// full old-ledger-prefix, verified backup and exact41 resource validation.
    pub(super) fn after_preservation(db: &'a Connection, row: Transition) -> Result<Self> {
        row.validate()?;
        let permit = Self { db, row };
        permit.check()?;
        if read_in(db)?.is_some() {
            return Err(fail("transition already exists"));
        }
        Ok(permit)
    }
    fn check(&self) -> Result<()> {
        if self.db.is_autocommit()
            || self.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
        {
            return Err(fail("exact active migration writer required"));
        }
        let version: u32 = self
            .db
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        let identity: (String, String, i64) = self.db.query_row(
            "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if version != 41
            || identity
                != (
                    self.row.catalog_id.clone(),
                    self.row.source_incarnation.clone(),
                    self.row.migrated_generation,
                )
        {
            return Err(fail("postmigration identity/generation changed"));
        }
        Ok(())
    }
    pub(super) fn append(self) -> Result<Transition> {
        self.check()?;
        let expected = self.row.tuple();
        self.db.create_scalar_function(
            FUNCTION,
            1,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |ctx| {
                let raw: String = ctx.get(0)?;
                Ok(i64::from(
                    raw.len() <= MAX_TUPLE_BYTES
                        && serde_json::from_str::<Value>(&raw).ok().as_ref() == Some(&expected),
                ))
            },
        )?;
        struct Reset<'a>(Option<&'a Connection>);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                if let Some(db) = self.0 {
                    if db.remove_function(FUNCTION, 1).is_err() {
                        poison(db);
                    }
                }
            }
        }
        let mut reset = Reset(Some(self.db));
        let r = &self.row;
        let outcome: Result<()> = (|| {
            self.db.execute(
                INSERT,
                params![
                    r.catalog_id,
                    r.source_incarnation,
                    r.source_generation,
                    r.approved_plan_sha256,
                    r.source_full_ledger_sha256,
                    r.source_tables_sha256,
                    r.verified_backup_image_sha256,
                    r.migration41_sql_sha256,
                    r.closed41_reference_sha256,
                    r.migrated_generation
                ],
            )?;
            if read_in(self.db)?.as_ref() != Some(r) {
                return Err(fail("inserted tuple differs"));
            }
            self.check()?;
            Ok(())
        })();
        if let Err(error) = self.db.remove_function(FUNCTION, 1) {
            poison(self.db);
            return Err(fail(format!("capability removal failed: {error}")));
        }
        reset.0 = None;
        outcome?;
        Ok(self.row)
    }
}
fn poison(db: &Connection) {
    // Never leave a granting callback usable if cleanup cannot be established.
    let _ = db.execute_batch("ROLLBACK");
    let _ = db.authorizer(Some(|_: rusqlite::hooks::AuthContext<'_>| {
        rusqlite::hooks::Authorization::Deny
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row() -> Transition {
        Transition {
            catalog_id: "opaque-catalog-id".into(),
            source_incarnation: "a".repeat(32),
            source_generation: 7,
            approved_plan_sha256: "1".repeat(64),
            source_full_ledger_sha256: "2".repeat(64),
            source_tables_sha256: "3".repeat(64),
            verified_backup_image_sha256: "4".repeat(64),
            migration41_sql_sha256: "5".repeat(64),
            closed41_reference_sha256: "6".repeat(64),
            migrated_generation: 7,
        }
    }
    #[test]
    fn every_tuple_field_changes_commitment() {
        let baseline = row();
        let original = baseline.digest().unwrap();
        for index in 0..10 {
            let mut tuple = baseline.tuple();
            tuple[index] = match index {
                0 => json!("00000000-0000-0000-0000-000000000002"),
                1 => json!("b".repeat(32)),
                2 => json!(6),
                9 => json!(8),
                _ => json!("f".repeat(64)),
            };
            let changed = Transition::decode(&tuple.to_string()).unwrap();
            assert_ne!(original, changed.digest().unwrap(), "field {index}");
        }
        assert!(Transition::decode(&"x".repeat(MAX_TUPLE_BYTES + 1)).is_err());
    }
    #[test]
    fn exact_consumed_insert_and_immutable_default_denial() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(SQL).unwrap();
        db.execute_batch("CREATE TABLE catalog_state(id INTEGER PRIMARY KEY,catalog_id TEXT,incarnation TEXT,generation INTEGER); PRAGMA user_version=41;").unwrap();
        let r = row();
        db.execute(
            "INSERT INTO catalog_state VALUES(1,?,?,?)",
            params![r.catalog_id, r.source_incarnation, r.migrated_generation],
        )
        .unwrap();
        assert!(db.execute("INSERT INTO catalog_native_transition_40_41 SELECT 1,'00000000-0000-0000-0000-000000000001',printf('%032d',0),7,printf('%064d',1),printf('%064d',2),printf('%064d',3),printf('%064d',4),printf('%064d',5),printf('%064d',6),7", []).is_err());
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        PreservedTransition::after_preservation(&db, r.clone())
            .unwrap()
            .append()
            .unwrap();
        assert_eq!(read_in(&db).unwrap(), Some(r));
        assert!(
            db.query_row(
                "SELECT catabolic_native_transition_40_41_insert('[]')",
                [],
                |r| r.get::<_, i64>(0)
            )
            .is_err()
        );
        assert!(
            db.execute(
                "UPDATE catalog_native_transition_40_41 SET migrated_generation=8",
                []
            )
            .is_err()
        );
        assert!(
            db.execute("DELETE FROM catalog_native_transition_40_41", [])
                .is_err()
        );
        db.execute_batch("COMMIT").unwrap();
    }
    #[test]
    fn changed_incarnation_before_append_refuses_without_marker() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(SQL).unwrap();
        db.execute_batch("CREATE TABLE catalog_state(id INTEGER PRIMARY KEY,catalog_id TEXT,incarnation TEXT,generation INTEGER); PRAGMA user_version=41;").unwrap();
        let r = row();
        db.execute(
            "INSERT INTO catalog_state VALUES(1,?,?,?)",
            params![r.catalog_id, r.source_incarnation, r.migrated_generation],
        )
        .unwrap();
        assert!(PreservedTransition::after_preservation(&db, r.clone()).is_err());
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        let capability = PreservedTransition::after_preservation(&db, r).unwrap();
        db.execute("UPDATE catalog_state SET incarnation=?", ["b".repeat(32)])
            .unwrap();
        assert!(capability.append().is_err());
        assert_eq!(read_in(&db).unwrap(), None);
        db.execute_batch("ROLLBACK").unwrap();
    }
    #[test]
    fn opaque_catalog_id_uses_exact_native_byte_bounds() {
        for id in [
            "x".to_owned(),
            "not-a-uuid".to_owned(),
            "x".repeat(256),
            "é".repeat(128),
        ] {
            let mut value = row();
            value.catalog_id = id;
            assert!(value.digest().is_ok());
            assert_eq!(
                Transition::decode(&value.tuple().to_string()).unwrap(),
                value
            );
        }
        for id in [String::new(), "x".repeat(257), "é".repeat(129)] {
            let mut value = row();
            value.catalog_id = id;
            assert!(value.digest().is_err());
            assert!(Transition::decode(&value.tuple().to_string()).is_err());
        }
    }
}
