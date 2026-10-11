// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
use crate::problem::{Problem, Result};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
#[derive(Default)]
struct Counts {
    active: BTreeMap<String, usize>,
    requests: BTreeMap<String, (Instant, usize)>,
}
pub struct Limits {
    state: Mutex<Counts>,
    pub streams: usize,
    pub per_principal: usize,
    pub rate: usize,
}
pub struct StreamGuard {
    limits: Arc<Limits>,
    principal: String,
}
impl Drop for StreamGuard {
    fn drop(&mut self) {
        let mut s = self.limits.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = s.active.get_mut(&self.principal) {
            *n -= 1;
            if *n == 0 {
                s.active.remove(&self.principal);
            }
        }
    }
}
impl Limits {
    pub fn new(streams: usize, per_principal: usize, rate: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(Counts::default()),
            streams,
            per_principal,
            rate,
        })
    }
    pub fn request(&self, principal: &str) -> Result<()> {
        let now = Instant::now();
        let mut s = self
            .state
            .lock()
            .map_err(|_| Problem::new("catalog_error", 500))?;
        s.requests
            .retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(60));
        let (_, count) = s.requests.entry(principal.into()).or_insert((now, 0));
        if *count >= self.rate {
            return Err(Problem::new("rate_limit", 429));
        }
        *count += 1;
        Ok(())
    }
    pub fn admit(self: &Arc<Self>, principal: &str) -> Result<StreamGuard> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| Problem::new("catalog_error", 500))?;
        if s.active.values().sum::<usize>() >= self.streams
            || s.active.get(principal).copied().unwrap_or(0) >= self.per_principal
        {
            return Err(Problem::new("stream_limit", 429));
        }
        *s.active.entry(principal.into()).or_default() += 1;
        Ok(StreamGuard {
            limits: self.clone(),
            principal: principal.into(),
        })
    }
}
