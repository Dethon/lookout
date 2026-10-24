//! A thin Microsoft Graph client: authorization, throttling, paging.

use std::sync::Arc;
use std::time::Duration;

use reqwest::{Method, StatusCode, header};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Error, Result, Session};

pub const BASE: &str = "https://graph.microsoft.com/v1.0";
// Throttled or busy responses are retried this often before giving up.
const RETRIES: u32 = 4;
// Used when a throttled response doesn't say how long to wait.
const DEFAULT_BACKOFF: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
// Ids that don't change when a message moves between folders, so the cache
// sees a move rather than a delete and a new message. HTML bodies, and dates
// in UTC.
const PREFER: &str = "IdType=\"ImmutableId\", outlook.body-content-type=\"html\", outlook.timezone=\"UTC\"";
// Small pages keep each response, and so the memory parsing it, small.
const PAGE_SIZE: &str = "odata.maxpagesize=100";

/// One account's connection to Graph.
#[derive(Clone)]
pub struct Graph {
    http: reqwest::Client,
    session: Arc<Session>,
    base: Arc<str>,
}

/// One page of a collection or a delta query.
#[derive(Debug, serde::Deserialize)]
pub struct Page<T> {
    #[serde(default = "Vec::new")]
    pub value: Vec<T>,
    #[serde(rename = "@odata.nextLink")]
    pub next_link: Option<String>,
    #[serde(rename = "@odata.deltaLink")]
    pub delta_link: Option<String>,
}

#[derive(serde::Deserialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(serde::Deserialize)]
struct ErrorDetail {
    code: String,
    message: String,
}

impl Graph {
    pub fn new(http: reqwest::Client, session: Arc<Session>) -> Self {
        Self::with_base(http, session, BASE)
    }

    /// A client for another endpoint, such as a test server.
    pub fn with_base(http: reqwest::Client, session: Arc<Session>, base: &str) -> Self {
        Self { http, session, base: base.into() }
    }

    pub fn account_id(&self) -> &str {
        self.session.account_id()
    }

    /// `path` is relative to the v1.0 root, or a full link Graph returned.
    fn url(&self, path: &str) -> String {
        if path.starts_with("https://") || path.starts_with("http://") {
            path.to_owned()
        } else {
            format!("{}{path}", self.base)
        }
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        content_type: &str,
        paged: bool,
    ) -> Result<reqwest::Response> {
        let url = self.url(path);
        let mut renewed = false;
        let mut attempt = 0;
        loop {
            let token = self.session.access_token().await?;
            let mut prefer = PREFER.to_owned();
            if paged {
                prefer.push_str(", ");
                prefer.push_str(PAGE_SIZE);
            }
            let mut request = self
                .http
                .request(method.clone(), &url)
                .bearer_auth(token)
                .header("Prefer", prefer);
            if let Some(body) = &body {
                request = request.header(header::CONTENT_TYPE, content_type).body(body.clone());
            } else if method == Method::POST {
                request = request.header(header::CONTENT_LENGTH, 0);
            }
            let response = request.send().await?;
            let status = response.status();
            if status.is_success() {
                return Ok(response);
            }
            if status == StatusCode::UNAUTHORIZED && !renewed {
                renewed = true;
                self.session.invalidate().await;
                continue;
            }
            let retryable = matches!(
                status,
                StatusCode::TOO_MANY_REQUESTS
                    | StatusCode::SERVICE_UNAVAILABLE
                    | StatusCode::GATEWAY_TIMEOUT
                    | StatusCode::BAD_GATEWAY
            );
            if retryable && attempt < RETRIES {
                attempt += 1;
                let wait = response
                    .headers()
                    .get(header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse().ok())
                    .map(Duration::from_secs)
                    .unwrap_or(DEFAULT_BACKOFF * attempt)
                    .min(MAX_BACKOFF);
                log::info!("{status} from {method} {path}; retrying in {wait:?}");
                tokio::time::sleep(wait).await;
                continue;
            }
            return Err(error_from(response).await);
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self.send(Method::GET, path, None, "", false).await?;
        Ok(serde_json::from_slice(&response.bytes().await?)?)
    }

    /// One page of a collection or delta query; follow `next_link` for more.
    pub async fn page<T: DeserializeOwned>(&self, path: &str) -> Result<Page<T>> {
        let response = self.send(Method::GET, path, None, "", true).await?;
        Ok(serde_json::from_slice(&response.bytes().await?)?)
    }

    /// Every item of a small collection, all pages.
    pub async fn all<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<T>> {
        let mut items = Vec::new();
        let mut next = Some(path.to_owned());
        while let Some(link) = next {
            let page: Page<T> = self.page(&link).await?;
            items.extend(page.value);
            next = page.next_link;
        }
        Ok(items)
    }

    pub async fn bytes(&self, path: &str) -> Result<Vec<u8>> {
        let response = self.send(Method::GET, path, None, "", false).await?;
        Ok(response.bytes().await?.to_vec())
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let response = self
            .send(Method::POST, path, Some(serde_json::to_vec(body)?), "application/json", false)
            .await?;
        Ok(serde_json::from_slice(&response.bytes().await?)?)
    }

    /// A POST whose answer has no body, such as `send` or `accept`.
    pub async fn post_empty<B: Serialize>(&self, path: &str, body: Option<&B>) -> Result<()> {
        let body = body.map(serde_json::to_vec).transpose()?;
        self.send(Method::POST, path, body, "application/json", false).await?;
        Ok(())
    }

    pub async fn patch<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let response = self
            .send(Method::PATCH, path, Some(serde_json::to_vec(body)?), "application/json", false)
            .await?;
        Ok(serde_json::from_slice(&response.bytes().await?)?)
    }

    pub async fn patch_empty<B: Serialize>(&self, path: &str, body: &B) -> Result<()> {
        self.send(Method::PATCH, path, Some(serde_json::to_vec(body)?), "application/json", false)
            .await?;
        Ok(())
    }

    pub async fn delete(&self, path: &str) -> Result<()> {
        self.send(Method::DELETE, path, None, "", false).await?;
        Ok(())
    }

    /// Uploads a large attachment in chunks to the session `upload_url`.
    /// The upload URL carries its own authorization.
    pub async fn upload(&self, upload_url: &str, bytes: &[u8]) -> Result<()> {
        // Multiples of 320 KiB, as the upload service asks.
        const CHUNK: usize = 320 * 1024 * 10;
        let total = bytes.len();
        for (index, chunk) in bytes.chunks(CHUNK).enumerate() {
            let start = index * CHUNK;
            let end = start + chunk.len() - 1;
            let response = self
                .http
                .put(upload_url)
                .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}"))
                .header(header::CONTENT_LENGTH, chunk.len())
                .body(chunk.to_vec())
                .send()
                .await?;
            if !response.status().is_success() {
                return Err(error_from(response).await);
            }
        }
        Ok(())
    }
}

async fn error_from(response: reqwest::Response) -> Error {
    let status = response.status().as_u16();
    let body = response.bytes().await.unwrap_or_default();
    match serde_json::from_slice::<ErrorBody>(&body) {
        Ok(ErrorBody { error }) => Error::Graph { status, code: error.code, message: error.message },
        Err(_) => Error::Graph {
            status,
            code: String::new(),
            message: String::from_utf8_lossy(&body).chars().take(200).collect(),
        },
    }
}

/// Percent-encodes a value for a query string or a path segment.
pub fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

/// A shared HTTP client: one connection pool for every account.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("Lookout/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(2)
        .build()
        .expect("the HTTP client builds")
}
