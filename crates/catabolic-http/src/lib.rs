// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Opt-in authenticated contract 1.7.0 transport. Never a Python fallback.
pub mod asgi;
mod content;
mod events;
mod limits;
mod models;
mod problem;
mod routes;
mod server;
pub use server::{ServerConfig, serve, serve_listener};

mod worker;
pub use worker::{
    InterruptSignal, InterruptibleOutcome, interruptible, interruptible_detailed,
    interruptible_detailed_with_signal, interruptible_with_signal, worker,
};
