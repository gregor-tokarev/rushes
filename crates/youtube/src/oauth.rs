//! Installed-app OAuth: loopback redirect on `127.0.0.1:<random port>` with PKCE.

use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ureq::Agent;
use url::Url;

use crate::config::{SCOPES, YoutubeConfig};
use crate::error::{Error, Result, api_error, oauth_error};
use crate::http::{self, HttpResponse, body_text};

const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Budget for reading one request. Browsers open speculative connections that never
/// send anything; one of them must not hold up the real redirect (or cancel) for long.
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_REQUEST_HEAD: usize = 16 * 1024;
/// Budget of [`check_client`]: it runs while the creator waits for the browser.
const CLIENT_CHECK_TIMEOUT: Duration = Duration::from_secs(15);

/// OAuth tokens for one Google account. `Debug` never prints the tokens.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: DateTime<Utc>,
    /// Space-separated scopes granted by the user.
    pub scope: String,
    pub id_token: Option<String>,
}

impl Credentials {
    /// True when the access token is expired or expires within `margin`.
    pub fn expires_within(&self, margin: Duration) -> bool {
        let margin = TimeDelta::from_std(margin).unwrap_or(TimeDelta::MAX);
        Utc::now().checked_add_signed(margin).is_none_or(|limit| self.expires_at <= limit)
    }

    pub fn has_scope(&self, scope: &str) -> bool {
        self.scope.split_whitespace().any(|granted| granted == scope)
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "<redacted>"))
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .field("id_token", &self.id_token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
    refresh_token: Option<String>,
    scope: Option<String>,
    id_token: Option<String>,
}

impl TokenResponse {
    fn into_credentials(self, previous: Option<&Credentials>) -> Credentials {
        // Google issues one-hour tokens; the clamp keeps a bogus value from overflowing.
        let lifetime = TimeDelta::try_seconds(self.expires_in.clamp(0, 365 * 24 * 3600)).unwrap_or_default();
        Credentials {
            access_token: self.access_token,
            refresh_token: self.refresh_token.or_else(|| previous.and_then(|p| p.refresh_token.clone())),
            expires_at: Utc::now() + lifetime,
            scope: self.scope.or_else(|| previous.map(|p| p.scope.clone())).unwrap_or_default(),
            id_token: self.id_token.or_else(|| previous.and_then(|p| p.id_token.clone())),
        }
    }
}

/// A sign-in in progress: the loopback listener is bound and the consent URL is
/// ready to open in the browser.
pub struct PendingAuthorization {
    listener: TcpListener,
    config: YoutubeConfig,
    redirect_uri: String,
    state: String,
    verifier: String,
    authorize_url: String,
}

/// Binds the loopback redirect listener and builds the consent URL.
pub fn begin_authorization(config: &YoutubeConfig) -> Result<PendingAuthorization> {
    if config.client_id.trim().is_empty() {
        return Err(Error::InvalidInput("the OAuth client id is empty".to_owned()));
    }
    // Google's token endpoint rejects Desktop clients without their secret, PKCE or not.
    if config.is_google() && config.client_secret.is_none() {
        return Err(Error::InvalidInput(
            "Google also needs the OAuth client secret of the Desktop app client".to_owned(),
        ));
    }
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| Error::io("binding the sign-in redirect port", e))?;
    let port = listener.local_addr().map_err(|e| Error::io("reading the sign-in redirect port", e))?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}");
    let verifier = random_token(48)?;
    let state = random_token(24)?;
    let authorize_url = authorize_url(config, &redirect_uri, &state, &pkce_challenge(&verifier))?;
    Ok(PendingAuthorization { listener, config: config.clone(), redirect_uri, state, verifier, authorize_url })
}

