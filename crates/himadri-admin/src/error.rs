//! Typed error for the admin CRUD surface (keys, models, endpoints).
//!
//! Replaces the previous `Result<T, String>` / `Option<T>` returns that
//! collapsed every failure into "not found" or a generic 500. Each variant
//! corresponds to one HTTP status, mapped once at the HTTP layer:
//!
//! | Variant      | Meaning                                   | HTTP |
//! |--------------|-------------------------------------------|------|
//! | `NotFound`   | no row with that id                       | 404  |
//! | `Validation` | input rejected before touching the store  | 400  |
//! | `Conflict`   | a state guard blocked the operation       | 409  |
//! | `Store`      | the backing store failed                  | 500  |
//!
//! `Validation`/`Conflict` messages are written for the client and safe to
//! return verbatim; `Store` messages may carry backend detail and should be
//! logged, with only a generic message sent to the client.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdminError {
    /// No row with the given id (also covers ids that cannot possibly match,
    /// e.g. a malformed UUID on the Postgres backend).
    #[error("not found")]
    NotFound,
    /// Invalid input, rejected before touching the store (e.g. an endpoint
    /// `base_url` that fails the SSRF guard).
    #[error("{0}")]
    Validation(String),
    /// A state guard blocked the operation (e.g. deleting a model that is
    /// still enabled).
    #[error("{0}")]
    Conflict(String),
    /// The backing store failed — not client-fixable.
    #[error("storage error: {0}")]
    Store(String),
}

#[cfg(any(feature = "sqlite", feature = "postgres"))]
impl From<sqlx::Error> for AdminError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => AdminError::NotFound,
            other => AdminError::Store(other.to_string()),
        }
    }
}
