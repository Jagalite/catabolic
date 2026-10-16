// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Explicit effect boundaries for native Catabolic workflows.
pub mod maintenance;
pub mod media;
pub mod notifications;
pub mod process;
pub mod projection;
pub mod public_watchers;

pub mod api;
pub mod fallback;
pub mod helper;
pub mod http_worker;
pub mod imports;
pub mod legacy_refresh;

pub mod rendering;

pub mod document;

pub mod artifacts;
pub mod catalog_refresh;
pub mod consumers;
pub mod destination_mappings;
pub mod observation;
pub mod playback;
pub mod processors;
pub mod receipts;
pub mod renditions;
pub mod rules;
pub mod watchers;

pub mod component_packaging;

mod packet_verification;

mod component_mux;

pub mod plex_import;

pub mod plex_login;

pub mod plex_matching;

pub mod plex_metadata;

pub mod public_scan;

pub mod public_processing;

pub mod public_artifacts;

pub mod source_events;

pub mod public_fallback;

pub mod identify;

pub mod operations;

pub mod host;

pub mod scoped_host;

mod helper_fence;

mod execution_cleanup;
