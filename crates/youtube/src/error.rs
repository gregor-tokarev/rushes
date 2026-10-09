use serde::Deserialize;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Network-level failure (DNS, connect, reset, timeout, TLS).
    #[error("network error: {0}")]
    Http(#[from] ureq::Error),

    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    /// Error answer from a Google or YouTube endpoint.
    #[error("{}", describe_api_error(*.status, .reason, .message))]
    Api { status: u16, reason: String, message: String },

    /// The OAuth token endpoint refused a code or refresh token (`invalid_grant`, ...).
    #[error("Google sign-in failed ({error}){}", .description.as_deref().map(|d| format!(": {d}")).unwrap_or_default())]
    OAuth { error: String, description: Option<String> },

    #[error("sign-in was cancelled in the browser")]
    AccessDenied,

    #[error("timed out waiting for the browser sign-in")]
    Timeout,

    /// Stopped through the cancel flag. For uploads `session_url` resumes the transfer.
    #[error("cancelled")]
    Cancelled { session_url: Option<String> },

    /// The resumable upload session no longer exists; the upload has to start over.
    #[error("the upload session expired; start the upload again")]
    SessionExpired,

    #[error("the access token expired and there is no refresh token; sign in again")]
    NoRefreshToken,

    #[error("{0}")]
    InvalidInput(String),

    #[error("this Google account has no YouTube channel yet; create one on youtube.com first")]
    NoChannel,

    /// `thumbnails.set` answered `403 forbidden`: custom thumbnails need a verified channel.
    #[error(
        "YouTube allows custom thumbnails only on verified channels; verify the channel at https://www.youtube.com/verify"
    )]
    ThumbnailsNotAllowed,

    #[error("unexpected response: {0}")]
    Protocol(String),

    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

impl Error {
    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Error::Io { context: context.into(), source }
    }

    /// True when the user has to sign in again: revoked or expired grant, or a
    /// consent where YouTube permissions were unticked.
    pub fn needs_sign_in(&self) -> bool {
        match self {
            Error::Api { status, reason, .. } => {
                *status == 401 || (*status == 403 && reason == "insufficientPermissions")
            }
            Error::OAuth { error, .. } => error == "invalid_grant",
            Error::NoRefreshToken => true,
            _ => false,
        }
    }

    /// True when the server could not be reached or the connection broke: offline,
    /// DNS failure, refused or reset connection, timeout.
    pub fn is_network(&self) -> bool {
        matches!(
            self,
            Error::Http(
                ureq::Error::Io(_)
                    | ureq::Error::Timeout(_)
                    | ureq::Error::ConnectionFailed
                    | ureq::Error::HostNotFound
                    | ureq::Error::Protocol(_)
                    | ureq::Error::BodyStalled
            )
        )
    }

    /// True when Google does not know the OAuth client: a wrong client ID or secret.
    pub fn is_invalid_client(&self) -> bool {
        match self {
            Error::OAuth { error, .. } => error == "invalid_client",
            Error::Api { reason, .. } => reason == "invalid_client",
            _ => false,
        }
    }

    /// Session URL to pass to `resume_upload` after a cancelled upload.
    pub fn session_url(&self) -> Option<&str> {
        match self {
            Error::Cancelled { session_url } => session_url.as_deref(),
            _ => None,
        }
    }

    /// Failures worth retrying after a backoff: dropped connections, timeouts,
    /// server-side errors and short-term rate limits.
    pub(crate) fn is_transient(&self) -> bool {
        self.is_network()
            || matches!(self, Error::Api { status, reason, .. } if is_server_trouble(*status) || is_rate_limit(*status, reason))
    }

    /// Why a transient failure is retried, for progress reports.
    pub(crate) fn retry_cause(&self) -> RetryCause {
        match self {
            Error::Api { status, reason, .. } if is_rate_limit(*status, reason) => RetryCause::RateLimited,
            Error::Api { .. } => RetryCause::Server,
            _ => RetryCause::Network,
        }
    }
}

