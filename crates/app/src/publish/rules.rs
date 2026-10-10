//! YouTube's limits on upload metadata, checked before any quota is spent.

use std::fmt;

use chrono::{DateTime, Utc};
use videocrm_core::{Privacy, Video};
use videocrm_youtube::VideoMetadata;

pub const TITLE_MAX_CHARS: usize = 100;
pub const DESCRIPTION_MAX_BYTES: usize = 5000;
pub const TAGS_MAX_CHARS: usize = 500;
/// YouTube rejects larger custom thumbnails.
pub const THUMBNAIL_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// A video category of the YouTube Data API (`snippet.categoryId`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Category {
    pub id: &'static str,
    pub label: &'static str,
}

/// The categories YouTube offers for uploads in every region.
pub const CATEGORIES: [Category; 15] = [
    Category { id: "1", label: "Film & Animation" },
    Category { id: "2", label: "Autos & Vehicles" },
    Category { id: "10", label: "Music" },
    Category { id: "15", label: "Pets & Animals" },
    Category { id: "17", label: "Sports" },
    Category { id: "19", label: "Travel & Events" },
    Category { id: "20", label: "Gaming" },
    Category { id: "22", label: "People & Blogs" },
    Category { id: "23", label: "Comedy" },
    Category { id: "24", label: "Entertainment" },
    Category { id: "25", label: "News & Politics" },
    Category { id: "26", label: "Howto & Style" },
    Category { id: "27", label: "Education" },
    Category { id: "28", label: "Science & Technology" },
    Category { id: "29", label: "Nonprofits & Activism" },
];

/// Category used when the video has none: YouTube's own default for uploads.
pub const DEFAULT_CATEGORY: &str = "22";

/// Length of the tags the way YouTube counts it: characters, plus the separators
/// between tags and the quotes around tags that contain a space.
pub fn tags_length(tags: &[String]) -> usize {
    let quoted: usize = tags.iter().map(|tag| tag.chars().count() + if tag.contains(' ') { 2 } else { 0 }).sum();
    quoted + tags.len().saturating_sub(1)
}

/// Whether uploads can use the YouTube account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountAccess {
    Connected,
    /// The account is known, but YouTube wants a new sign-in.
    NeedsSignIn,
    NotConnected,
}

/// Why a video cannot be uploaded yet. `Display` is the sentence shown to the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockingReason {
    NotConnected,
    NeedsSignIn,
    NoFile,
    FileMissing,
    EmptyTitle,
    TitleTooLong(usize),
    TitleHasBrackets,
    DescriptionTooLong(usize),
    DescriptionHasBrackets,
    TagsTooLong(usize),
    TagsHaveBrackets,
    ScheduledNotPrivate,
    ScheduleInPast,
}

impl fmt::Display for BlockingReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlockingReason::NotConnected => write!(f, "Connect a YouTube account."),
            BlockingReason::NeedsSignIn => write!(f, "Sign in to YouTube again to upload."),
            BlockingReason::NoFile => write!(f, "Choose the video file to upload."),
            BlockingReason::FileMissing => write!(f, "The chosen video file no longer exists. Choose it again."),
            BlockingReason::EmptyTitle => write!(f, "Add a title."),
            BlockingReason::TitleTooLong(len) => {
                write!(f, "Shorten the title to {TITLE_MAX_CHARS} characters (it has {len}).")
            }
            BlockingReason::TitleHasBrackets => write!(f, "Remove < and > from the title."),
            BlockingReason::DescriptionTooLong(len) => {
                write!(f, "Shorten the description to 5,000 bytes (it has {}).", group_thousands(*len))
            }
            BlockingReason::DescriptionHasBrackets => write!(f, "Remove < and > from the description."),
            BlockingReason::TagsTooLong(len) => {
                write!(f, "Shorten the tags to {TAGS_MAX_CHARS} characters in total (they have {len}).")
            }
            BlockingReason::TagsHaveBrackets => write!(f, "Remove < and > from the tags."),
            BlockingReason::ScheduledNotPrivate => write!(f, "Scheduled videos must stay private until they go live."),
            BlockingReason::ScheduleInPast => write!(f, "Pick a publish time in the future."),
        }
    }
}

