// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
use axum::{
    body::Body,
    http::{Response, StatusCode},
};
use serde_json::json;
#[derive(Debug)]
pub struct Problem {
    pub code: String,
    pub status: u16,
    pub headers: Vec<(String, String)>,
}
pub type Result<T> = std::result::Result<T, Problem>;
impl Problem {
    pub fn new(code: &str, status: u16) -> Self {
        Self {
            code: code.into(),
            status,
            headers: vec![],
        }
    }
    pub fn response(self) -> Response<Body> {
        let id = uuid::Uuid::new_v4().to_string();
        let large = self.status == 413;
        let value = json!({"type":if large{"about:blank".into()}else{format!("urn:catabolic:problem:{}",self.code)},"title":if large{self.code.clone()}else{self.code.replace('_'," ")},"status":self.status,"code":self.code,"request_id":id,"retryable":matches!(self.status,429|503),"remediation":if large{"Reduce the request body to at most 1 MiB."}else if matches!(self.status,429|503){"retry_later"}else{"inspect_request_and_permissions"}});
        let mut r = Response::builder()
            .status(StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
            .header("Content-Type", "application/problem+json")
            .header("Cache-Control", "no-store")
            .header("X-Request-ID", id);
        if self.status == 401 {
            r = r.header("WWW-Authenticate", "Bearer")
        }
        if matches!(self.status, 429 | 503) {
            r = r.header("Retry-After", "1")
        }
        for (k, v) in self.headers {
            r = r.header(k, v)
        }
        r.body(Body::from(serde_json::to_vec(&value).unwrap()))
            .unwrap()
    }
}
impl From<catabolic_store::Error> for Problem {
    fn from(e: catabolic_store::Error) -> Self {
        let code = e.0.as_str();
        if code.contains("another Catabolic writer")
            || code.contains("database is locked")
            || code.contains("database is busy")
        {
            return Self::new("catalog_busy", 503);
        }
        if code == "authorized_catalog_reinitialized" {
            return Self::new("catalog_error", 500);
        }
        if code == "stale_resolution_plan" {
            return Self::new("catalog_error", 409);
        }
        let status = match code {
            "unauthorized" => 401,
            "not_found" | "unknown_principal" => 404,
            "forbidden"
            | "read_only"
            | "source_not_exposed"
            | "private_content"
            | "operation_not_approved"
            | "incomplete_authorization_selection"
            | "playback_content_not_authorized"
            | "callback_origin_not_allowed" => 403,
            "invalid_cursor"
            | "invalid_page"
            | "invalid_rendition_request"
            | "expected_plan_required"
            | "invalid_callback_url"
            | "invalid_webhook_events"
            | "invalid_playback_request"
            | "invalid_idempotency_key"
            | "grant_requires_complete_item_selection"
            | "invalid_callback_origin"
            | "invalid_grant"
            | "invalid_grant_fields"
            | "invalid_principal"
            | "invalid_revisions"
            | "invalid_token_configuration"
            | "invalid_token_grant" => 400,
            "playback_session_expired" | "playback_session_cancelled" => 410,
            "request_limit"
            | "ticket_limit"
            | "ticket_transfer_limit"
            | "webhook_limit"
            | "stream_limit"
            | "rate_limit"
            | "playback_session_limit" => 429,
            "range_not_satisfiable" | "playback_segment_range_unsupported" => 416,
            "precondition_failed" => 412,
            "invalid_request"
            | "selection_incomplete"
            | "snapshot_limit"
            | "authorization_scope_too_large"
            | "operator_plan_limit"
            | "invalid_definition"
            | "exact_and_logical_selection_conflict" => 422,
            _ => 409,
        };
        let safe = !code.is_empty()
            && code
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        Self::new(if safe { code } else { "catalog_error" }, status)
    }
}
impl From<std::io::Error> for Problem {
    fn from(_: std::io::Error) -> Self {
        Self::new("catalog_error", 409)
    }
}

impl From<rusqlite::Error> for Problem {
    fn from(e: rusqlite::Error) -> Self {
        catabolic_store::Error::from(e).into()
    }
}
