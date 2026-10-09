use std::time::Duration;

use serde::de::DeserializeOwned;
use ureq::Agent;
use ureq::http::Response;

use crate::error::{Error, Result, api_error};

pub(crate) type HttpResponse = Response<ureq::Body>;

/// Agent shared by OAuth and API calls. Status codes are handled by the caller and
/// redirects are not followed, because resumable uploads answer 308 to mean
/// "incomplete", not "moved".
pub(crate) fn agent() -> Agent {
    Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_send_request(Some(Duration::from_secs(30)))
        .timeout_send_body(Some(Duration::from_secs(60)))
        .timeout_recv_response(Some(Duration::from_secs(120)))
        .timeout_recv_body(Some(Duration::from_secs(120)))
        .user_agent(concat!("videocrm/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

pub(crate) fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

pub(crate) fn body_text(response: &mut HttpResponse) -> String {
    // The error body is best effort: the status code alone is still reported.
    response.body_mut().read_to_string().unwrap_or_default()
}

/// Parses a 2xx JSON body, or turns the response into [`Error::Api`].
pub(crate) fn json<T: DeserializeOwned>(mut response: HttpResponse) -> Result<T> {
    let status = response.status().as_u16();
    if !response.status().is_success() {
        return Err(api_error(status, &body_text(&mut response)));
    }
    let text = response.body_mut().read_to_string()?;
    serde_json::from_str(&text)
        .map_err(|error| Error::Protocol(format!("invalid JSON from server: {error}")))
}
