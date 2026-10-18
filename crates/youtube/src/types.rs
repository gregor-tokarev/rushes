//! Data returned by the client, plus the subset of the YouTube wire format it is
//! parsed from.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

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
    pub description: String,
    /// Only the channel's owner sees them.
    pub tags: Vec<String>,
    pub category_id: String,
    pub privacy: Privacy,
    /// Scheduled publish time of a private video.
    pub publish_at: Option<DateTime<Utc>>,
    pub published_at: Option<DateTime<Utc>>,
    pub upload_status: UploadStatus,
    pub processing_status: Option<ProcessingStatus>,
    pub url: String,
    pub thumbnail: Option<String>,
}

impl VideoInfo {
    pub fn details(&self) -> Details {
        Details {
            title: self.title.clone(),
            description: self.description.clone(),
            tags: self.tags.clone(),
            category_id: self.category_id.clone(),
        }
    }

    pub fn set_details(&mut self, details: Details) {
        self.title = details.title;
        self.description = details.description;
        self.tags = details.tags;
        self.category_id = details.category_id;
    }
}

/// What viewers read about a video, and where YouTube files it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Details {
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    /// Category id such as "22" (People & Blogs).
    pub category_id: String,
}

/// New details for a video on YouTube; what is `None` stays as YouTube has it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DetailsChange {
    pub title: Option<String>,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    pub category_id: Option<String>,
}

impl DetailsChange {
    /// Checks YouTube's documented limits before any quota is spent.
    pub fn validate(&self) -> Result<()> {
        if let Some(title) = &self.title {
            check_title(title)?;
        }
        if let Some(description) = &self.description {
            check_description(description)?;
        }
        if let Some(tags) = &self.tags {
            check_tags(tags)?;
        }
        if let Some(category_id) = &self.category_id {
            check_category(category_id)?;
        }
        Ok(())
    }
}

fn invalid(message: &str) -> Result<()> {
    Err(Error::InvalidInput(message.to_owned()))
}

fn has_brackets(text: &str) -> bool {
    text.contains(['<', '>'])
}

pub(crate) fn check_title(title: &str) -> Result<()> {
    if title.trim().is_empty() {
        return invalid("the title is empty");
    }
    if title.chars().count() > 100 {
        return invalid("the title is longer than 100 characters");
    }
    if has_brackets(title) {
        return invalid("the title cannot contain < or >");
    }
    Ok(())
}

pub(crate) fn check_description(description: &str) -> Result<()> {
    if description.len() > 5000 {
        return invalid("the description is longer than 5000 bytes");
    }
    if has_brackets(description) {
        return invalid("the description cannot contain < or >");
    }
    Ok(())
}

pub(crate) fn check_tags(tags: &[String]) -> Result<()> {
    if tags.iter().any(|tag| has_brackets(tag)) {
        return invalid("tags cannot contain < or >");
    }
    // YouTube counts separators and the quotes around tags with spaces.
    let length = tags.iter().map(|tag| tag.chars().count() + if tag.contains(' ') { 2 } else { 0 }).sum::<usize>()
        + tags.len().saturating_sub(1);
    if length > 500 {
        return invalid("the tags are longer than 500 characters in total");
    }
    Ok(())
}

pub(crate) fn check_category(category_id: &str) -> Result<()> {
    if category_id.is_empty() || !category_id.bytes().all(|b| b.is_ascii_digit()) {
        return invalid("the category id must be numeric");
    }
    Ok(())
}

/// Who can watch a video, and when a private one goes public.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Visibility {
    pub privacy: Privacy,
    /// Scheduled publish time; YouTube requires `privacy == Private` until then.
    pub publish_at: Option<DateTime<Utc>>,
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
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub category_id: String,
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
    pub publish_at: Option<DateTime<Utc>>,
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
            description: resource.snippet.as_ref().map(|s| s.description.clone()).unwrap_or_default(),
            tags: resource.snippet.as_ref().map(|s| s.tags.clone()).unwrap_or_default(),
            category_id: resource.snippet.as_ref().map(|s| s.category_id.clone()).unwrap_or_default(),
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

