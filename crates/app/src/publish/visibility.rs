//! Who can watch videos on YouTube: what the Publish tab's Visibility section
//! chooses goes to the video once it is there. The section keeps the creator's
//! choice, and says so when YouTube has another.

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
    /// YouTube took the change but kept the video `privacy`.
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

/// What YouTube has once it did what `visibility` asks: a schedule past its time
/// made the video public.
fn by_now(visibility: Visibility) -> Visibility {
    match visibility.publish_at {
        Some(at) if at <= Utc::now() => Visibility { privacy: rushes_youtube::Privacy::Public, publish_at: None },
        _ => visibility,
    }
}

/// Whether `visibility` is what `video` last recorded YouTube having.
fn recorded(video: &Video, visibility: Visibility) -> bool {
    video.youtube.as_ref().is_some_and(|link| {
        same(Visibility { privacy: youtube_privacy(link.privacy), publish_at: link.publish_at }, visibility)
    })
}

/// Records what YouTube has for `video`, with the planner following: a video
/// YouTube schedules is scheduled, one anyone can watch is published. The form
/// keeps the creator's choice.
fn record(video: &mut Video, visibility: Visibility) {
    let privacy = core_privacy(visibility.privacy);
    if let Some(link) = video.youtube.as_mut() {
        link.privacy = privacy;
        link.publish_at = visibility.publish_at;
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

    /// Who can watch `video` on YouTube, as YouTube last said, when that isn't what
    /// its publish settings choose: a change that didn't get there (also in an
    /// earlier run of the app), or one made in YouTube Studio.
    pub fn other_visibility_on_youtube(&self, video: &Video) -> Option<Visibility> {
        let info = self.remote.get(&video.youtube.as_ref()?.video_id)?;
        let on_youtube = Visibility { privacy: info.privacy, publish_at: info.publish_at };
        (!same(by_now(chosen(video)), on_youtube)).then_some(on_youtube)
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
                // A newer choice goes next, and is recorded then.
                if let Some(mut video) = video.clone().filter(|_| !chosen_again) {
                    record(&mut video, now);
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

    /// Sends the visibility of `video_id` to the video its upload just put on YouTube
    /// as `uploaded`, when it changed while the video uploaded (also before a resume,
    /// which keeps the settings the upload started with) or YouTube has another.
    pub(super) fn send_visibility_after_upload(
        &mut self,
        video_id: VideoId,
        uploaded: Visibility,
        cx: &mut Context<Self>,
    ) {
        let changed = self.visibility_after_upload.remove(&video_id);
        let other = self.state.read(cx).video(video_id).is_some_and(|video| !same(chosen(video), uploaded));
        if changed || other {
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
    /// changes, and records its visibility (published in Studio, or at its
    /// scheduled time) on the videos linked to it. An answer asked before the last
    /// change, or while one is on its way, may predate it: the visibility that
    /// change left stays.
    pub(super) fn on_youtube_video(&mut self, mut info: VideoInfo, changes: u64, cx: &mut Context<Self>) {
        let youtube_id = info.id.clone();
        let current = self.visibility_changes(&youtube_id) == changes
            && self.visibility.get(&youtube_id) != Some(&VisibilitySync::Sending);
        if !current && let Some(known) = self.remote.get(&youtube_id) {
            info.privacy = known.privacy;
            info.publish_at = known.publish_at;
        }
        // Changed since, in YouTube Studio: what YouTube kept is no news anymore.
        if current
            && let Some(VisibilitySync::Kept(kept)) = self.visibility.get(&youtube_id)
            && *kept != core_privacy(info.privacy)
        {
            self.visibility.remove(&youtube_id);
        }
        let visibility = Visibility { privacy: info.privacy, publish_at: info.publish_at };
        self.remote.insert(youtube_id.clone(), info);
        cx.notify();
        if !current {
            return;
        }
        let changed: Vec<Video> = self
            .state
            .read(cx)
            .rows()
            .iter()
            .map(|row| &row.video)
            .filter(|video| video.youtube.as_ref().is_some_and(|link| link.video_id == youtube_id))
            .filter(|video| !recorded(video, visibility))
            .map(|video| {
                let mut changed = video.clone();
                record(&mut changed, visibility);
                changed
            })
            .collect();
        if !changed.is_empty() {
            self.state.update(cx, |state, cx| {
                for video in changed {
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
            publish_at: None,
        });
        video
    }

    #[test]
    fn the_planner_follows_youtube() {
        let tomorrow = Utc::now() + TimeDelta::days(1);
        let mut video = uploaded(Privacy::Private, Some(tomorrow));
        video.status = VideoStatus::Ready;
        record(&mut video, Visibility { privacy: rushes_youtube::Privacy::Private, publish_at: Some(tomorrow) });
        assert_eq!(video.status, VideoStatus::Scheduled);
        assert_eq!(video.publish_date, Some(tomorrow.with_timezone(&Local).date_naive()));

        // No schedule anymore: ready again, until anyone can watch it.
        record(&mut video, Visibility { privacy: rushes_youtube::Privacy::Private, publish_at: None });
        assert_eq!(video.status, VideoStatus::Ready);
        record(&mut video, Visibility { privacy: rushes_youtube::Privacy::Unlisted, publish_at: None });
        assert_eq!(video.youtube.as_ref().map(|link| link.privacy), Some(Privacy::Unlisted));
        assert_eq!(video.status, VideoStatus::Published);
        // A published video made private stays published in the planner.
        record(&mut video, Visibility { privacy: rushes_youtube::Privacy::Private, publish_at: None });
        assert_eq!(video.status, VideoStatus::Published);
        // The form keeps the creator's choice.
        assert_eq!((video.publish.privacy, video.publish.publish_at), (Privacy::Private, Some(tomorrow)));
    }

    #[test]
    fn what_youtube_has_is_recorded_once() {
        let tomorrow = Utc::now() + TimeDelta::days(1);
        let mut video = uploaded(Privacy::Private, Some(tomorrow));
        let scheduled = Visibility { privacy: rushes_youtube::Privacy::Private, publish_at: Some(tomorrow) };
        // Recorded before Rushes kept schedules.
        assert!(!recorded(&video, scheduled));
        record(&mut video, scheduled);
        assert!(recorded(&video, scheduled));
        // Published at its time.
        assert!(!recorded(&video, Visibility { privacy: rushes_youtube::Privacy::Public, publish_at: None }));
    }

    #[test]
    fn schedules_count_to_the_second_and_end_public() {
        let at = DateTime::parse_from_rfc3339("2031-05-06T07:08:09.5Z").unwrap().to_utc();
        let youtube = Visibility {
            privacy: rushes_youtube::Privacy::Private,
            publish_at: Some(DateTime::parse_from_rfc3339("2031-05-06T07:08:09Z").unwrap().to_utc()),
        };
        assert!(same(by_now(chosen(&uploaded(Privacy::Private, Some(at)))), youtube));
        // YouTube made it public at its time.
        let past = uploaded(Privacy::Private, Some(Utc::now() - TimeDelta::hours(1)));
        let public = Visibility { privacy: rushes_youtube::Privacy::Public, publish_at: None };
        assert!(same(by_now(chosen(&past)), public));
    }
}
