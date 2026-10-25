//! Candidate-only trusted-connection baseline. Raw clients deliberately replacing
//! SQLite hooks are outside this boundary. Callbacks never execute SQL.
use crate::{Error, Result, Store};
use rusqlite::{
    Connection,
    hooks::{AuthAction, AuthContext, Authorization},
};

use super::scope_history::origins;
fn protected(table: &str) -> bool {
    matches!(
        table,
        "catalog_source_scope_events"
            | "catalog_source_scope_heads"
            | "catalog_native_transition_40_41"
    )
}
fn decision(context: &AuthContext<'_>, origins: &[String]) -> Authorization {
    // Prevent a caller from spoofing an approved accessor with a TEMP trigger,
    // or replacing/removing the installed producer/immutability guards.
    match context.action {
        AuthAction::CreateTrigger {
            trigger_name,
            table_name,
        }
        | AuthAction::CreateTempTrigger {
            trigger_name,
            table_name,
        }
        | AuthAction::DropTrigger {
            trigger_name,
            table_name,
        }
        | AuthAction::DropTempTrigger {
            trigger_name,
            table_name,
        } if protected(table_name) || origins.iter().any(|v| v == trigger_name) => {
            return Authorization::Deny;
        }
        _ => {}
    }
    let write = match context.action {
        AuthAction::Insert { table_name }
        | AuthAction::Update { table_name, .. }
        | AuthAction::Delete { table_name }
        | AuthAction::DropTable { table_name }
        | AuthAction::AlterTable { table_name, .. } => Some(table_name),
        _ => None,
    };
    if context.database_name == Some("main") && write.is_some_and(protected) {
        let producer = context
            .accessor
            .is_some_and(|name| origins.iter().any(|v| v == name));
        return match context.action {
            // Exact transition-row authority remains in its default-deny UDF.
            // The baseline protects the owner and its guards, not that capability.
            AuthAction::Insert {
                table_name: "catalog_native_transition_40_41",
            } => Authorization::Allow,
            AuthAction::Insert { .. } if producer => Authorization::Allow,
            AuthAction::Update {
                table_name: "catalog_source_scope_heads",
                ..
            } if producer => Authorization::Allow,
            _ => Authorization::Deny,
        };
    }
    Authorization::Allow
}
fn deny_all(db: &Connection) {
    let _ = db.authorizer(Some(|_: AuthContext<'_>| Authorization::Deny));
}
/// Only call after successful candidate schema/identity/settings admission.
/// Installing a denial baseline does not itself admit a schema or a writer.
pub(crate) fn install_validated_candidate(db: &Connection) -> Result<()> {
    install_validated_closed41(db)
}

/// Requires successful closed-resource, ledger, settings and identity validation.
/// This denial baseline does not grant transition or mutation authority.
pub(crate) fn install_validated_closed41(db: &Connection) -> Result<()> {
    let allowed = origins();
    if let Err(error) = db.authorizer(Some(move |context: AuthContext<'_>| {
        decision(&context, &allowed)
    })) {
        deny_all(db);
        return Err(error.into());
    }
    Ok(())
}

/// Private fixed endpoint runner boundary state. There is no caller SQL slot.
pub(crate) struct EndpointBoundaries {
    pub(crate) attempted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    expected: std::sync::atomic::AtomicU8,
}
impl EndpointBoundaries {
    pub(crate) fn new(attempted: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self {
            attempted,
            expected: std::sync::atomic::AtomicU8::new(0),
        }
    }
    pub(crate) fn arm(&self, boundary: EndpointBoundary) {
        self.expected
            .store(boundary as u8, std::sync::atomic::Ordering::Release);
    }
    pub(crate) fn consumed(&self) -> bool {
        self.expected.load(std::sync::atomic::Ordering::Acquire) == 0
    }
    pub(crate) fn disarm(&self) {
        self.expected.store(0, std::sync::atomic::Ordering::Release);
    }
    fn accepts(&self, action: AuthAction<'_>) -> bool {
        use rusqlite::hooks::TransactionOperation;
        let value = match action {
            AuthAction::Transaction {
                operation: TransactionOperation::Begin,
            } => 1,
            // Pinned rusqlite maps SQLite's COMMIT operand to Unknown. Only the
            // runner's literal COMMIT arms this slot; all unarmed Unknowns deny.
            AuthAction::Transaction {
                operation: TransactionOperation::Unknown,
            } => 2,
            AuthAction::Transaction {
                operation: TransactionOperation::Rollback,
            } => 3,
            _ => return false,
        };
        self.expected
            .compare_exchange(
                value,
                0,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
    }
}
#[derive(Clone, Copy)]
pub(crate) enum EndpointBoundary {
    Begin = 1,
    Commit = 2,
    Rollback = 3,
}
fn endpoint_readonly(action: AuthAction<'_>) -> bool {
    match action {
        AuthAction::Read { .. }
        | AuthAction::Select
        | AuthAction::Function { .. }
        | AuthAction::Recursive => true,
        AuthAction::Pragma {
            pragma_name,
            pragma_value: None,
        } => matches!(
            pragma_name,
            "schema_version"
                | "user_version"
                | "foreign_keys"
                | "recursive_triggers"
                | "read_uncommitted"
        ),
        AuthAction::Pragma {
            pragma_name: "table_info",
            pragma_value: Some(table),
        } => !table.is_empty() && table.len() <= 256,
        _ => false,
    }
}

pub(crate) struct Guard<'a> {
    db: &'a Connection,
    store: Option<&'a Store>,
    schema: u32,
    armed: bool,
}
impl<'a> Guard<'a> {
    pub(crate) fn for_store(store: &'a Store) -> Result<Self> {
        if store.transaction_poisoned.get() || (store.writable && store.lock.is_none()) {
            return Err(Error("cannot replace unavailable Store authorizer".into()));
        }
        // A deny-all standalone poison/placeholder must not be revived either.
        store.db.query_row("SELECT 1", [], |_| Ok(()))?;
        Ok(Self {
            db: &store.db,
            store: Some(store),
            schema: store.schema_version,
            armed: true,
        })
    }
    /// Exact migration SQL only. No callback into another Store owner is allowed
    /// in this corridor; preflight fails before replacing a deny-all hook.
    pub(crate) fn for_migration(db: &'a Connection) -> Result<Self> {
        let schema = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
        Ok(Self {
            db,
            store: None,
            schema,
            armed: true,
        })
    }
    pub(crate) fn replace<F>(&self, mut callback: F) -> Result<()>
    where
        F: for<'r> FnMut(AuthContext<'r>) -> Authorization + Send + 'static,
    {
        let allowed = origins();
        let schema = self.schema;
        self.db.authorizer(Some(move |context: AuthContext<'_>| {
            if schema == 41 && decision(&context, &allowed) == Authorization::Deny {
                Authorization::Deny
            } else {
                callback(context)
            }
        }))?;
        Ok(())
    }
    /// Fixed read-only endpoint corridor. Denied attempts remain sticky even
    /// when a host interrupt callback swallows SQLite's error. The ordinary
    /// candidate baseline still composes; this is not a caller-supplied filter.
    pub(crate) fn replace_endpoint_reads(
        &self,
        attempted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<()> {
        let allowed = origins();
        let schema = self.schema;
        self.db.authorizer(Some(move |context: AuthContext<'_>| {
            let readonly = endpoint_readonly(context.action);
            if !readonly || (schema == 41 && decision(&context, &allowed) == Authorization::Deny) {
                attempted.store(true, std::sync::atomic::Ordering::Release);
                Authorization::Deny
            } else {
                Authorization::Allow
            }
        }))?;
        Ok(())
    }
    pub(crate) fn replace_endpoint_operation(
        &self,
        boundaries: std::sync::Arc<EndpointBoundaries>,
    ) -> Result<()> {
        let allowed = origins();
        let schema = self.schema;
        self.db.authorizer(Some(move |context: AuthContext<'_>| {
            let admitted = endpoint_readonly(context.action) || boundaries.accepts(context.action);
            if !admitted || (schema == 41 && decision(&context, &allowed) == Authorization::Deny) {
                boundaries
                    .attempted
                    .store(true, std::sync::atomic::Ordering::Release);
                Authorization::Deny
            } else {
                Authorization::Allow
            }
        }))?;
        Ok(())
    }
    fn restore_inner(&self) -> Result<()> {
        if self
            .store
            .is_some_and(|s| s.transaction_poisoned.get() || (s.writable && s.lock.is_none()))
        {
            deny_all(self.db);
            return Err(Error("Store poisoned while authorizer was borrowed".into()));
        }
        let result = if self.schema == 41 {
            install_validated_candidate(self.db)
        } else {
            self.db
                .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
                .map_err(Into::into)
        };
        if result.is_err() {
            if let Some(store) = self.store {
                store.transaction_poisoned.set(true);
            }
            deny_all(self.db);
        }
        result
    }
    pub(crate) fn restore(mut self) -> Result<()> {
        self.armed = false;
        self.restore_inner()
    }
}
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.restore_inner();
        }
    }
}

