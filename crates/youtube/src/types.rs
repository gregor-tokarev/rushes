//! Data returned by the client, plus the subset of the YouTube wire format it is
//! parsed from.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// `status.privacyStatus` of a video.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Privacy {
    #[default]
    Private,
    Unlisted,
    Public,
}

impl Privacy {
    pub fn as_str(self) -> &'static str {
        match self {
            Privacy::Private => "private",
            Privacy::Unlisted => "unlisted",
            Privacy::Public => "public",
        }
    }
}

/// `status.uploadStatus`: whether YouTube has the file and finished with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UploadStatus {
    Uploaded,
    Processed,
    Failed,
    Rejected,
    Deleted,
    #[serde(other)]
    Unknown,
}

/// `processingDetails.processingStatus`. Only visible to the video's owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessingStatus {
    Processing,
    Succeeded,
    Failed,
    Terminated,
    #[serde(other)]
    Unknown,
}

/// OpenID Connect userinfo of the signed-in Google account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleUser {
    /// Stable account id (`sub`; the v2 endpoint calls it `id`).
    #[serde(alias = "id")]
    pub sub: String,
    pub email: String,
    pub name: Option<String>,
    pub picture: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Channel {
    pub id: String,
    pub title: String,
    /// Handle such as `@creator`.
    pub custom_url: Option<String>,
    pub thumbnail_url: Option<String>,
    /// `None` when the channel hides its subscriber count.
    pub subscriber_count: Option<u64>,
    /// Public videos only, as YouTube counts them.
    pub video_count: u64,
    pub uploads_playlist_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoInfo {
    pub id: String,
    pub title: String,
    pub privacy: Privacy,
    /// Scheduled publish time of a private video.
    pub publish_at: Option<DateTime<Utc>>,
    pub published_at: Option<DateTime<Utc>>,
    pub upload_status: UploadStatus,
    pub processing_status: Option<ProcessingStatus>,
    pub url: String,
    pub thumbnail: Option<String>,
}

pub fn watch_url(video_id: &str) -> String {
    format!("https://www.youtube.com/watch?v={video_id}")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListResponse<T> {
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    pub next_page_token: Option<String>,
}

#[derive(Default, Deserialize)]
pub(crate) struct Thumbnails {
    default: Option<Thumbnail>,
    medium: Option<Thumbnail>,
    high: Option<Thumbnail>,
    standard: Option<Thumbnail>,
    maxres: Option<Thumbnail>,
}

#[derive(Deserialize)]
struct Thumbnail {
    url: String,
}

impl Thumbnails {
    /// Largest available size.
    pub fn best(&self) -> Option<String> {
        [&self.maxres, &self.standard, &self.high, &self.medium, &self.default]
            .into_iter()
            .flatten()
            .next()
            .map(|thumbnail| thumbnail.url.clone())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChannelResource {
    id: String,
    snippet: Option<ChannelSnippet>,
    content_details: Option<ChannelContentDetails>,
    statistics: Option<ChannelStatistics>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChannelSnippet {
    title: String,
    custom_url: Option<String>,
    #[serde(default)]
    thumbnails: Thumbnails,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChannelContentDetails {
    related_playlists: RelatedPlaylists,
}

#[derive(Deserialize)]
struct RelatedPlaylists {
    uploads: String,
}

/// Counts arrive as decimal strings.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChannelStatistics {
    subscriber_count: Option<String>,
    #[serde(default)]
    hidden_subscriber_count: bool,
    video_count: Option<String>,
}

impl From<ChannelResource> for Channel {
    fn from(resource: ChannelResource) -> Self {
        let snippet = resource.snippet;
        let statistics = resource.statistics;
        let count = |value: Option<&String>| value.and_then(|v| v.parse::<u64>().ok());
        Channel {
            id: resource.id,
            title: snippet.as_ref().map(|s| s.title.clone()).unwrap_or_default(),
            custom_url: snippet.as_ref().and_then(|s| s.custom_url.clone()),
            thumbnail_url: snippet.as_ref().and_then(|s| s.thumbnails.best()),
            subscriber_count: statistics
                .as_ref()
                .filter(|s| !s.hidden_subscriber_count)
                .and_then(|s| count(s.subscriber_count.as_ref())),
            video_count: statistics.as_ref().and_then(|s| count(s.video_count.as_ref())).unwrap_or(0),
            uploads_playlist_id: resource.content_details.map(|d| d.related_playlists.uploads).unwrap_or_default(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VideoResource {
    pub id: String,
    pub snippet: Option<VideoSnippet>,
    pub status: Option<VideoStatus>,
    processing_details: Option<ProcessingDetails>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VideoSnippet {
    pub title: String,
    pub channel_id: String,
    published_at: Option<DateTime<Utc>>,
    #[serde(default)]
    thumbnails: Thumbnails,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VideoStatus {
    pub upload_status: UploadStatus,
    pub privacy_status: Privacy,
    publish_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProcessingDetails {
    processing_status: ProcessingStatus,
}

impl From<VideoResource> for VideoInfo {
    fn from(resource: VideoResource) -> Self {
        let url = watch_url(&resource.id);
        VideoInfo {
            title: resource.snippet.as_ref().map(|s| s.title.clone()).unwrap_or_default(),
            privacy: resource.status.as_ref().map(|s| s.privacy_status).unwrap_or_default(),
            publish_at: resource.status.as_ref().and_then(|s| s.publish_at),
            published_at: resource.snippet.as_ref().and_then(|s| s.published_at),
            upload_status: resource.status.as_ref().map_or(UploadStatus::Unknown, |s| s.upload_status),
            processing_status: resource.processing_details.map(|d| d.processing_status),
            thumbnail: resource.snippet.as_ref().and_then(|s| s.thumbnails.best()),
            url,
            id: resource.id,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlaylistItemResource {
    pub content_details: PlaylistItemContentDetails,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlaylistItemContentDetails {
    pub video_id: String,
}

#[derive(Deserialize)]
pub(crate) struct ThumbnailSetResponse {
    #[serde(default)]
    pub items: Vec<Thumbnails>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_channel_resource() {
        let json = r#"{"kind":"youtube#channel","id":"UC1","snippet":{"title":"Demo","customUrl":"@demo",
            "thumbnails":{"default":{"url":"small","width":88,"height":88},"high":{"url":"large"}}},
            "contentDetails":{"relatedPlaylists":{"likes":"","uploads":"UU1"}},
            "statistics":{"viewCount":"10","subscriberCount":"42","hiddenSubscriberCount":false,"videoCount":"7"}}"#;
        let channel = Channel::from(serde_json::from_str::<ChannelResource>(json).unwrap());
        assert_eq!(
            channel,
            Channel {
                id: "UC1".into(),
                title: "Demo".into(),
                custom_url: Some("@demo".into()),
                thumbnail_url: Some("large".into()),
                subscriber_count: Some(42),
                video_count: 7,
                uploads_playlist_id: "UU1".into(),
            }
        );

        let hidden =
            r#"{"id":"UC2","statistics":{"subscriberCount":"0","hiddenSubscriberCount":true,"videoCount":"1"}}"#;
        let channel = Channel::from(serde_json::from_str::<ChannelResource>(hidden).unwrap());
        assert_eq!(channel.subscriber_count, None);
        assert_eq!(channel.video_count, 1);
    }

    #[test]
    fn parses_video_resource_with_unknown_enum_values() {
        let json = r#"{"id":"abc","snippet":{"title":"T","channelId":"UC1","publishedAt":"2026-01-02T03:04:05Z"},
            "status":{"uploadStatus":"somethingNew","privacyStatus":"private","publishAt":"2026-02-01T00:00:00Z"},
            "processingDetails":{"processingStatus":"processing"}}"#;
        let video = VideoInfo::from(serde_json::from_str::<VideoResource>(json).unwrap());
        assert_eq!(video.upload_status, UploadStatus::Unknown);
        assert_eq!(video.processing_status, Some(ProcessingStatus::Processing));
        assert_eq!(video.privacy, Privacy::Private);
        assert_eq!(video.url, "https://www.youtube.com/watch?v=abc");
        assert_eq!(video.publish_at.unwrap().to_rfc3339(), "2026-02-01T00:00:00+00:00");
        assert_eq!(video.thumbnail, None);
    }

    #[test]
    fn userinfo_accepts_v2_id_field() {
        let user: GoogleUser = serde_json::from_str(r#"{"id":"123","email":"a@b.c","name":"A"}"#).unwrap();
        assert_eq!(user.sub, "123");
        assert_eq!(user.picture, None);
    }
}