impl PendingAuthorization {
    /// URL to open in the user's browser.
    pub fn authorize_url(&self) -> &str {
        &self.authorize_url
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Serves the browser redirect, then exchanges the code for tokens. Unrelated
    /// requests (favicon, probes) are answered and ignored. Checks `cancel` about
    /// every 50 ms. `Duration::MAX` waits until the redirect or cancel.
    pub fn wait(self, timeout: Duration, cancel: &AtomicBool) -> Result<Credentials> {
        self.listener.set_nonblocking(true).map_err(|e| Error::io("configuring the sign-in redirect listener", e))?;
        let deadline = Instant::now().checked_add(timeout);
        let agent = http::agent();
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled { session_url: None });
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(Error::Timeout);
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(outcome) = self.handle_connection(&agent, stream) {
                        return outcome;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(POLL_INTERVAL),
                Err(e) => return Err(Error::io("accepting the sign-in redirect", e)),
            }
        }
    }

    /// `None` when the connection was not the redirect of this sign-in.
    fn handle_connection(&self, agent: &Agent, mut stream: TcpStream) -> Option<Result<Credentials>> {
        let target = read_request_target(&stream)?;
        let url = Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
        let param = |name: &str| url.query_pairs().find(|(key, _)| key == name).map(|(_, value)| value.into_owned());
        let (code, error) = (param("code"), param("error"));
        if url.path() != "/" || (code.is_none() && error.is_none()) {
            respond(
                &mut stream,
                "404 Not Found",
                &page("Not found", "This address only completes the Rushes sign-in.", false),
            );
            return None;
        }

        // Any local process or web page can reach this port; only our state counts.
        if param("state").as_deref() != Some(self.state.as_str()) {
            respond(
                &mut stream,
                "400 Bad Request",
                &page(
                    "Sign-in link expired",
                    "This is not the sign-in Rushes is waiting for. Start again from Rushes.",
                    false,
                ),
            );
            return None;
        }

        let outcome = if let Some(error) = error {
            Err(match error.as_str() {
                "access_denied" => Error::AccessDenied,
                _ => Error::OAuth { error, description: param("error_description") },
            })
        } else {
            let code = code.unwrap_or_default();
            exchange_code(agent, &self.config, &code, &self.redirect_uri, &self.verifier)
        };

        let body = match &outcome {
            Ok(_) => page("You’re signed in", "Return to Rushes — you can close this tab.", true),
            Err(error) => page("Sign-in didn’t finish", &format!("{error}. Return to Rushes and try again."), false),
        };
        respond(&mut stream, "200 OK", &body);
        Some(outcome)
    }
}

fn authorize_url(config: &YoutubeConfig, redirect_uri: &str, state: &str, challenge: &str) -> Result<String> {
    let scope = SCOPES.join(" ");
    let url = Url::parse_with_params(
        &config.auth_url,
        [
            ("client_id", config.client_id.as_str()),
            ("redirect_uri", redirect_uri),
            ("response_type", "code"),
            ("scope", scope.as_str()),
            ("state", state),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("access_type", "offline"),
            ("prompt", "consent"),
            ("include_granted_scopes", "true"),
        ],
    )
    .map_err(|e| Error::InvalidInput(format!("invalid authorization URL {}: {e}", config.auth_url)))?;
    Ok(url.into())
}

/// URL-safe random string with `bytes` bytes of entropy (4/3 as many characters).
fn random_token(bytes: usize) -> Result<String> {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).map_err(|e| Error::io("generating random sign-in values", e.into()))?;
    Ok(URL_SAFE_NO_PAD.encode(buffer))
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Reads the request head (at most [`MAX_REQUEST_HEAD`] bytes within
/// [`REQUEST_READ_TIMEOUT`]) and returns the target of a GET, e.g. `/?code=...`.
fn read_request_target(stream: &TcpStream) -> Option<String> {
    stream.set_nonblocking(false).ok()?;
    stream.set_write_timeout(Some(REQUEST_READ_TIMEOUT)).ok()?;
    let deadline = Instant::now() + REQUEST_READ_TIMEOUT;
    let mut reader = BufReader::new(DeadlineReader { stream, deadline }.take(MAX_REQUEST_HEAD as u64));
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let mut parts = request_line.split_whitespace();
    let (method, target) = (parts.next()?, parts.next()?);
    // Drain the headers so the browser sees a clean response.
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if line.trim().is_empty() => break,
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    (method == "GET").then(|| target.to_owned())
}

/// Socket reader that gives up at `deadline`, however slowly bytes trickle in.
struct DeadlineReader<'a> {
    stream: &'a TcpStream,
    deadline: Instant,
}