/// Why an upload request is being retried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryCause {
    /// The server could not be reached or the connection broke.
    Network,
    /// The server answered with an error of its own (5xx, timeouts).
    Server,
    /// Too many requests in a short time.
    RateLimited,
}

fn is_server_trouble(status: u16) -> bool {
    matches!(status, 408 | 500..=599)
}

fn is_rate_limit(status: u16, reason: &str) -> bool {
    status == 429 || (status == 403 && matches!(reason, "rateLimitExceeded" | "userRateLimitExceeded"))
}

fn describe_api_error(status: u16, reason: &str, message: &str) -> String {
    match reason {
        "quotaExceeded" => "The YouTube API quota for today is used up. It resets at midnight Pacific time; \
             each upload costs about 1,600 of the default 10,000 daily units."
            .to_owned(),
        "uploadLimitExceeded" => "This channel reached YouTube's upload limit. Try again in about 24 hours.".to_owned(),
        "insufficientPermissions" => {
            "VideoCRM is missing permission to manage your YouTube videos. Connect the account \
             again and leave every permission box ticked."
                .to_owned()
        }
        _ if is_rate_limit(status, reason) => {
            "YouTube is limiting requests right now. Try again in a few minutes.".to_owned()
        }
        // Google's own text ("Backend Error") tells the creator nothing.
        _ if is_server_trouble(status) => {
            "YouTube is having problems right now. Try again in a few minutes.".to_owned()
        }
        _ => format!("{message} (HTTP {status}, {reason})"),
    }
}

/// `{"error": {"code", "message", "errors": [{"reason", ...}]}}` from Google APIs, or
/// `{"error": "...", "error_description": "..."}` from OAuth endpoints.
#[derive(Deserialize)]
#[serde(untagged)]
enum ErrorBody {
    Api { error: ApiErrorBody },
    OAuth { error: String, error_description: Option<String> },
}

#[derive(Deserialize)]
struct ApiErrorBody {
    #[serde(default)]
    message: String,
    #[serde(default)]
    errors: Vec<ApiErrorItem>,
    status: Option<String>,
}

#[derive(Deserialize)]
struct ApiErrorItem {
    reason: Option<String>,
}

/// Builds [`Error::Api`] from a non-success response body.
pub(crate) fn api_error(status: u16, body: &str) -> Error {
    match serde_json::from_str::<ErrorBody>(body) {
        Ok(ErrorBody::Api { error }) => Error::Api {
            status,
            reason: error
                .errors
                .into_iter()
                .find_map(|item| item.reason)
                .or(error.status)
                .unwrap_or_else(|| "unknown".to_owned()),
            message: error.message,
        },
        Ok(ErrorBody::OAuth { error, error_description }) => {
            Error::Api { status, message: error_description.unwrap_or_else(|| error.clone()), reason: error }
        }
        Err(_) => Error::Api { status, reason: "unknown".to_owned(), message: body.trim().chars().take(200).collect() },
    }
}

