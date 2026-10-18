//! Who can watch videos on YouTube: what the Publish tab's Visibility section
//! chooses goes to the video once it is there, and what YouTube has comes back to it.

use chrono::{DateTime, Local, Utc};
use gpui_kit::{AppContext as _, Context};
use rushes_core::{Privacy, Video, VideoId, VideoStatus};
use rushes_youtube::{Error as YoutubeError, MANAGE_SCOPE, VideoInfo, Visibility};

use super::rules::{core_privacy, youtube_privacy};
use super::service::{YoutubeAccount, YoutubeService, describe_error};

/// Changing who can watch a video on YouTube, for this run of the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VisibilitySync {
    /// The change is on its way.
    Sending,
    /// The change didn't reach YouTube; `problem` says why, as the creator should read it.
    Failed {
        problem: String,
        /// The account to sign in to again, when that is what it takes, by key.
        reconnect: Option<String>,
    },
    /// YouTube took the change but kept the video `privacy`, which the form shows now.
    Kept(Privacy),
}

/// What `video` asks YouTube for.
fn chosen(video: &Video) -> Visibility {
    Visibility { privacy: youtube_privacy(video.publish.privacy), publish_at: video.publish.publish_at }
}

/// Whether `a` and `b` are the same as far as YouTube is concerned: it keeps
/// schedules to the second.
fn same(a: Visibility, b: Visibility) -> bool {
    a.privacy == b.privacy && a.publish_at.map(|at| at.timestamp()) == b.publish_at.map(|at| at.timestamp())
}

/// Whether YouTube has another visibility for `video` than the app knows of.
fn differs(video: &Video, visibility: Visibility) -> bool {
    !same(chosen(video), visibility)
        || video.youtube.as_ref().is_some_and(|link| link.privacy != core_privacy(visibility.privacy))
}

/// The video's visibility as YouTube has it, with the planner following: a video
/// YouTube schedules is scheduled, one anyone can watch is published.
fn take_visibility(video: &mut Video, visibility: Visibility) {
    let privacy = core_privacy(visibility.privacy);
    if !same(chosen(video), visibility) {
        video.publish.privacy = privacy;
        video.publish.publish_at = visibility.publish_at;
    }
    if let Some(link) = video.youtube.as_mut() {
        link.privacy = privacy;
    }
    plan(video, privacy, visibility.publish_at);
}

fn plan(video: &mut Video, privacy: Privacy, publish_at: Option<DateTime<Utc>>) {
    if let Some(at) = publish_at {
        video.status = VideoStatus::Scheduled;
        video.publish_date = Some(at.with_timezone(&Local).date_naive());
    } else if privacy != Privacy::Private {
        video.status = VideoStatus::Published;
    } else if video.status == VideoStatus::Scheduled {
        video.status = VideoStatus::Ready;
    }
}

impl YoutubeService {
    /// Where changing who can watch the YouTube video `youtube_id` stands, while the
    /// change is on its way or after YouTube didn't make it.
    pub fn visibility_sync(&self, youtube_id: &str) -> Option<&VisibilitySync> {
        self.visibility.get(youtube_id)
    }