/// Everything that stops `video` from being uploaded right now, in form order.
pub fn blocking_reasons(
    video: &Video,
    account: AccountAccess,
    file_exists: bool,
    now: DateTime<Utc>,
) -> Vec<BlockingReason> {
    let publish = &video.publish;
    let has_brackets = |text: &str| text.contains(['<', '>']);
    let mut reasons = Vec::new();
    match account {
        AccountAccess::Connected => {}
        AccountAccess::NeedsSignIn => reasons.push(BlockingReason::NeedsSignIn),
        AccountAccess::NotConnected => reasons.push(BlockingReason::NotConnected),
    }
    match (&publish.file, file_exists) {
        (None, _) => reasons.push(BlockingReason::NoFile),
        (Some(_), false) => reasons.push(BlockingReason::FileMissing),
        (Some(_), true) => {}
    }
    let title = video.upload_title();
    let title_len = title.chars().count();
    if title.trim().is_empty() {
        reasons.push(BlockingReason::EmptyTitle);
    } else if title_len > TITLE_MAX_CHARS {
        reasons.push(BlockingReason::TitleTooLong(title_len));
    }
    if has_brackets(title) {
        reasons.push(BlockingReason::TitleHasBrackets);
    }
    if publish.description.len() > DESCRIPTION_MAX_BYTES {
        reasons.push(BlockingReason::DescriptionTooLong(publish.description.len()));
    }
    if has_brackets(&publish.description) {
        reasons.push(BlockingReason::DescriptionHasBrackets);
    }
    let tags_len = tags_length(&publish.tags);
    if tags_len > TAGS_MAX_CHARS {
        reasons.push(BlockingReason::TagsTooLong(tags_len));
    }
    if publish.tags.iter().any(|tag| has_brackets(tag)) {
        reasons.push(BlockingReason::TagsHaveBrackets);
    }
    if let Some(publish_at) = publish.publish_at {
        if publish.privacy != Privacy::Private {
            reasons.push(BlockingReason::ScheduledNotPrivate);
        }
        if publish_at <= now {
            reasons.push(BlockingReason::ScheduleInPast);
        }
    }
    reasons
}

/// The `videos.insert` metadata for `video`.
pub fn metadata(video: &Video) -> VideoMetadata {
    let publish = &video.publish;
    VideoMetadata {
        title: video.upload_title().trim().to_owned(),
        description: publish.description.clone(),
        tags: publish.tags.clone(),
        category_id: if publish.category_id.is_empty() {
            DEFAULT_CATEGORY.to_owned()
        } else {
            publish.category_id.clone()
        },
        privacy: youtube_privacy(publish.privacy),
        publish_at: publish.publish_at,
        made_for_kids: publish.made_for_kids,
        default_language: None,
    }
}

pub fn youtube_privacy(privacy: Privacy) -> videocrm_youtube::Privacy {
    match privacy {
        Privacy::Private => videocrm_youtube::Privacy::Private,
        Privacy::Unlisted => videocrm_youtube::Privacy::Unlisted,
        Privacy::Public => videocrm_youtube::Privacy::Public,
    }
}

pub fn core_privacy(privacy: videocrm_youtube::Privacy) -> Privacy {
    match privacy {
        videocrm_youtube::Privacy::Private => Privacy::Private,
        videocrm_youtube::Privacy::Unlisted => Privacy::Unlisted,
        videocrm_youtube::Privacy::Public => Privacy::Public,
    }
}

