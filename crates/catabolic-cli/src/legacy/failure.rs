// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Provenance-bearing failures at the legacy command boundary.
#[derive(Debug)]
pub enum Failure {
    Detailed(catabolic_store::access_admin::Failure),
    Json(super::json_diagnostic::JsonError),
}
pub type Result<T> = std::result::Result<T, Failure>;
impl From<catabolic_store::Error> for Failure {
    fn from(error: catabolic_store::Error) -> Self {
        Self::Detailed(error.into())
    }
}
impl From<catabolic_store::access_admin::Failure> for Failure {
    fn from(error: catabolic_store::access_admin::Failure) -> Self {
        Self::Detailed(error)
    }
}
impl From<super::json_diagnostic::JsonError> for Failure {
    fn from(error: super::json_diagnostic::JsonError) -> Self {
        Self::Json(error)
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Detailed(error) => error.fmt(f),
            Self::Json(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for Failure {}