    /// Changes who can watch the YouTube video of `video_id` to what its publish
    /// settings say. A change made while another is on its way follows that one; one
    /// made while the video uploads again goes to its new video.
    pub fn set_youtube_visibility(&mut self, video_id: VideoId, cx: &mut Context<Self>) {
        if self.is_uploading_any(&[video_id]) {
            // The upload sent the settings it started with.
            self.visibility_after_upload.insert(video_id);
            return;
        }
        let Some(video) = self.state.read(cx).video(video_id) else { return };
        let Some(link) = video.youtube.clone() else { return };
        let visibility = chosen(video);
        let youtube_id = link.video_id;
        if self.visibility.get(&youtube_id) == Some(&VisibilitySync::Sending) {
            self.visibility_changed_again.insert(youtube_id);
            return;
        }
        let channel = link.channel_id;
        let client = self.client_for(&channel);
        let name = self.find_account(&channel).map(YoutubeAccount::name);
        let failed = |problem: String, reconnect: bool| VisibilitySync::Failed {
            problem,
            reconnect: reconnect.then(|| channel.clone()),
        };
        let refused = match (&client, name) {
            // Sign-ins from before Rushes changed videos on YouTube didn't allow it.
            (Some(client), Some(name)) if !client.credentials().has_scope(MANAGE_SCOPE) => Some(failed(
                format!(
                    "Reconnect {name} to let Rushes change videos on YouTube, and leave every permission box ticked."
                ),
                true,
            )),
            (Some(_), _) => None,
            (None, Some(name)) => Some(failed(format!("Reconnect {name} to send the change."), true)),
            (None, None) => {
                Some(failed("Connect the YouTube account the video is on, then try again.".to_owned(), false))
            }
        };
        if let Some(refused) = refused {
            self.visibility.insert(youtube_id, refused);
            cx.notify();
            return;
        }
        let Some(client) = client else { return };
        self.visibility.insert(youtube_id.clone(), VisibilitySync::Sending);
        self.visibility_changed(&youtube_id);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let id = youtube_id.clone();
            let result = cx.background_spawn(async move { client.set_visibility(&id, visibility) }).await;
            this.update(cx, |this, cx| this.on_visibility_sent(video_id, youtube_id, channel, visibility, result, cx))
                .ok();
        })
        .detach();
    }

    fn on_visibility_sent(
        &mut self,
        video_id: VideoId,
        youtube_id: String,
        channel: String,
        sent: Visibility,
        result: Result<Option<Visibility>, YoutubeError>,
        cx: &mut Context<Self>,
    ) {
        self.visibility.remove(&youtube_id);
        self.visibility_changed(&youtube_id);
        let video = self.state.read(cx).video(video_id).cloned();
        // The video may have gone, or uploaded again to another YouTube video.
        let video = video.filter(|video| video.youtube.as_ref().is_some_and(|link| link.video_id == youtube_id));
        // A change made while this one was on its way: chosen again, or saved since.
        let chosen_again = self.visibility_changed_again.remove(&youtube_id)
            || video.as_ref().is_some_and(|video| !same(chosen(video), sent));
        match result {
            Ok(Some(now)) => {
                if let Some(info) = self.remote.get_mut(&youtube_id) {
                    info.privacy = now.privacy;
                    info.publish_at = now.publish_at;
                }
                if !same(now, sent) {
                    self.visibility.insert(youtube_id.clone(), VisibilitySync::Kept(core_privacy(now.privacy)));
                }
                // A newer choice is the one to keep; it goes next.
                if let Some(mut video) = video.clone().filter(|_| !chosen_again) {
                    take_visibility(&mut video, now);
                    self.state.update(cx, |state, cx| state.update_video(video, cx));
                }
            }
            Ok(None) => self.on_video_gone(&youtube_id, &channel, cx),
            Err(error) => {
                let problem = describe_error(&error);
                let reconnect = error.needs_sign_in().then(|| channel.clone());
                if reconnect.is_some() {
                    self.require_sign_in(&channel, cx);
                }
                self.visibility.insert(youtube_id, VisibilitySync::Failed { problem, reconnect });
            }
        }
        cx.notify();
        if video.is_some() && chosen_again {
            self.set_youtube_visibility(video_id, cx);
        }
    }

    /// Sends the visibility of `video_id` once its upload ended, when it changed meanwhile:
    /// to the new video, or to the one already there when the upload didn't finish.
    pub(super) fn send_visibility_after_upload(&mut self, video_id: VideoId, cx: &mut Context<Self>) {
        if self.visibility_after_upload.remove(&video_id) {
            self.set_youtube_visibility(video_id, cx);
        }
    }

    /// Sends again the changes that waited for the account `key` to sign in again.
    pub(super) fn send_visibility_after_sign_in(&mut self, key: &str, cx: &mut Context<Self>) {
        let waiting: Vec<VideoId> = self
            .state
            .read(cx)
            .rows()
            .iter()
            .map(|row| &row.video)
            .filter(|video| {
                video.youtube.as_ref().is_some_and(|link| {
                    link.channel_id == key
                        && matches!(
                            self.visibility.get(&link.video_id),
                            Some(VisibilitySync::Failed { reconnect: Some(_), .. })
                        )
                })
            })
            .map(|video| video.id)
            .collect();
        for video_id in waiting {
            self.set_youtube_visibility(video_id, cx);
        }
    }

    /// Counts a change sent to `youtube_id`: what YouTube said about it before
    /// the change is out of date.
    fn visibility_changed(&mut self, youtube_id: &str) {
        *self.visibility_changes.entry(youtube_id.to_owned()).or_default() += 1;
    }

    /// How many changes went to `youtube_id`, to tell answers asked before one.
    pub(super) fn visibility_changes(&self, youtube_id: &str) -> u64 {
        self.visibility_changes.get(youtube_id).copied().unwrap_or_default()
    }

    /// Keeps what YouTube said about a video, asked after `changes` visibility
    /// changes. The Visibility section takes what YouTube has, unless a change of
    /// the creator's is on its way or didn't reach YouTube, or the answer is older
    /// than the last change.
    pub(super) fn on_youtube_video(&mut self, mut info: VideoInfo, changes: u64, cx: &mut Context<Self>) {
        let youtube_id = info.id.clone();
        let current = self.visibility_changes(&youtube_id) == changes
            && !matches!(
                self.visibility.get(&youtube_id),
                Some(VisibilitySync::Sending | VisibilitySync::Failed { .. })
            );
        if !current && let Some(known) = self.remote.get(&youtube_id) {
            info.privacy = known.privacy;
            info.publish_at = known.publish_at;
        }
        let visibility = Visibility { privacy: info.privacy, publish_at: info.publish_at };
        self.remote.insert(youtube_id.clone(), info);
        cx.notify();
        if !current {
            return;
        }
        // Changed since, in YouTube Studio: what YouTube kept is no news anymore.
        if let Some(VisibilitySync::Kept(kept)) = self.visibility.get(&youtube_id)
            && *kept != core_privacy(visibility.privacy)
        {
            self.visibility.remove(&youtube_id);
        }
        let taken: Vec<Video> = self
            .state
            .read(cx)
            .rows()
            .iter()
            .map(|row| &row.video)
            .filter(|video| video.youtube.as_ref().is_some_and(|link| link.video_id == youtube_id))
            // An upload under way takes the form's visibility to its new video.
            .filter(|video| !self.is_uploading_any(&[video.id]))
            .filter(|video| differs(video, visibility))
            .map(|video| {
                let mut taken = video.clone();
                take_visibility(&mut taken, visibility);
                taken
            })
            .collect();
        if !taken.is_empty() {
            self.state.update(cx, |state, cx| {
                for video in taken {
                    state.update_video(video, cx);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use rushes_core::YoutubeLink;

    use super::*;

    fn uploaded(privacy: Privacy, publish_at: Option<DateTime<Utc>>) -> Video {
        let mut video = Video::new("Rust Ownership Explained");
        video.publish.privacy = privacy;
        video.publish.publish_at = publish_at;
        video.youtube = Some(YoutubeLink {
            video_id: "abc".into(),
            url: "https://www.youtube.com/watch?v=abc".into(),
            channel_id: "UC1".into(),
            privacy,
            uploaded_at: Utc::now(),
            thumbnail: None,
        });
        video
    }

    #[test]
    fn the_planner_follows_youtube() {
        let tomorrow = Utc::now() + TimeDelta::days(1);
        let mut video = uploaded(Privacy::Private, None);
        video.status = VideoStatus::Ready;
        take_visibility(
            &mut video,
            Visibility { privacy: rushes_youtube::Privacy::Private, publish_at: Some(tomorrow) },
        );
        assert_eq!((video.publish.privacy, video.publish.publish_at), (Privacy::Private, Some(tomorrow)));
        assert_eq!(video.status, VideoStatus::Scheduled);
        assert_eq!(video.publish_date, Some(tomorrow.with_timezone(&Local).date_naive()));

        // No schedule anymore: ready again, until anyone can watch it.
        take_visibility(&mut video, Visibility { privacy: rushes_youtube::Privacy::Private, publish_at: None });
        assert_eq!(video.status, VideoStatus::Ready);
        take_visibility(&mut video, Visibility { privacy: rushes_youtube::Privacy::Unlisted, publish_at: None });
        assert_eq!(video.publish.privacy, Privacy::Unlisted);
        assert_eq!(video.youtube.as_ref().map(|link| link.privacy), Some(Privacy::Unlisted));
        assert_eq!(video.status, VideoStatus::Published);
        // A published video made private stays published in the planner.
        take_visibility(&mut video, Visibility { privacy: rushes_youtube::Privacy::Private, publish_at: None });
        assert_eq!(video.status, VideoStatus::Published);
    }

    #[test]
    fn only_another_visibility_is_news() {
        let video = uploaded(Privacy::Public, None);
        assert!(!differs(&video, Visibility { privacy: rushes_youtube::Privacy::Public, publish_at: None }));
        assert!(differs(&video, Visibility { privacy: rushes_youtube::Privacy::Unlisted, publish_at: None }));
        // Uploaded as unlisted, kept private by YouTube.
        let mut kept = uploaded(Privacy::Unlisted, None);
        if let Some(link) = kept.youtube.as_mut() {
            link.privacy = Privacy::Private;
        }
        assert!(differs(&kept, Visibility { privacy: rushes_youtube::Privacy::Private, publish_at: None }));
    }

    #[test]
    fn schedules_count_to_the_second() {
        let at = DateTime::parse_from_rfc3339("2031-05-06T07:08:09.5Z").unwrap().to_utc();
        let mut video = uploaded(Privacy::Private, Some(at));
        let youtube = Visibility {
            privacy: rushes_youtube::Privacy::Private,
            publish_at: Some(DateTime::parse_from_rfc3339("2031-05-06T07:08:09Z").unwrap().to_utc()),
        };
        assert!(same(chosen(&video), youtube));
        take_visibility(&mut video, youtube);
        assert_eq!(video.publish.publish_at, Some(at), "the creator's time stays");
    }
}
