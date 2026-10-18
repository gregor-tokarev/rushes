use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use ureq::Agent;
use ureq::http::StatusCode;

use crate::config::YoutubeConfig;
use crate::error::{Error, Result};
use crate::http::{self, HttpResponse, bearer};
use crate::oauth::{self, Credentials};
use crate::types::{
    Channel, ChannelResource, GoogleUser, ListResponse, PlaylistItemResource, Privacy, StatusPart,
    ThumbnailSetResponse, Thumbnails, VideoInfo, VideoResource, Visibility, VisibilityStatus,
};
use crate::upload::UploadOptions;

/// Refresh this long before the access token expires, so a request never races expiry.
const REFRESH_MARGIN: Duration = Duration::from_secs(60);
/// Page size limit of `videos.list` and `playlistItems.list`.
const MAX_PAGE: usize = 50;
const MAX_THUMBNAIL_BYTES: u64 = 2 * 1024 * 1024;

type RefreshCallback = Box<dyn Fn(&Credentials) + Send + Sync>;

/// Blocking YouTube Data API client for one signed-in account. `Send + Sync`: share
/// it behind an `Arc`; token refreshes are serialized internally.
pub struct YoutubeClient {
    pub(crate) config: YoutubeConfig,
    pub(crate) agent: Agent,
    credentials: Mutex<Credentials>,
    /// Held for the whole refresh round trip so concurrent 401s refresh only once,
    /// while `credentials` stays readable.
    refreshing: Mutex<()>,
    on_refresh: RefreshCallback,
    pub(crate) upload: UploadOptions,
}

impl YoutubeClient {
    /// `on_refresh` receives every refreshed token set, in order, so the app can
    /// persist it. It runs on the thread that refreshed, before the new token is used.
    pub fn new(
        config: YoutubeConfig,
        credentials: Credentials,
        on_refresh: impl Fn(&Credentials) + Send + Sync + 'static,
    ) -> Self {
        Self {
            config,
            agent: http::agent(),
            credentials: Mutex::new(credentials),
            refreshing: Mutex::new(()),
            on_refresh: Box::new(on_refresh),
            upload: UploadOptions::default(),
        }
    }

    pub fn with_upload_options(mut self, options: UploadOptions) -> Self {
        self.upload = options.normalized();
        self
    }

    pub fn config(&self) -> &YoutubeConfig {
        &self.config
    }

    /// Current tokens, including any refresh that happened since construction.
    pub fn credentials(&self) -> Credentials {
        self.lock_credentials().clone()
    }

    pub fn userinfo(&self) -> Result<GoogleUser> {
        self.get_json(&self.config.userinfo_url, &[])
    }

    /// The channel of the signed-in account.
    pub fn my_channel(&self) -> Result<Channel> {
        let url = format!("{}/channels", self.config.api_base);
        let list: ListResponse<ChannelResource> =
            self.get_json(&url, &[("part", "snippet,contentDetails,statistics"), ("mine", "true")])?;
        list.items.into_iter().next().map(Channel::from).ok_or(Error::NoChannel)
    }

    /// `None` when the video does not exist or is not visible to this account.
    pub fn get_video(&self, id: &str) -> Result<Option<VideoInfo>> {
        Ok(self.get_videos(&[id.to_owned()])?.into_iter().next())
    }

