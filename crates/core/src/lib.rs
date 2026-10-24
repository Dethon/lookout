//! Lookout's engine: signing in to Microsoft accounts, talking to Microsoft
//! Graph, and keeping mail, calendars and contacts in a local SQLite cache.

pub mod auth;
pub mod config;
pub mod graph;
pub mod calendar;
pub mod html;
pub mod mail;
pub mod people;
pub mod release;
pub mod model;
mod secrets;
pub mod store;
pub mod sync;
pub mod when;

#[cfg(test)]
mod mock_tests;

pub use auth::{Account, Session};
pub use config::Config;
pub use graph::Graph;
pub use store::Store;
pub use reqwest;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no folder for application data")]
    NoDataDir,
    #[error("sign-in failed: {0}")]
    Auth(String),
    #[error("the account needs to sign in again")]
    SignInRequired,
    #[error("network: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Microsoft Graph {status}: {code}: {message}")]
    Graph {
        status: u16,
        code: String,
        message: String,
    },
    #[error("cache: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("unexpected response: {0}")]
    Json(#[from] serde_json::Error),
    #[error("secret storage: {0}")]
    Secret(String),
}

impl Error {
    /// A resource that is gone on the server: a message deleted elsewhere,
    /// or a delta link that expired.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Error::Graph { status: 404, .. })
    }

    /// The delta link is too old and the sync has to start over.
    pub fn is_sync_reset(&self) -> bool {
        matches!(self, Error::Graph { status: 410, .. })
            || matches!(self, Error::Graph { code, .. } if code.contains("syncStateNotFound") || code.contains("resyncRequired"))
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
