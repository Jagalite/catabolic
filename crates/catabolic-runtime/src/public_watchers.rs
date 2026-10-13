// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Public adapters retain the caller's Store, transaction and evaluation scope.
use catabolic_store::{Error, Result, Store, embedding::Session, selection::Evaluation, watcher};
use serde_json::{Value, json};
use std::ops::{Deref, DerefMut};

struct Evaluated<'a>(&'a mut Evaluation);
impl Deref for Evaluated<'_> {
    type Target = Store;
    fn deref(&self) -> &Store {
        self.0.store()
    }
}
impl DerefMut for Evaluated<'_> {
    fn deref_mut(&mut self) -> &mut Store {
        self.0.store_mut()
    }
}

pub fn call(session: &mut Session, profile: &str, operation: &str, args: &Value) -> Result<Value> {
    session.store()?;
    if operation == "run" {
        run(session, profile, args)
    } else {
        session.watcher_call(profile, operation, args)
    }
}

fn run(session: &mut Session, profile: &str, args: &Value) -> Result<Value> {
    let name = args["name"].as_str().unwrap_or("");
    let definition = watcher::definition(session.store()?, profile, name)?;
    if definition["reaction"] == "projection" {
        // Public input never forwards private fixture admission options.
        catabolic_store::filesystem_mutation::require_fallback_fixture(&json!({}))?;
    }
    if definition["plan"]["kind"] != "query"
        || !matches!(definition["reaction"].as_str(), Some("report" | "event"))
    {
        return Err(Error("native public watcher run currently requires a query plan with report or event reaction; other run families require a borrowed runtime owner".into()));
    }
    let now = args
        .get("now")
        .map(|v| {
            v.as_f64()
                .filter(|v| v.is_finite())
                .ok_or_else(|| Error("invalid watcher clock".into()))
        })
        .transpose()?
        .unwrap_or_else(crate::media::now);
    session.with_evaluation(profile, &json!({}), 60_000, 100_000, |evaluation| {
        watcher::run_owned(
            &mut Evaluated(evaluation),
            profile,
            name,
            args["trigger"].as_str().unwrap_or("manual"),
            now,
            &mut |owner, request| {
                let mut request = request.clone();
                request["isolate"] = json!(true);
                crate::observation::execute(owner, profile, "scan", &request)
            },
            &mut |owner, context| {
                let evaluated =
                    catabolic_store::plans::evaluate(owner.0, &context.definition["plan"]);
                let result = (|| {
                    catabolic_store::public_watchers::resume_interrupts(owner)?;
                    let evaluated = evaluated?;
                    watcher::complete_evaluation(
                        owner,
                        profile,
                        &context,
                        &evaluated,
                        &mut |_, _, _, _| Err(Error("unexpected watcher effect".into())),
                    )
                })();
                if result.is_err() {
                    // Cancellation stops completion, then the existing run
                    // owner must be allowed to persist its failure cleanup.
                    owner.db.progress_handler(0, None::<fn() -> bool>)?;
                }
                result
            },
        )
    })
}