    /// Up to `max` of the channel's uploads, newest first.
    pub fn list_my_uploads(&self, max: usize) -> Result<Vec<VideoInfo>> {
        let playlist = self.my_channel()?.uploads_playlist_id;
        let url = format!("{}/playlistItems", self.config.api_base);
        let mut ids = Vec::new();
        let mut page_token: Option<String> = None;
        while ids.len() < max {
            let page_size = (max - ids.len()).min(MAX_PAGE).to_string();
            let mut query =
                vec![("part", "contentDetails"), ("playlistId", playlist.as_str()), ("maxResults", &page_size)];
            if let Some(token) = &page_token {
                query.push(("pageToken", token));
            }
            let page: ListResponse<PlaylistItemResource> = self.get_json(&url, &query)?;
            ids.extend(page.items.into_iter().map(|item| item.content_details.video_id));
            match page.next_page_token {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }
        ids.truncate(max);
        self.get_videos(&ids)
    }

    /// Sets a custom thumbnail from a PNG or JPEG file (at most 2 MB). Returns the URL
    /// of the largest thumbnail size. [`Error::ThumbnailsNotAllowed`] when the channel
    /// is not verified.
    pub fn set_thumbnail(&self, video_id: &str, path: &Path) -> Result<String> {
        let content_type = match extension(path).as_deref() {
            Some("png") => "image/png",
            Some("jpg" | "jpeg") => "image/jpeg",
            _ => {
                return Err(Error::InvalidInput("the thumbnail must be a PNG or JPEG file".to_owned()));
            }
        };
        let size = std::fs::metadata(path).map_err(|e| Error::io(format!("reading {}", path.display()), e))?.len();
        if size > MAX_THUMBNAIL_BYTES {
            return Err(Error::InvalidInput("the thumbnail is larger than YouTube’s 2 MB limit".to_owned()));
        }
        let image = std::fs::read(path).map_err(|e| Error::io(format!("reading {}", path.display()), e))?;
        let url = format!("{}/thumbnails/set", self.config.upload_base);
        let response = self.authorized(|token| {
            self.agent
                .post(&url)
                .query("videoId", video_id)
                .query("uploadType", "media")
                .header("Authorization", bearer(token))
                .header("Content-Type", content_type)
                .send(&image[..])
        })?;
        let set: ThumbnailSetResponse = http::json(response).map_err(|error| match error {
            // What YouTube answers for channels that are not phone-verified.
            Error::Api { status: 403, ref reason, .. } if reason == "forbidden" => Error::ThumbnailsNotAllowed,
            error => error,
        })?;
        set.items
            .first()
            .and_then(Thumbnails::best)
            .ok_or_else(|| Error::Protocol("thumbnails.set returned no thumbnail".to_owned()))
    }

    /// Changes who can watch the video and when a private one goes public; the rest of
    /// its status stays as it is. Returns what YouTube has afterwards, which may differ
    /// from what was asked: YouTube keeps uploads from API projects Google hasn't
    /// audited private. `None` when the video doesn't exist or this account can't see
    /// it. The sign-in needs [`MANAGE_SCOPE`](crate::MANAGE_SCOPE).
    pub fn set_visibility(&self, video_id: &str, visibility: Visibility) -> Result<Option<Visibility>> {
        if let Some(publish_at) = visibility.publish_at {
            if visibility.privacy != Privacy::Private {
                return Err(Error::InvalidInput("a scheduled video must be private until it is published".to_owned()));
            }
            if publish_at <= Utc::now() {
                return Err(Error::InvalidInput("the scheduled publish time is in the past".to_owned()));
            }
        }
        let url = format!("{}/videos", self.config.api_base);
        // `videos.update` clears whatever the request leaves out, so the rest of the
        // status is sent back as YouTube has it.
        let list: ListResponse<StatusPart<Map<String, Value>>> =
            self.get_json(&url, &[("part", "status"), ("id", video_id)])?;
        let Some(current) = list.items.into_iter().next() else { return Ok(None) };
        let body = serde_json::to_vec(&json!({"id": video_id, "status": status_update(current.status, visibility)}))?;
        let response = self.authorized(|token| {
            self.agent
                .put(&url)
                .query("part", "status")
                .header("Authorization", bearer(token))
                .header("Content-Type", "application/json; charset=UTF-8")
                .send(&body[..])
        })?;
        let updated: StatusPart<VisibilityStatus> = http::json(response)?;
        Ok(Some(updated.status.into()))
    }

    /// Signs the account out at Google. The caller should also clear the stored credentials.
    pub fn revoke(&self) -> Result<()> {
        oauth::revoke_with(&self.agent, &self.config, &self.credentials())
    }

    fn get_videos(&self, ids: &[String]) -> Result<Vec<VideoInfo>> {
        let url = format!("{}/videos", self.config.api_base);
        let mut found = HashMap::new();
        for batch in ids.chunks(MAX_PAGE) {
            let joined = batch.join(",");
            let list: ListResponse<VideoResource> =
                self.get_json(&url, &[("part", "snippet,status,processingDetails"), ("id", &joined)])?;
            found.extend(list.items.into_iter().map(|video| (video.id.clone(), VideoInfo::from(video))));
        }
        Ok(ids.iter().filter_map(|id| found.remove(id)).collect())
    }

    fn get_json<T: DeserializeOwned>(&self, url: &str, query: &[(&str, &str)]) -> Result<T> {
        let response = self.authorized(|token| {
            self.agent.get(url).query_pairs(query.iter().copied()).header("Authorization", bearer(token)).call()
        })?;
        http::json(response)
    }

    /// Sends a request with a fresh access token. On 401 the token is refreshed once
    /// and the request sent again; `send` must therefore be repeatable.
    pub(crate) fn authorized(
        &self,
        mut send: impl FnMut(&str) -> std::result::Result<HttpResponse, ureq::Error>,
    ) -> Result<HttpResponse> {
        let token = self.access_token()?;
        let response = send(&token)?;
        if response.status() != StatusCode::UNAUTHORIZED {
            return Ok(response);
        }
        match self.refresh_replacing(&token)? {
            Some(token) => Ok(send(&token)?),
            None => Ok(response),
        }
    }

    fn lock_credentials(&self) -> MutexGuard<'_, Credentials> {
        // Credentials are replaced whole, so a panic elsewhere cannot leave them torn.
        self.credentials.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn access_token(&self) -> Result<String> {
        let current = self.credentials();
        if current.refresh_token.is_some()
            && current.expires_within(REFRESH_MARGIN)
            && let Some(token) = self.refresh_replacing(&current.access_token)?
        {
            return Ok(token);
        }
        Ok(current.access_token)
    }

    /// Refreshes `stale` unless another thread already replaced it, and returns the
    /// token to use. `None` when there is no refresh token.
    fn refresh_replacing(&self, stale: &str) -> Result<Option<String>> {
        let _refreshing = self.refreshing.lock().unwrap_or_else(PoisonError::into_inner);
        let current = self.credentials();
        if current.access_token != stale {
            return Ok(Some(current.access_token));
        }
        if current.refresh_token.is_none() {
            return Ok(None);
        }
        let fresh = oauth::refresh_with(&self.agent, &self.config, &current)?;
        (self.on_refresh)(&fresh);
        let token = fresh.access_token.clone();
        *self.lock_credentials() = fresh;
        Ok(Some(token))
    }
}

/// The `status` part to send for `visibility`, from the one YouTube has.
fn status_update(mut status: Map<String, Value>, visibility: Visibility) -> Map<String, Value> {
    // Read-only: YouTube works them out.
    for key in ["uploadStatus", "failureReason", "rejectionReason", "madeForKids"] {
        status.remove(key);
    }
    status.insert("privacyStatus".to_owned(), visibility.privacy.as_str().into());
    match visibility.publish_at {
        Some(at) => status.insert("publishAt".to_owned(), at.to_rfc3339_opts(SecondsFormat::Secs, true).into()),
        None => status.remove("publishAt"),
    };
    status
}

pub(crate) fn extension(path: &Path) -> Option<String> {
    path.extension().and_then(|ext| ext.to_str()).map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_updates_keep_the_rest_of_the_status() {
        let status = json!({
            "uploadStatus": "processed", "privacyStatus": "private", "publishAt": "2030-01-02T03:04:05Z",
            "license": "creativeCommon", "embeddable": false, "publicStatsViewable": true,
            "madeForKids": false, "selfDeclaredMadeForKids": false, "containsSyntheticMedia": true,
        });
        let Value::Object(status) = status else { unreachable!() };
        let public = Visibility { privacy: Privacy::Public, publish_at: None };
        assert_eq!(
            Value::Object(status_update(status.clone(), public)),
            json!({
                "privacyStatus": "public", "license": "creativeCommon", "embeddable": false,
                "publicStatsViewable": true, "selfDeclaredMadeForKids": false, "containsSyntheticMedia": true,
            })
        );
        let at = chrono::DateTime::parse_from_rfc3339("2031-05-06T07:08:09.5Z").unwrap().to_utc();
        let scheduled = status_update(status, Visibility { privacy: Privacy::Private, publish_at: Some(at) });
        assert_eq!(scheduled["privacyStatus"], "private");
        assert_eq!(scheduled["publishAt"], "2031-05-06T07:08:09Z");
    }

    #[test]
    fn client_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<YoutubeClient>();
    }
}
