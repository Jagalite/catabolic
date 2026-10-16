// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Provenance-bearing failures at the legacy command boundary.
#[derive(Debug)]
pub enum Failure {
    Detailed(catabolic_store::access_admin::Failure),
    Boundary(catabolic_store::DetailedError),
    Json(super::json_diagnostic::JsonError),
    Variable(super::variable_admission::AdmissionError),
}
pub type Result<T> = std::result::Result<T, Failure>;
impl From<catabolic_store::Error> for Failure {
    fn from(error: catabolic_store::Error) -> Self {
        Self::Detailed(error.into())
    }
}
impl From<catabolic_store::DetailedError> for Failure {
    fn from(error: catabolic_store::DetailedError) -> Self {
        Self::Boundary(error)
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
impl From<super::variable_admission::AdmissionError> for Failure {
    fn from(error: super::variable_admission::AdmissionError) -> Self {
        Self::Variable(error)
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Detailed(error) => error.fmt(f),
            Self::Boundary(error) => error.fmt(f),
            Self::Json(error) => error.fmt(f),
            Self::Variable(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for Failure {}
