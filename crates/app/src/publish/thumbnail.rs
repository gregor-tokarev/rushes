//! Custom thumbnails of videos on YouTube: set when an upload finishes, and when
//! the creator saves another image chosen for a video that is already there.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::{AppContext as _, Context};
use rushes_core::{UploadId, UploadStatus, Video, VideoId};
use rushes_youtube::{Error as YoutubeError, YoutubeClient};

use super::service::{YoutubeService, describe_error};
use crate::ui::{notification, push_notification};

/// Setting the custom thumbnail of a video on YouTube, for this run of the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThumbnailSync {
    /// The image is on its way.
    Sending(PathBuf),
    /// YouTube didn't take the image; `problem` says why, as the creator should read it.
    Failed { image: PathBuf, problem: String },
}

/// Where a thumbnail goes: a video on YouTube, linked from a video of the app.
pub(super) struct ThumbnailTarget {
    pub video_id: VideoId,
    /// The YouTube video id.
    pub youtube_id: String,
    /// The account the YouTube video belongs to, by key.
    pub channel: String,
}

/// What is sent as a thumbnail: the image, as the form chose it last.
struct SentImage {
    path: PathBuf,
    /// How many images had been chosen for the video when it went.
    choice: u64,
}

impl YoutubeService {
    /// Where setting the thumbnail of the YouTube video `youtube_id` stands, while the
    /// image is on its way or after YouTube didn't take it.
    pub fn thumbnail_sync(&self, youtube_id: &str) -> Option<&ThumbnailSync> {
        self.thumbnails.get(youtube_id)
    }

    /// Why the custom thumbnail of a finished upload isn't set.
    pub fn thumbnail_problem(&self, id: UploadId) -> Option<&str> {
        let (UploadStatus::Processing { video_id: youtube_id } | UploadStatus::Done { video_id: youtube_id }) =
            &self.job(id)?.status
        else {
            return None;
        };
        match self.thumbnails.get(youtube_id)? {
            ThumbnailSync::Failed { problem, .. } => Some(problem),
            ThumbnailSync::Sending(_) => None,
        }
    }

    /// The form chose `image` for `video_id`, or none. The file YouTube's thumbnail
    /// was set from, chosen again, may have new contents: the video no longer
    /// records YouTube having it, so saving sends it, also after a restart.
    pub fn choose_youtube_thumbnail(&mut self, video_id: VideoId, image: Option<&Path>, cx: &mut Context<Self>) {
        *self.thumbnail_choices.entry(video_id).or_default() += 1;
        let Some(mut video) = self.state.read(cx).video(video_id).cloned() else { return };
        if let (Some(image), Some(link)) = (image, video.youtube.as_mut())
            && link.thumbnail.as_deref() == Some(image)
        {
            link.thumbnail = None;
            self.state.update(cx, |state, cx| state.update_video(video, cx));
        }
        cx.notify();
    }

    /// Whether saving sends the image `video` chooses to its video on YouTube: the
    /// video doesn't record YouTube having it.
    pub fn unsaved_thumbnail(&self, video: &Video) -> bool {
        let (Some(link), Some(image)) = (&video.youtube, &video.publish.thumbnail) else { return false };
        link.thumbnail.as_ref() != Some(image)
    }

