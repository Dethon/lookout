//! Signing in with the Microsoft identity platform: the authorization code
//! flow with PKCE, answered on a loopback port, as a public client. Works
//! for personal accounts and work or school ones alike (`common`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use crate::{Error, Result, secrets};

const AUTHORITY: &str = "https://login.microsoftonline.com/common/oauth2/v2.0";
const SCOPES: &str = "offline_access openid profile User.Read Mail.ReadWrite Mail.Send \
                      Calendars.ReadWrite Contacts.ReadWrite MailboxSettings.Read";
// Writing inbox rules, as blocking a sender does. Asked for the first time a
// sender is blocked, not at sign-in.
const RULES_SCOPE: &str = "MailboxSettings.ReadWrite";
// Renewed this long before it expires, so a request never carries a token
// that runs out on the way.
const EXPIRY_MARGIN: Duration = Duration::from_secs(300);
// How long the browser has to come back with a code.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(600);
const DONE_PAGE: &str = "<!doctype html><html><head><meta charset=utf-8><title>Lookout</title></head>\
    <body style=\"font-family:sans-serif;text-align:center;margin-top:20vh\">\
    <h1>Done</h1><p>You can close this tab and return to Lookout.</p></body></html>";
const FAILED_PAGE: &str = "<!doctype html><html><head><meta charset=utf-8><title>Lookout</title></head>\
    <body style=\"font-family:sans-serif;text-align:center;margin-top:20vh\">\
    <h1>Sign-in didn't finish</h1><p>Return to Lookout and try again.</p></body></html>";

/// A signed-in mailbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// The user's Graph id; names the account in the cache.
    pub id: String,
    pub name: String,
    pub email: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct TokenError {
    error: String,
    #[serde(default)]
    error_description: String,
}

struct Tokens {
    access: String,
    expires: Instant,
    refresh: String,
}

/// Hands out access tokens for one account, renewing them as they expire.
/// The lock means requests running together renew only once.
pub struct Session {
    client_id: String,
    account: String,
    http: reqwest::Client,
    tokens: Mutex<Option<Tokens>>,
    /// Inbox rules may be written: the tokens carry `RULES_SCOPE`.
    rules: AtomicBool,
    /// Held while asking for `RULES_SCOPE`, so the browser opens once.
    asking: Mutex<()>,
}

impl Session {
    /// The session saved by an earlier sign-in, if its refresh token is kept.
    pub fn restore(http: reqwest::Client, client_id: &str, account: &str) -> Option<Self> {
        let refresh = secrets::load(account)?;
        Some(Self {
            client_id: client_id.to_owned(),
            account: account.to_owned(),
            http,
            tokens: Mutex::new(Some(Tokens {
                access: String::new(),
                expires: Instant::now(),
                refresh,
            })),
            rules: AtomicBool::new(false),
            asking: Mutex::new(()),
        })
    }

    fn scopes(&self) -> String {
        if self.rules.load(Ordering::Relaxed) { format!("{SCOPES} {RULES_SCOPE}") } else { SCOPES.to_owned() }
    }

    /// A token valid for a few more minutes at least.
    pub async fn access_token(&self) -> Result<String> {
        let mut tokens = self.tokens.lock().await;
        let current = tokens.as_ref().ok_or(Error::SignInRequired)?;
        if !current.access.is_empty() && current.expires > Instant::now() + EXPIRY_MARGIN {
            return Ok(current.access.clone());
        }
        let refreshed = self.redeem_refresh(&current.refresh, &self.scopes()).await;
        match refreshed {
            Ok(fresh) => {
                let access = fresh.access.clone();
                // The service may rotate the refresh token; keep the newest.
                if fresh.refresh != current.refresh {
                    secrets::store(&self.account, &fresh.refresh)?;
                }
                *tokens = Some(fresh);
                Ok(access)
            }
            Err(Error::SignInRequired) => {
                *tokens = None;
                Err(Error::SignInRequired)
            }
            Err(other) => Err(other),
        }
    }

    /// Makes the next request renew its token, after the service turned the
    /// current one down.
    pub async fn invalidate(&self) {
        if let Some(tokens) = self.tokens.lock().await.as_mut() {
            tokens.access.clear();
        }
    }