/// Builds [`Error::OAuth`] from a token endpoint error body.
pub(crate) fn oauth_error(status: u16, body: &str) -> Error {
    match serde_json::from_str::<ErrorBody>(body) {
        Ok(ErrorBody::OAuth { error, error_description }) => Error::OAuth { error, description: error_description },
        _ => api_error(status, body),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_youtube_error_bodies() {
        let body = r#"{"error":{"code":403,"message":"The request cannot be completed because you have exceeded your quota.",
            "errors":[{"message":"...","domain":"youtube.quota","reason":"quotaExceeded"}]}}"#;
        let error = api_error(403, body);
        assert!(matches!(&error, Error::Api { status: 403, reason, .. } if reason == "quotaExceeded"));
        assert!(error.to_string().contains("quota for today is used up"));
        assert!(!error.is_transient());
    }

    #[test]
    fn falls_back_to_status_and_raw_text() {
        let error =
            api_error(401, r#"{"error":{"code":401,"message":"Invalid Credentials","status":"UNAUTHENTICATED"}}"#);
        assert!(matches!(&error, Error::Api { reason, .. } if reason == "UNAUTHENTICATED"));
        assert!(error.needs_sign_in());

        let error = api_error(502, "<html>Bad Gateway</html>");
        assert!(
            matches!(&error, Error::Api { reason, message, .. } if reason == "unknown" && message.contains("Bad Gateway"))
        );
        assert!(error.is_transient());
    }

    #[test]
    fn parses_oauth_error_bodies() {
        let error =
            oauth_error(400, r#"{"error":"invalid_grant","error_description":"The refresh token is invalid."}"#);
        assert!(matches!(&error, Error::OAuth { error, .. } if error == "invalid_grant"));
        assert!(error.needs_sign_in());
        assert_eq!(error.to_string(), "Google sign-in failed (invalid_grant): The refresh token is invalid.");

        let error = api_error(401, r#"{"error":"invalid_token","error_description":"Authentication required."}"#);
        assert!(
            matches!(&error, Error::Api { reason, message, .. } if reason == "invalid_token" && message == "Authentication required.")
        );
    }

    #[test]
    fn rate_limits_are_retried_but_quota_is_not() {
        let body =
            |reason: &str| format!(r#"{{"error":{{"code":403,"message":"x","errors":[{{"reason":"{reason}"}}]}}}}"#);
        assert!(api_error(403, &body("rateLimitExceeded")).is_transient());
        assert!(api_error(403, &body("userRateLimitExceeded")).is_transient());
        assert!(!api_error(403, &body("quotaExceeded")).is_transient());
        assert!(!api_error(403, &body("forbidden")).is_transient());
    }

    #[test]
    fn missing_scopes_ask_for_a_new_sign_in() {
        let body = r#"{"error":{"code":403,"message":"Request had insufficient authentication scopes.",
            "errors":[{"message":"Insufficient Permission","domain":"global","reason":"insufficientPermissions"}],
            "status":"PERMISSION_DENIED"}}"#;
        let error = api_error(403, body);
        assert!(error.needs_sign_in());
        assert!(error.to_string().contains("permission box"));
        assert!(
            !api_error(403, r#"{"error":{"code":403,"message":"x","errors":[{"reason":"forbidden"}]}}"#)
                .needs_sign_in()
        );
    }

    #[test]
    fn outages_read_as_plain_language() {
        let body = r#"{"error":{"code":503,"message":"Backend Error","errors":[{"reason":"backendError"}]}}"#;
        let error = api_error(503, body);
        assert_eq!(error.to_string(), "YouTube is having problems right now. Try again in a few minutes.");
        assert!(error.is_transient() && !error.is_network());
        assert_eq!(error.retry_cause(), RetryCause::Server);

        let limited =
            api_error(429, r#"{"error":{"code":429,"message":"x","errors":[{"reason":"rateLimitExceeded"}]}}"#);
        assert!(limited.to_string().contains("limiting requests"));
        assert_eq!(limited.retry_cause(), RetryCause::RateLimited);

        let offline = Error::Http(ureq::Error::ConnectionFailed);
        assert!(offline.is_network() && offline.is_transient());
        assert_eq!(offline.retry_cause(), RetryCause::Network);
    }

    #[test]
    fn unknown_oauth_clients_are_recognised() {
        let error =
            oauth_error(401, r#"{"error":"invalid_client","error_description":"The OAuth client was not found."}"#);
        assert!(error.is_invalid_client());
        assert!(!error.needs_sign_in());
        assert!(!oauth_error(400, r#"{"error":"invalid_grant"}"#).is_invalid_client());
    }

    #[test]
    fn upload_limit_has_its_own_message() {
        let body = r#"{"error":{"code":400,"message":"x","errors":[{"reason":"uploadLimitExceeded"}]}}"#;
        assert!(api_error(400, body).to_string().contains("upload limit"));
    }
}