    /// Sets the image chosen for `video_id` as the custom thumbnail of its video on
    /// YouTube. An image chosen while another is on its way follows that one. While
    /// the video uploads again, the upload takes the image to its new video instead.
    pub fn set_youtube_thumbnail(&mut self, video_id: VideoId, cx: &mut Context<Self>) {
        if self.is_uploading_any(&[video_id]) {
            return;
        }
        let Some(video) = self.state.read(cx).video(video_id) else { return };
        let (Some(link), Some(image)) = (video.youtube.clone(), video.publish.thumbnail.clone()) else { return };
        if matches!(self.thumbnails.get(&link.video_id), Some(ThumbnailSync::Sending(_))) {
            self.thumbnails_chosen_again.insert(link.video_id);
            return;
        }
        let target = ThumbnailTarget { video_id, youtube_id: link.video_id, channel: link.channel_id };
        match self.client_for(&target.channel) {
            Some(client) => self.send_thumbnail(target, client, image, false, cx),
            None => {
                let problem = match self.find_account(&target.channel) {
                    Some(account) => format!("Sign in to {} again, then try again.", account.name()),
                    None => "Connect the YouTube account the video is on, then try again.".to_owned(),
                };
                self.thumbnails.insert(target.youtube_id, ThumbnailSync::Failed { image, problem });
                cx.notify();
            }
        }
    }

    /// Sends `image` as the thumbnail of `target`. `announce` also tells a failure in
    /// a notification: when an upload finishes, the creator may be on another page.
    /// Custom thumbnails need a verified channel; failing to set one doesn't fail an upload.
    pub(super) fn send_thumbnail(
        &mut self,
        target: ThumbnailTarget,
        client: Arc<YoutubeClient>,
        image: PathBuf,
        announce: bool,
        cx: &mut Context<Self>,
    ) {
        self.thumbnails.insert(target.youtube_id.clone(), ThumbnailSync::Sending(image.clone()));
        cx.notify();
        let choice = self.thumbnail_choices.get(&target.video_id).copied().unwrap_or_default();
        cx.spawn(async move |this, cx| {
            let (youtube_id, sent) = (target.youtube_id.clone(), image.clone());
            let result = cx.background_spawn(async move { client.set_thumbnail(&youtube_id, &sent) }).await;
            let sent = SentImage { path: image, choice };
            this.update(cx, |this, cx| this.on_thumbnail_sent(target, sent, result.map(drop), announce, cx)).ok();
        })
        .detach();
    }

    fn on_thumbnail_sent(
        &mut self,
        target: ThumbnailTarget,
        sent: SentImage,
        result: Result<(), YoutubeError>,
        announce: bool,
        cx: &mut Context<Self>,
    ) {
        let image = sent.path;
        // An image chosen since (maybe the same file with new contents) isn't what went.
        let chosen_since = self.thumbnail_choices.get(&target.video_id).copied().unwrap_or_default() != sent.choice;
        let video = self.state.read(cx).video(target.video_id).cloned();
        // The video may have gone, or uploaded again to another YouTube video.
        let video = video.filter(|video| video.youtube.as_ref().is_some_and(|link| link.video_id == target.youtube_id));
        match result {
            Ok(()) => {
                self.thumbnails.remove(&target.youtube_id);
                if !chosen_since
                    && let Some(mut video) = video.clone()
                    && let Some(link) = video.youtube.as_mut()
                    && link.thumbnail.as_ref() != Some(&image)
                {
                    link.thumbnail = Some(image.clone());
                    self.state.update(cx, |state, cx| state.update_video(video, cx));
                }
            }
            Err(error) => {
                let problem = describe_error(&error);
                if error.needs_sign_in() {
                    self.require_sign_in(&target.channel, cx);
                }
                self.thumbnails.insert(
                    target.youtube_id.clone(),
                    ThumbnailSync::Failed { image: image.clone(), problem: problem.clone() },
                );
                if announce {
                    let title =
                        video.as_ref().map_or_else(|| "Untitled".to_owned(), |video| video.upload_title().to_owned());
                    push_notification(cx, move |cx| {
                        notification::warning(problem, cx).title(format!("The thumbnail of “{title}” wasn’t set"))
                    });
                }
            }
        }
        cx.notify();
        // An image saved while this one was on its way follows it.
        let chosen_again = self.thumbnails_chosen_again.remove(&target.youtube_id);
        if video.is_some() && chosen_again {
            self.set_youtube_thumbnail(target.video_id, cx);
        }
    }
}