impl Read for DeadlineReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(remaining))?;
        let mut stream = self.stream;
        stream.read(buf)
    }
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    // The browser may already have given up on the tab; the sign-in result stands either way.
    let _ = stream.write_all(response.as_bytes()).and_then(|()| stream.flush());
}

fn page(title: &str, message: &str, success: bool) -> String {
    let (accent, icon) = if success { ("#16a34a", "&#10003;") } else { ("#dc2626", "!") };
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} · Rushes</title>
<style>
  body {{ margin: 0; min-height: 100vh; display: grid; place-items: center; background: #f6f6f4;
         font: 15px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", Inter, sans-serif; color: #2f2f2f; }}
  main {{ background: #fff; border: 1px solid #e6e6e3; border-radius: 12px; padding: 40px 48px;
         text-align: center; max-width: 380px; box-shadow: 0 1px 3px rgba(0, 0, 0, .06); }}
  .icon {{ width: 44px; height: 44px; margin: 0 auto 16px; border-radius: 50%; background: {accent};
          color: #fff; display: grid; place-items: center; font-size: 22px; font-weight: 600; }}
  h1 {{ font-size: 20px; font-weight: 600; margin: 0 0 8px; }}
  p {{ margin: 0; color: #6b6b6b; }}
</style>
</head>
<body><main><div class="icon">{icon}</div><h1>{title}</h1><p>{message}</p></main></body>
</html>
"#,
        title = escape_html(title),
        message = escape_html(message),
    )
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn token_request(mut response: HttpResponse, previous: Option<&Credentials>) -> Result<Credentials> {
    let status = response.status().as_u16();
    if !response.status().is_success() {
        return Err(oauth_error(status, &body_text(&mut response)));
    }
    let token: TokenResponse = http::json(response)?;
    Ok(token.into_credentials(previous))
}

fn exchange_code(
    agent: &Agent,
    config: &YoutubeConfig,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<Credentials> {
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", config.client_id.as_str()),
        ("code_verifier", verifier),
    ];
    if let Some(secret) = &config.client_secret {
        form.push(("client_secret", secret));
    }
    token_request(agent.post(&config.token_url).send_form(form)?, None)
}

/// Asks the token endpoint whether it knows the OAuth client, before any browser
/// opens. A made-up authorization code is refused with `invalid_grant` when the client
/// exists and with `invalid_client` when the client ID or secret is wrong; only the
/// latter is an error here. Fails with [`Error::Http`] when the server can't be reached.
pub fn check_client(config: &YoutubeConfig) -> Result<()> {
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", "rushes-client-check"),
        ("redirect_uri", "http://127.0.0.1"),
        ("client_id", config.client_id.as_str()),
        ("code_verifier", "rushes-client-check-rushes-client-check-rushes"),
    ];
    if let Some(secret) = &config.client_secret {
        form.push(("client_secret", secret));
    }
    let mut response = http::agent()
        .post(&config.token_url)
        .config()
        .timeout_global(Some(CLIENT_CHECK_TIMEOUT))
        .build()
        .send_form(form)?;
    let status = response.status().as_u16();
    if response.status().is_success() {
        return Ok(());
    }
    match oauth_error(status, &body_text(&mut response)) {
        error if error.is_invalid_client() => Err(error),
        // Anything else is about the made-up code, or for the real sign-in to find out.
        _ => Ok(()),
    }
}

/// Exchanges the refresh token for a new access token. Google usually does not
/// return a new refresh token; the old one is kept then.
pub fn refresh(config: &YoutubeConfig, credentials: &Credentials) -> Result<Credentials> {
    refresh_with(&http::agent(), config, credentials)
}

pub(crate) fn refresh_with(agent: &Agent, config: &YoutubeConfig, credentials: &Credentials) -> Result<Credentials> {
    let refresh_token = credentials.refresh_token.as_deref().ok_or(Error::NoRefreshToken)?;
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", config.client_id.as_str()),
    ];
    if let Some(secret) = &config.client_secret {
        form.push(("client_secret", secret));
    }
    token_request(agent.post(&config.token_url).send_form(form)?, Some(credentials))
}

/// Revokes the grant: the access token and, if present, the refresh token. Tokens
/// Google already considers invalid count as revoked.
pub fn revoke(config: &YoutubeConfig, credentials: &Credentials) -> Result<()> {
    revoke_with(&http::agent(), config, credentials)
}

pub(crate) fn revoke_with(agent: &Agent, config: &YoutubeConfig, credentials: &Credentials) -> Result<()> {
    let tokens = std::iter::once(credentials.access_token.as_str()).chain(credentials.refresh_token.as_deref());
    for token in tokens {
        let mut response = agent.post(&config.revoke_url).send_form([("token", token)])?;
        let status = response.status().as_u16();
        if response.status().is_success() {
            continue;
        }
        let body = body_text(&mut response);
        if status == 400 && body.contains("invalid_token") {
            continue;
        }
        return Err(api_error(status, &body));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc_7636_example() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_is_64_url_safe_characters() {
        let verifier = random_token(48).unwrap();
        assert_eq!(verifier.len(), 64);
        assert!(verifier.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(verifier, random_token(48).unwrap());
    }

    #[test]
    fn authorize_url_carries_the_installed_app_parameters() {
        let config = YoutubeConfig::google("client.apps.googleusercontent.com", None);
        let url = authorize_url(&config, "http://127.0.0.1:5555", "the-state", "the-challenge").unwrap();
        let url = Url::parse(&url).unwrap();
        assert_eq!(url.host_str(), Some("accounts.google.com"));
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(params["client_id"], "client.apps.googleusercontent.com");
        assert_eq!(params["redirect_uri"], "http://127.0.0.1:5555");
        assert_eq!(params["response_type"], "code");
        assert_eq!(params["state"], "the-state");
        assert_eq!(params["code_challenge"], "the-challenge");
        assert_eq!(params["code_challenge_method"], "S256");
        assert_eq!(params["access_type"], "offline");
        assert_eq!(params["prompt"], "consent");
        assert_eq!(params["include_granted_scopes"], "true");
        assert_eq!(
            params["scope"],
            "openid email profile https://www.googleapis.com/auth/youtube.upload https://www.googleapis.com/auth/youtube.readonly \
             https://www.googleapis.com/auth/youtube.force-ssl"
        );
    }

    fn emulator_config() -> YoutubeConfig {
        YoutubeConfig::emulator("http://127.0.0.1:1", "http://127.0.0.1:2")
    }

    fn address(pending: &PendingAuthorization) -> String {
        pending.redirect_uri().trim_start_matches("http://").to_owned()
    }

    /// Sends a raw request to the loopback server and returns the raw response.
    fn browser_get(address: &str, path: &str) -> String {
        let mut stream = TcpStream::connect(address).unwrap();
        write!(stream, "GET {path} HTTP/1.1\r\nHost: {address}\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn begin_authorization_binds_a_loopback_port() {
        let pending = begin_authorization(&emulator_config()).unwrap();
        assert!(pending.redirect_uri().starts_with("http://127.0.0.1:"));
        assert!(pending.authorize_url().contains(&format!("state={}", pending.state)));
    }

    #[test]
    fn google_sign_in_requires_the_client_secret() {
        let without = YoutubeConfig::google("id.apps.googleusercontent.com", None);
        assert!(matches!(begin_authorization(&without), Err(Error::InvalidInput(_))));
        let with = YoutubeConfig::google("id.apps.googleusercontent.com", Some("secret".into()));
        assert!(begin_authorization(&with).is_ok());
    }

    #[test]
    fn wait_ignores_unrelated_requests_and_foreign_state() {
        let pending = begin_authorization(&emulator_config()).unwrap();
        let address = address(&pending);
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let browser = thread::spawn({
            let cancel = cancel.clone();
            move || {
                let favicon = browser_get(&address, "/favicon.ico");
                let forged = browser_get(&address, "/?code=abc&state=forged");
                cancel.store(true, Ordering::Relaxed);
                (favicon, forged)
            }
        });
        let result = pending.wait(Duration::from_secs(10), &cancel);
        let (favicon, forged) = browser.join().unwrap();
        assert!(favicon.starts_with("HTTP/1.1 404"));
        assert!(forged.starts_with("HTTP/1.1 400"), "{forged}");
        assert!(forged.contains("Sign-in link expired"));
        assert!(matches!(result, Err(Error::Cancelled { session_url: None })));
    }

    #[test]
    fn a_stalled_connection_does_not_block_cancellation() {
        let pending = begin_authorization(&emulator_config()).unwrap();
        let mut stalled = TcpStream::connect(address(&pending)).unwrap();
        stalled.write_all(b"G").unwrap();
        let started = Instant::now();
        let result = pending.wait(Duration::from_secs(30), &AtomicBool::new(true));
        assert!(matches!(result, Err(Error::Cancelled { .. })));
        assert!(started.elapsed() < REQUEST_READ_TIMEOUT + Duration::from_secs(1));

        let pending = begin_authorization(&emulator_config()).unwrap();
        let mut stalled = TcpStream::connect(address(&pending)).unwrap();
        stalled.write_all(b"GET /?code=1").unwrap();
        let started = Instant::now();
        let result = pending.wait(Duration::from_millis(500), &AtomicBool::new(false));
        assert!(matches!(result, Err(Error::Timeout)));
        assert!(started.elapsed() < REQUEST_READ_TIMEOUT + Duration::from_secs(1));
    }

    #[test]
    fn wait_without_a_deadline_still_honours_cancel() {
        let pending = begin_authorization(&emulator_config()).unwrap();
        let result = pending.wait(Duration::MAX, &AtomicBool::new(true));
        assert!(matches!(result, Err(Error::Cancelled { .. })));
    }

    #[test]
    fn wait_reports_denied_consent() {
        let pending = begin_authorization(&emulator_config()).unwrap();
        let address = address(&pending);
        let state = pending.state.clone();
        let browser = thread::spawn(move || browser_get(&address, &format!("/?error=access_denied&state={state}")));
        let result = pending.wait(Duration::from_secs(10), &AtomicBool::new(false));
        assert!(browser.join().unwrap().contains("Sign-in didn"));
        assert!(matches!(result, Err(Error::AccessDenied)));
    }

    /// A token endpoint that answers one request with `status` and `body`.
    fn token_server(status: &'static str, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        format!("http://{address}/token")
    }

    #[test]
    fn client_check_tells_a_wrong_client_from_a_made_up_code() {
        let config = |token_url: String| YoutubeConfig {
            token_url,
            ..YoutubeConfig::google("000000000000-bogus.apps.googleusercontent.com", Some("GOCSPX-x".into()))
        };
        let unknown = token_server(
            "401 Unauthorized",
            r#"{"error":"invalid_client","error_description":"The OAuth client was not found."}"#,
        );
        assert!(check_client(&config(unknown)).unwrap_err().is_invalid_client());
        let known =
            token_server("400 Bad Request", r#"{"error":"invalid_grant","error_description":"Malformed auth code."}"#);
        assert!(check_client(&config(known)).is_ok());
    }

    #[test]
    fn token_response_keeps_previous_refresh_token() {
        let previous = Credentials {
            access_token: "old".into(),
            refresh_token: Some("refresh".into()),
            expires_at: Utc::now(),
            scope: "openid".into(),
            id_token: None,
        };
        let token = TokenResponse {
            access_token: "new".into(),
            expires_in: 3600,
            refresh_token: None,
            scope: None,
            id_token: None,
        };
        let credentials = token.into_credentials(Some(&previous));
        assert_eq!(credentials.refresh_token.as_deref(), Some("refresh"));
        assert_eq!(credentials.scope, "openid");
        assert!(!credentials.expires_within(Duration::from_secs(3000)));
        assert!(credentials.expires_within(Duration::from_secs(3700)));
        assert!(!format!("{credentials:?}").contains("new"));
    }
}
