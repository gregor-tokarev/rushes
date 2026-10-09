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

    #[error("the sign-in response did not match this request; start the sign-in again")]
    StateMismatch,

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

    #[error("unexpected response: {0}")]
    Protocol(String),

    #[error("invalid credentials file: {0}")]
    Json(#[from] serde_json::Error),
}

impl Error {
    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Error::Io { context: context.into(), source }
    }

    /// True when the user has to sign in again (revoked or expired grant).
    pub fn needs_sign_in(&self) -> bool {
        match self {
            Error::Api { status, .. } => *status == 401,
            Error::OAuth { error, .. } => error == "invalid_grant",
            Error::NoRefreshToken => true,
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

    /// Failures worth retrying after a backoff: dropped connections, timeouts and
    /// server-side errors.
    pub(crate) fn is_transient(&self) -> bool {
        match self {
            Error::Http(error) => matches!(
                error,
                ureq::Error::Io(_)
                    | ureq::Error::Timeout(_)
                    | ureq::Error::ConnectionFailed
                    | ureq::Error::HostNotFound
                    | ureq::Error::Protocol(_)
                    | ureq::Error::BodyStalled
            ),
            Error::Api { status, .. } => matches!(status, 408 | 429 | 500..=599),
            _ => false,
        }
    }
}

fn describe_api_error(status: u16, reason: &str, message: &str) -> String {
    match reason {
        "quotaExceeded" => "The YouTube API quota for today is used up. It resets at midnight Pacific time; \
             each upload costs about 1,600 of the default 10,000 daily units."
            .to_owned(),
        "uploadLimitExceeded" => {
            "This channel reached YouTube's upload limit. Try again in about 24 hours.".to_owned()
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
        Ok(ErrorBody::OAuth { error, error_description }) => Error::Api {
            status,
            message: error_description.unwrap_or_else(|| error.clone()),
            reason: error,
        },
        Err(_) => Error::Api {
            status,
            reason: "unknown".to_owned(),
            message: body.trim().chars().take(200).collect(),
        },
    }
}

/// Builds [`Error::OAuth`] from a token endpoint error body.
pub(crate) fn oauth_error(status: u16, body: &str) -> Error {
    match serde_json::from_str::<ErrorBody>(body) {
        Ok(ErrorBody::OAuth { error, error_description }) => {
            Error::OAuth { error, description: error_description }
        }
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
        let error = api_error(401, r#"{"error":{"code":401,"message":"Invalid Credentials","status":"UNAUTHENTICATED"}}"#);
        assert!(matches!(&error, Error::Api { reason, .. } if reason == "UNAUTHENTICATED"));
        assert!(error.needs_sign_in());

        let error = api_error(502, "<html>Bad Gateway</html>");
        assert!(matches!(&error, Error::Api { reason, message, .. } if reason == "unknown" && message.contains("Bad Gateway")));
        assert!(error.is_transient());
    }

    #[test]
    fn parses_oauth_error_bodies() {
        let error = oauth_error(400, r#"{"error":"invalid_grant","error_description":"The refresh token is invalid."}"#);
        assert!(matches!(&error, Error::OAuth { error, .. } if error == "invalid_grant"));
        assert!(error.needs_sign_in());
        assert_eq!(error.to_string(), "Google sign-in failed (invalid_grant): The refresh token is invalid.");

        let error = api_error(401, r#"{"error":"invalid_token","error_description":"Authentication required."}"#);
        assert!(matches!(&error, Error::Api { reason, message, .. } if reason == "invalid_token" && message == "Authentication required."));
    }

    #[test]
    fn upload_limit_has_its_own_message() {
        let body = r#"{"error":{"code":400,"message":"x","errors":[{"reason":"uploadLimitExceeded"}]}}"#;
        assert!(api_error(400, body).to_string().contains("upload limit"));
    }
}