/// A video resource with only its `status` part.
#[derive(Deserialize)]
pub(crate) struct StatusPart<T> {
    pub status: T,
}

/// The visibility in a `status` part.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VisibilityStatus {
    privacy_status: Privacy,
    publish_at: Option<DateTime<Utc>>,
}

impl From<VisibilityStatus> for Visibility {
    fn from(status: VisibilityStatus) -> Self {
        Visibility { privacy: status.privacy_status, publish_at: status.publish_at }
    }
}

/// A video resource with only its `snippet` part.
#[derive(Deserialize)]
pub(crate) struct SnippetPart<T> {
    pub snippet: T,
}

/// The details in a `snippet` part.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DetailsSnippet {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    category_id: String,
}

impl From<DetailsSnippet> for Details {
    fn from(snippet: DetailsSnippet) -> Self {
        Details {
            title: snippet.title,
            description: snippet.description,
            tags: snippet.tags,
            category_id: snippet.category_id,
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
        let json = r#"{"id":"abc","snippet":{"title":"T","channelId":"UC1","publishedAt":"2026-01-02T03:04:05Z",
            "description":"D","tags":["a","b"],"categoryId":"28"},
            "status":{"uploadStatus":"somethingNew","privacyStatus":"private","publishAt":"2026-02-01T00:00:00Z"},
            "processingDetails":{"processingStatus":"processing"}}"#;
        let video = VideoInfo::from(serde_json::from_str::<VideoResource>(json).unwrap());
        assert_eq!(video.upload_status, UploadStatus::Unknown);
        assert_eq!(video.processing_status, Some(ProcessingStatus::Processing));
        assert_eq!(video.privacy, Privacy::Private);
        assert_eq!(video.url, "https://www.youtube.com/watch?v=abc");
        assert_eq!(video.publish_at.unwrap().to_rfc3339(), "2026-02-01T00:00:00+00:00");
        assert_eq!(video.thumbnail, None);
        let details = Details {
            title: "T".into(),
            description: "D".into(),
            tags: vec!["a".into(), "b".into()],
            category_id: "28".into(),
        };
        assert_eq!(video.details(), details);
    }

    #[test]
    fn details_changes_are_checked_before_they_go() {
        assert!(DetailsChange::default().validate().is_ok());
        let check = |change: DetailsChange| change.validate();
        assert!(check(DetailsChange { title: Some("  ".into()), ..Default::default() }).is_err());
        assert!(check(DetailsChange { title: Some("é".repeat(100)), ..Default::default() }).is_ok());
        assert!(check(DetailsChange { title: Some("x".repeat(101)), ..Default::default() }).is_err());
        assert!(check(DetailsChange { description: Some(String::new()), ..Default::default() }).is_ok());
        assert!(check(DetailsChange { description: Some("a <b>".into()), ..Default::default() }).is_err());
        assert!(check(DetailsChange { description: Some("é".repeat(2501)), ..Default::default() }).is_err());
        assert!(check(DetailsChange { tags: Some(Vec::new()), ..Default::default() }).is_ok());
        assert!(
            check(DetailsChange { tags: Some(vec!["x".repeat(250), "y".repeat(250)]), ..Default::default() }).is_err()
        );
        assert!(check(DetailsChange { category_id: Some("27".into()), ..Default::default() }).is_ok());
        assert!(check(DetailsChange { category_id: Some(String::new()), ..Default::default() }).is_err());
    }

    #[test]
    fn userinfo_accepts_v2_id_field() {
        let user: GoogleUser = serde_json::from_str(r#"{"id":"123","email":"a@b.c","name":"A"}"#).unwrap();
        assert_eq!(user.sub, "123");
        assert_eq!(user.picture, None);
    }
}