    /// Lets this session write inbox rules: at once if the permission was
    /// granted before, here or on another device; otherwise in the browser,
    /// for `email`, calling `asking` as it opens.
    pub async fn allow_rules(&self, email: &str, asking: impl FnOnce()) -> Result<()> {
        if self.rules.load(Ordering::Relaxed) {
            return Ok(());
        }
        let _asking = self.asking.lock().await;
        if self.rules.load(Ordering::Relaxed) {
            return Ok(());
        }
        let scopes = format!("{SCOPES} {RULES_SCOPE}");
        {
            let mut tokens = self.tokens.lock().await;
            let refresh = tokens.as_ref().ok_or(Error::SignInRequired)?.refresh.clone();
            match self.redeem_refresh(&refresh, &scopes).await {
                Ok(fresh) => {
                    if fresh.refresh != refresh {
                        secrets::store(&self.account, &fresh.refresh)?;
                    }
                    *tokens = Some(fresh);
                    self.rules.store(true, Ordering::Relaxed);
                    return Ok(());
                }
                // Not granted yet (consent_required); the refresh token
                // itself still works.
                Err(Error::SignInRequired | Error::Auth(_)) => {}
                Err(other) => return Err(other),
            }
        }
        asking();
        let fresh = authorize(&self.http, &self.client_id, &scopes, ("login_hint", email)).await?;
        if who(&self.http, &fresh.access).await?.id != self.account {
            return Err(Error::Auth(format!("approved for another account than {email}")));
        }
        secrets::store(&self.account, &fresh.refresh)?;
        *self.tokens.lock().await = Some(fresh);
        self.rules.store(true, Ordering::Relaxed);
        Ok(())
    }