/// Owned, fixed planning-only observer. Its storage is preadmitted by the
/// retained rotation owner. No Store/Connection borrow or TLS guard is retained.
/// There is deliberately no Drop restoration: the operation must settle while
/// it still owns both connections, or retain the complete failed operation.
pub(crate) struct PlannedReadObserver {
    boundaries: std::sync::Arc<EndpointBoundaries>,
    restore: Option<Vec<String>>,
    handle: Option<usize>,
    installed: bool,
}
impl PlannedReadObserver {
    pub(crate) fn fixed_storage_bytes() -> Option<usize> {
        // Arc payloads plus two strong/weak counters and worst-case alignment
        // padding; two installed closure captures plus one restore capture.
        let headers = 4 * std::mem::size_of::<usize>();
        std::mem::size_of::<EndpointBoundaries>()
            .checked_add(headers)?
            .checked_add(
                std::mem::size_of::<std::sync::atomic::AtomicBool>().checked_add(headers)?,
            )?
            .checked_mul(2)?
            .checked_add(2 * std::mem::size_of::<std::sync::Arc<EndpointBoundaries>>())?
            .checked_add(std::mem::size_of::<Option<Vec<String>>>())
    }
    pub(crate) fn prepared(original: bool) -> Self {
        let attempted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        Self {
            boundaries: std::sync::Arc::new(EndpointBoundaries::new(attempted)),
            restore: original.then(origins),
            handle: None,
            installed: false,
        }
    }
    pub(crate) fn install(&mut self, db: &Connection) -> Result<()> {
        if self.installed || self.handle.is_some() {
            return Err(Error("rotation observer already installed".into()));
        }
        // The SQLite handle belongs to the retained Connection and remains
        // stable when the Rust operation moves. It is never dereferenced here.
        self.handle = Some(unsafe { db.handle() } as usize);
        self.installed = true;
        let boundaries = self.boundaries.clone();
        db.authorizer(Some(move |context: AuthContext<'_>| {
            let read = match context.action {
                AuthAction::Read { .. }
                | AuthAction::Select
                | AuthAction::Function { .. }
                | AuthAction::Recursive => true,
                // Pinned SQLite FTS5 integrity reads issue this qualified,
                // argument-free metadata query. Do not admit other schemas,
                // unqualified forms, or arguments through this exception.
                AuthAction::Pragma {
                    pragma_name: "data_version",
                    pragma_value: None,
                } => context.database_name == Some("main"),
                AuthAction::Pragma {
                    pragma_name,
                    pragma_value: None,
                } => matches!(
                    pragma_name,
                    "schema_version"
                        | "user_version"
                        | "foreign_keys"
                        | "recursive_triggers"
                        | "read_uncommitted"
                        | "page_count"
                        | "page_size"
                        | "journal_mode"
                        | "integrity_check"
                        | "foreign_key_check"
                        | "table_list"
                ),
                AuthAction::Pragma {
                    pragma_name,
                    pragma_value: Some(table),
                } => {
                    matches!(pragma_name, "table_info" | "table_xinfo")
                        && !table.is_empty()
                        && table.len() <= 256
                }
                _ => false,
            };
            if read || boundaries.accepts(context.action) {
                Authorization::Allow
            } else {
                boundaries
                    .attempted
                    .store(true, std::sync::atomic::Ordering::Release);
                Authorization::Deny
            }
        }))?;
        Ok(())
    }
    pub(crate) fn installed(&self) -> bool {
        self.installed
    }
    pub(crate) fn check(&self, db: &Connection) -> Result<()> {
        if !self.installed
            || self.handle != Some(unsafe { db.handle() } as usize)
            || self
                .boundaries
                .attempted
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(Error("rotation planning continuity refused".into()));
        }
        Ok(())
    }
    pub(crate) fn boundary(&self, db: &Connection, boundary: EndpointBoundary) -> Result<()> {
        if self.handle != Some(unsafe { db.handle() } as usize) || !self.installed {
            return Err(Error("rotation planning boundary owner differs".into()));
        }
        struct Disarm<'a>(&'a EndpointBoundaries);
        impl Drop for Disarm<'_> {
            fn drop(&mut self) {
                self.0.disarm();
            }
        }
        self.boundaries.arm(boundary);
        let _disarm = Disarm(&self.boundaries);
        let sql = match boundary {
            EndpointBoundary::Begin => "BEGIN IMMEDIATE",
            EndpointBoundary::Rollback => "ROLLBACK",
            // This planning-only owner never has commit authority.
            EndpointBoundary::Commit => {
                self.boundaries
                    .attempted
                    .store(true, std::sync::atomic::Ordering::Release);
                return Err(Error("rotation planning cannot commit".into()));
            }
        };
        let result = db.execute_batch(sql);
        if !self.boundaries.consumed() {
            self.boundaries
                .attempted
                .store(true, std::sync::atomic::Ordering::Release);
            return Err(Error("rotation planning boundary was not consumed".into()));
        }
        result?;
        Ok(())
    }
    pub(crate) fn restore(&mut self, db: &Connection, original: bool) -> Result<()> {
        if self.handle != Some(unsafe { db.handle() } as usize) || !db.is_autocommit() {
            return Err(Error(
                "rotation observer requires settled exact connection".into(),
            ));
        }
        let allowed =
            if original {
                Some(self.restore.take().ok_or_else(|| {
                    Error("rotation observer restoration already attempted".into())
                })?)
            } else {
                None
            };
        // The names already exist and the replacement callback allocation was
        // admitted before acquisition. No destructor invokes this operation.
        if original {
            db.authorizer(Some(move |context: AuthContext<'_>| {
                decision(
                    &context,
                    allowed.as_ref().expect("original restoration names"),
                )
            }))?;
        } else {
            drop(allowed);
            db.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
        }
        self.installed = false;
        Ok(())
    }
}

#[cfg(test)]
#[path = "authorizer/planned_reads_tests.rs"]
mod planned_reads_tests;
