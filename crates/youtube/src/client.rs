use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::de::DeserializeOwned;
use ureq::Agent;
use ureq::http::StatusCode;

use crate::config::YoutubeConfig;
use crate::error::{Error, Result};
use crate::http::{self, HttpResponse, bearer};
use crate::oauth::{self, Credentials};
use crate::types::{
    Channel, ChannelResource, GoogleUser, ListResponse, PlaylistItemResource, ThumbnailSetResponse,
    Thumbnails, VideoInfo, VideoResource,
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
    on_refresh: RefreshCallback,
    pub(crate) upload: UploadOptions,
}

impl YoutubeClient {
    /// `on_refresh` receives every refreshed token set so the app can persist it. It
    /// runs while the client's token lock is held: don't call back into the client.
    pub fn new(
        config: YoutubeConfig,
        credentials: Credentials,
        on_refresh: impl Fn(&Credentials) + Send + Sync + 'static,
    ) -> Self {
        Self {
            config,
            agent: http::agent(),
            credentials: Mutex::new(credentials),
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
        let list: ListResponse<ChannelResource> = self.get_json(
            &url,
            &[
                ("part", "snippet,contentDetails,statistics"),
                ("mine", "true"),
            ],
        )?;
        list.items
            .into_iter()
            .next()
            .map(Channel::from)
            .ok_or(Error::NoChannel)
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
            let mut query = vec![
                ("part", "contentDetails"),
                ("playlistId", playlist.as_str()),
                ("maxResults", &page_size),
            ];
            if let Some(token) = &page_token {
                query.push(("pageToken", token));
            }
            let page: ListResponse<PlaylistItemResource> = self.get_json(&url, &query)?;
            ids.extend(
                page.items
                    .into_iter()
                    .map(|item| item.content_details.video_id),
            );
            match page.next_page_token {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }
        ids.truncate(max);
        self.get_videos(&ids)
    }

    /// Sets a custom thumbnail from a PNG or JPEG file (at most 2 MB). Returns the URL
    /// of the largest thumbnail size.
    pub fn set_thumbnail(&self, video_id: &str, path: &Path) -> Result<String> {
        let content_type = match extension(path).as_deref() {
            Some("png") => "image/png",
            Some("jpg" | "jpeg") => "image/jpeg",
            _ => {
                return Err(Error::InvalidInput(
                    "the thumbnail must be a PNG or JPEG file".to_owned(),
                ));
            }
        };
        let size = std::fs::metadata(path)
            .map_err(|e| Error::io(format!("reading {}", path.display()), e))?
            .len();
        if size > MAX_THUMBNAIL_BYTES {
            return Err(Error::InvalidInput(
                "the thumbnail is larger than YouTube's 2 MB limit".to_owned(),
            ));
        }
        let image =
            std::fs::read(path).map_err(|e| Error::io(format!("reading {}", path.display()), e))?;
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
        let set: ThumbnailSetResponse = http::json(response)?;
        set.items
            .first()
            .and_then(Thumbnails::best)
            .ok_or_else(|| Error::Protocol("thumbnails.set returned no thumbnail".to_owned()))
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
            let list: ListResponse<VideoResource> = self.get_json(
                &url,
                &[
                    ("part", "snippet,status,processingDetails"),
                    ("id", &joined),
                ],
            )?;
            found.extend(
                list.items
                    .into_iter()
                    .map(|video| (video.id.clone(), VideoInfo::from(video))),
            );
        }
        Ok(ids.iter().filter_map(|id| found.remove(id)).collect())
    }

    fn get_json<T: DeserializeOwned>(&self, url: &str, query: &[(&str, &str)]) -> Result<T> {
        let response = self.authorized(|token| {
            self.agent
                .get(url)
                .query_pairs(query.iter().copied())
                .header("Authorization", bearer(token))
                .call()
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
        match self.refresh_rejected(&token)? {
            Some(token) => Ok(send(&token)?),
            None => Ok(response),
        }
    }

    fn lock_credentials(&self) -> MutexGuard<'_, Credentials> {
        // Credentials are replaced whole, so a panic elsewhere cannot leave them torn.
        self.credentials
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn access_token(&self) -> Result<String> {
        let mut credentials = self.lock_credentials();
        if credentials.expires_within(REFRESH_MARGIN) && credentials.refresh_token.is_some() {
            self.refresh_locked(&mut credentials)?;
        }
        Ok(credentials.access_token.clone())
    }

    /// New token after `rejected` got a 401, unless another thread already refreshed.
    /// `None` when there is no refresh token to try.
    fn refresh_rejected(&self, rejected: &str) -> Result<Option<String>> {
        let mut credentials = self.lock_credentials();
        if credentials.access_token != rejected {
            return Ok(Some(credentials.access_token.clone()));
        }
        if credentials.refresh_token.is_none() {
            return Ok(None);
        }
        self.refresh_locked(&mut credentials)?;
        Ok(Some(credentials.access_token.clone()))
    }

    fn refresh_locked(&self, credentials: &mut Credentials) -> Result<()> {
        let fresh = oauth::refresh_with(&self.agent, &self.config, credentials)?;
        (self.on_refresh)(&fresh);
        *credentials = fresh;
        Ok(())
    }
}

pub(crate) fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<YoutubeClient>();
    }
}