    async fn redeem_refresh(&self, refresh: &str, scopes: &str) -> Result<Tokens> {
        let form = [
            ("client_id", self.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("scope", scopes),
        ];
        let response = self
            .http
            .post(format!("{AUTHORITY}/token"))
            .form(&form)
            .send()
            .await?;
        tokens_from(response, Some(refresh)).await
    }

    pub fn account_id(&self) -> &str {
        &self.account
    }

    /// A session with a token that never runs out, for tests.
    #[cfg(test)]
    pub(crate) fn fixed(http: reqwest::Client, account: &str, access: &str) -> Self {
        Self {
            client_id: String::new(),
            account: account.to_owned(),
            http,
            tokens: Mutex::new(Some(Tokens {
                access: access.to_owned(),
                expires: Instant::now() + Duration::from_secs(86400),
                refresh: String::new(),
            })),
            rules: AtomicBool::new(true),
            asking: Mutex::new(()),
        }
    }
}

async fn tokens_from(response: reqwest::Response, old_refresh: Option<&str>) -> Result<Tokens> {
    let status = response.status();
    let body = response.bytes().await?;
    if !status.is_success() {
        let error: TokenError = serde_json::from_slice(&body).unwrap_or(TokenError {
            error: status.to_string(),
            error_description: String::from_utf8_lossy(&body).into_owned(),
        });
        // A refresh token that was revoked, expired or lost its consent.
        if error.error == "invalid_grant" || error.error == "interaction_required" {
            return Err(Error::SignInRequired);
        }
        return Err(Error::Auth(format!(
            "{}: {}",
            error.error,
            error.error_description.lines().next().unwrap_or_default()
        )));
    }
    let token: TokenResponse = serde_json::from_slice(&body)?;
    let refresh = token
        .refresh_token
        .or_else(|| old_refresh.map(str::to_owned))
        .ok_or_else(|| Error::Auth("no refresh token was issued".into()))?;
    Ok(Tokens {
        access: token.access_token,
        expires: Instant::now() + Duration::from_secs(token.expires_in),
        refresh,
    })
}

/// Signs in a new account in the browser. Returns its session and who it is.
pub async fn sign_in(http: reqwest::Client, client_id: &str) -> Result<(Session, Account)> {
    let tokens = authorize(&http, client_id, SCOPES, ("prompt", "select_account")).await?;
    let me = who(&http, &tokens.access).await?;
    let account = Account {
        id: me.id,
        email: me.mail.filter(|mail| !mail.is_empty()).unwrap_or(me.user_principal_name),
        name: me.display_name.unwrap_or_default(),
    };
    secrets::store(&account.id, &tokens.refresh)?;
    let session = Session {
        client_id: client_id.to_owned(),
        account: account.id.clone(),
        http,
        tokens: Mutex::new(Some(tokens)),
        rules: AtomicBool::new(false),
        asking: Mutex::new(()),
    };
    Ok((session, account))
}

/// Who the token is for.
async fn who(http: &reqwest::Client, access: &str) -> Result<Me> {
    Ok(http
        .get(format!("{}/me?$select=id,displayName,mail,userPrincipalName", crate::graph::BASE))
        .bearer_auth(access)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

/// Asks for `scopes` in the browser and redeems the code it comes back
/// with; `hint` is one more query pair, such as the account to choose.
async fn authorize(http: &reqwest::Client, client_id: &str, scopes: &str, hint: (&str, &str)) -> Result<Tokens> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let redirect = format!("http://localhost:{port}");
    let verifier = random_token();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_token();

    let mut url = url::Url::parse(&format!("{AUTHORITY}/authorize")).expect("a valid URL");
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &redirect)
        .append_pair("response_mode", "query")
        .append_pair("scope", scopes)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .append_pair(hint.0, hint.1);
    open::that_detached(url.as_str()).map_err(|error| Error::Auth(format!("opening the browser: {error}")))?;

    let code = tokio::time::timeout(SIGN_IN_TIMEOUT, receive_code(&listener, &state))
        .await
        .map_err(|_| Error::Auth("timed out waiting for the browser".into()))??;

    let form = [
        ("client_id", client_id),
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("code_verifier", verifier.as_str()),
        ("scope", scopes),
    ];
    let response = http.post(format!("{AUTHORITY}/token")).form(&form).send().await?;
    tokens_from(response, None).await
}

/// Forgets an account's refresh token.
pub fn sign_out(account: &str) {
    secrets::delete(account);
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Me {
    id: String,
    display_name: Option<String>,
    mail: Option<String>,
    user_principal_name: String,
}

/// Answers the browser's redirects until one carries the code for `state`.
async fn receive_code(listener: &TcpListener, state: &str) -> Result<String> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut buffer = vec![0; 8192];
        let mut read = 0;
        // The request line is all that is needed; it comes in the first packet or two.
        while read < buffer.len() {
            let n = stream.read(&mut buffer[read..]).await?;
            if n == 0 {
                break;
            }
            read += n;
            if buffer[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let request = String::from_utf8_lossy(&buffer[..read]);
        let outcome = parse_redirect(&request, state);
        let page = if matches!(outcome, Some(Ok(_))) { DONE_PAGE } else { FAILED_PAGE };
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        );
        _ = stream.write_all(response.as_bytes()).await;
        _ = stream.shutdown().await;
        match outcome {
            Some(result) => return result,
            // A favicon request or similar.
            None => continue,
        }
    }
}

/// The code in a redirect's request line; `None` for requests that aren't
/// the redirect.
fn parse_redirect(request: &str, state: &str) -> Option<Result<String>> {
    let target = request.lines().next()?.split_whitespace().nth(1)?;
    let url = url::Url::parse(&format!("http://localhost{target}")).ok()?;
    if url.path() != "/" {
        return None;
    }
    let mut code = None;
    let mut returned_state = None;
    let mut error = None;
    for (key, value) in url.query_pairs() {
        match &*key {
            "code" => code = Some(value.into_owned()),
            "state" => returned_state = Some(value.into_owned()),
            "error_description" => error = Some(value.into_owned()),
            "error" if error.is_none() => error = Some(value.into_owned()),
            _ => {}
        }
    }
    if let Some(error) = error {
        return Some(Err(Error::Auth(error)));
    }
    if returned_state.as_deref() != Some(state) {
        return Some(Err(Error::Auth("the sign-in response didn't match the request".into())));
    }
    code.map(Ok)
}

fn random_token() -> String {
    let bytes: [u8; 32] = rand::random();
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_comes_from_the_redirect() {
        let request = "GET /?code=abc123&state=xyz HTTP/1.1\r\nHost: localhost\r\n\r\n";
        assert_eq!(parse_redirect(request, "xyz").unwrap().unwrap(), "abc123");
    }

    #[test]
    fn a_mismatched_state_is_refused() {
        let request = "GET /?code=abc123&state=other HTTP/1.1\r\n\r\n";
        assert!(parse_redirect(request, "xyz").unwrap().is_err());
    }

    #[test]
    fn errors_are_reported() {
        let request = "GET /?error=access_denied&error_description=User+cancelled HTTP/1.1\r\n\r\n";
        let Err(Error::Auth(message)) = parse_redirect(request, "xyz").unwrap() else {
            panic!("expected an error");
        };
        assert_eq!(message, "User cancelled");
    }

    #[test]
    fn other_requests_are_ignored() {
        assert!(parse_redirect("GET /favicon.ico HTTP/1.1\r\n\r\n", "xyz").is_none());
    }
}