/// `5000` -> `5,000`.
pub fn group_thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (ix, digit) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::*;

    fn ready_video() -> Video {
        let mut video = Video::new("Rust in 100 seconds");
        video.publish.file = Some("/videos/final.mp4".into());
        video
    }

    #[test]
    fn a_complete_video_has_no_blocking_reasons() {
        assert_eq!(blocking_reasons(&ready_video(), AccountAccess::Connected, true, Utc::now()), Vec::new());
    }

    #[test]
    fn reports_connection_and_file_problems() {
        let mut video = ready_video();
        assert_eq!(
            blocking_reasons(&video, AccountAccess::NotConnected, false, Utc::now()),
            vec![BlockingReason::NotConnected, BlockingReason::FileMissing]
        );
        // An expired sign-in says so, rather than asking to connect an account it knows.
        assert_eq!(
            blocking_reasons(&video, AccountAccess::NeedsSignIn, true, Utc::now()),
            vec![BlockingReason::NeedsSignIn]
        );
        assert_eq!(BlockingReason::NeedsSignIn.to_string(), "Sign in to YouTube again to upload.");
        video.publish.file = None;
        assert_eq!(blocking_reasons(&video, AccountAccess::Connected, false, Utc::now()), vec![BlockingReason::NoFile]);
    }

    #[test]
    fn title_falls_back_to_the_video_title_and_is_limited_to_100_characters() {
        let mut video = ready_video();
        video.title = "   ".into();
        assert_eq!(
            blocking_reasons(&video, AccountAccess::Connected, true, Utc::now()),
            vec![BlockingReason::EmptyTitle]
        );

        video.publish.title = "é".repeat(100);
        assert!(blocking_reasons(&video, AccountAccess::Connected, true, Utc::now()).is_empty());
        video.publish.title.push('x');
        assert_eq!(
            blocking_reasons(&video, AccountAccess::Connected, true, Utc::now()),
            vec![BlockingReason::TitleTooLong(101)]
        );

        video.publish.title = "a <b> title".into();
        assert_eq!(
            blocking_reasons(&video, AccountAccess::Connected, true, Utc::now()),
            vec![BlockingReason::TitleHasBrackets]
        );
    }

    #[test]
    fn description_is_limited_in_bytes() {
        let mut video = ready_video();
        // 2,500 two-byte characters fit; one more ASCII byte does not.
        video.publish.description = "é".repeat(2500);
        assert!(blocking_reasons(&video, AccountAccess::Connected, true, Utc::now()).is_empty());
        video.publish.description.push('.');
        assert_eq!(
            blocking_reasons(&video, AccountAccess::Connected, true, Utc::now()),
            vec![BlockingReason::DescriptionTooLong(5001)]
        );
        assert_eq!(
            BlockingReason::DescriptionTooLong(5001).to_string(),
            "Shorten the description to 5,000 bytes (it has 5,001)."
        );

        video.publish.description = "1 < 2".into();
        assert_eq!(
            blocking_reasons(&video, AccountAccess::Connected, true, Utc::now()),
            vec![BlockingReason::DescriptionHasBrackets]
        );
    }

    #[test]
    fn tags_count_separators_and_quotes() {
        assert_eq!(tags_length(&[]), 0);
        assert_eq!(tags_length(&["rust".into()]), 4);
        // "rust,"desktop app"" = 4 + 1 + 13
        assert_eq!(tags_length(&["rust".into(), "desktop app".into()]), 18);

        let mut video = ready_video();
        video.publish.tags = vec!["x".repeat(250), "y".repeat(249)];
        assert!(blocking_reasons(&video, AccountAccess::Connected, true, Utc::now()).is_empty());
        video.publish.tags[1].push('y');
        assert_eq!(
            blocking_reasons(&video, AccountAccess::Connected, true, Utc::now()),
            vec![BlockingReason::TagsTooLong(501)]
        );

        video.publish.tags = vec!["<script>".into()];
        assert_eq!(
            blocking_reasons(&video, AccountAccess::Connected, true, Utc::now()),
            vec![BlockingReason::TagsHaveBrackets]
        );
    }

    #[test]
    fn scheduling_requires_private_and_a_future_time() {
        let now = Utc::now();
        let mut video = ready_video();
        video.publish.publish_at = Some(now + TimeDelta::hours(1));
        assert!(blocking_reasons(&video, AccountAccess::Connected, true, now).is_empty());

        video.publish.privacy = Privacy::Public;
        assert_eq!(
            blocking_reasons(&video, AccountAccess::Connected, true, now),
            vec![BlockingReason::ScheduledNotPrivate]
        );

        video.publish.privacy = Privacy::Private;
        video.publish.publish_at = Some(now);
        assert_eq!(blocking_reasons(&video, AccountAccess::Connected, true, now), vec![BlockingReason::ScheduleInPast]);
    }

    #[test]
    fn metadata_maps_the_publish_settings() {
        let mut video = ready_video();
        video.publish.description = "About ownership".into();
        video.publish.tags = vec!["rust".into()];
        video.publish.privacy = Privacy::Unlisted;
        video.publish.made_for_kids = true;
        let metadata = metadata(&video);
        assert_eq!(metadata.title, "Rust in 100 seconds");
        assert_eq!(metadata.category_id, DEFAULT_CATEGORY);
        assert_eq!(metadata.privacy, videocrm_youtube::Privacy::Unlisted);
        assert!(metadata.made_for_kids);
        assert!(metadata.validate().is_ok());

        video.publish.category_id = "28".into();
        assert_eq!(super::metadata(&video).category_id, "28");
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1234567), "1,234,567");
    }
}
