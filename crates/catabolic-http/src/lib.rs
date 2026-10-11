// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Opt-in authenticated contract 1.7.0 transport. Never a Python fallback.
mod content;
mod events;
mod limits;
mod models;
mod problem;
mod routes;
mod server;
pub use server::{ServerConfig, serve, serve_listener};

mod worker;
pub use worker::{interruptible, worker};
