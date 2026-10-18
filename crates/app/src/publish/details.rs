//! What viewers read about videos on YouTube: the title, description, tags and
//! category the Publish tab's Details section chooses go to the video once it is
//! there, a moment after the last change, so a description typed in bursts goes
//! once. Only the details that changed go; the others stay as YouTube has them,
//! also when they were changed in YouTube Studio. The section keeps the creator's
//! choice, and says so when YouTube has another.

use std::ops::{BitOr, BitOrAssign};
use std::time::Duration;

use gpui_kit::{AppContext as _, Context};
use rushes_core::{Video, VideoId};
use rushes_youtube::{Details, DetailsChange, Error as YoutubeError, VideoInfo};

use super::rules;
use super::service::{YoutubeService, describe_error};

/// How long details wait after their last change before they go to YouTube.
pub const DETAILS_SEND_DELAY: Duration = Duration::from_secs(3);

/// Some of a video's details.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DetailFields {
    pub title: bool,
    pub description: bool,
    pub tags: bool,
    pub category: bool,
}

impl DetailFields {
    pub const ALL: Self = Self { title: true, description: true, tags: true, category: true };

    pub fn is_empty(self) -> bool {
        self == Self::default()
    }

    /// The details `a` and `b` differ in, as viewers see them: YouTube may drop
    /// the spaces and line breaks a description ends with.
    pub fn between(a: &Details, b: &Details) -> Self {
        let description = |text: &str| text.replace("\r\n", "\n").trim_end().to_owned();
        Self {
            title: a.title.trim() != b.title.trim(),
            description: description(&a.description) != description(&b.description),
            tags: a.tags != b.tags,
            category: a.category_id != b.category_id,
        }
    }

    /// These fields of `details`, as a change of them.
    fn change(self, details: Details) -> DetailsChange {
        DetailsChange {
            title: self.title.then_some(details.title),
            description: self.description.then_some(details.description),
            tags: self.tags.then_some(details.tags),
            category_id: self.category.then_some(details.category_id),
        }
    }

    /// "the title and tags are", for a sentence.
    pub fn sentence_subject(self) -> String {
        let names: Vec<&str> = [
            (self.title, "title"),
            (self.description, "description"),
            (self.tags, "tags"),
            (self.category, "category"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
        let verb = if names.len() > 1 || names == ["tags"] { "are" } else { "is" };
        let names = match names.as_slice() {
            [] => String::new(),
            [one] => (*one).to_owned(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        };
        format!("the {names} {verb}")
    }
}

impl BitOr for DetailFields {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self {
            title: self.title || other.title,
            description: self.description || other.description,
            tags: self.tags || other.tags,
            category: self.category || other.category,
        }
    }
}

impl BitOrAssign for DetailFields {
    fn bitor_assign(&mut self, other: Self) {
        *self = *self | other;
    }
}

/// Changing the details of a video on YouTube, for this run of the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DetailsSync {
    /// The change is on its way.
    Sending,
    /// The change didn't reach YouTube; `problem` says why, as the creator should read it.
    Failed {
        problem: String,
        /// The account to sign in to again, when that is what it takes, by key.
        reconnect: Option<String>,
    },
}

impl YoutubeService {
    /// Where changing the details of the YouTube video `youtube_id` stands, while
    /// the change is on its way or after YouTube didn't make it.
    pub fn details_sync(&self, youtube_id: &str) -> Option<&DetailsSync> {
        self.details.get(youtube_id)
    }

    /// The details YouTube, as it last said, has otherwise than the publish
    /// settings of `video` choose, and what YouTube has: a change that didn't get
    /// there (also in an earlier run of the app), or one made in YouTube Studio.
    /// `None` while a change waits to go or is on its way.
    pub fn other_details_on_youtube(&self, video: &Video) -> Option<(DetailFields, Details)> {
        let youtube_id = &video.youtube.as_ref()?.video_id;
        if self.details_waiting.get(&video.id).is_some_and(|fields| !fields.is_empty())
            || self.details.contains_key(youtube_id)
        {
            return None;
        }
        let on_youtube = self.remote.get(youtube_id)?.details();
        let fields = DetailFields::between(&rules::details(video), &on_youtube);
        (!fields.is_empty()).then_some((fields, on_youtube))
    }

    /// The publish settings of `video_id` changed its `fields`: they go to its
    /// video on YouTube [`DETAILS_SEND_DELAY`] after the last change, or to the
    /// video an upload under way puts there, once it's there.
    pub fn change_youtube_details(&mut self, video_id: VideoId, fields: DetailFields, cx: &mut Context<Self>) {
        let uploaded = self.state.read(cx).video(video_id).is_some_and(|video| video.youtube.is_some());
        let uploading = self.is_uploading_any(&[video_id]);
        // Until then, the first upload takes them.
        if fields.is_empty() || !(uploaded || uploading) {
            return;
        }
        *self.details_waiting.entry(video_id).or_default() |= fields;
        if uploading {
            // The upload sent the details it started with.
            return;
        }
        self.wait_to_send_details(video_id, cx);
        cx.notify();
    }

    /// The form is changing the details of `video_id` again, before it saves
    /// them: what waits to go waits [`DETAILS_SEND_DELAY`] more, so a draft being
    /// typed doesn't go half-written.
    pub fn postpone_youtube_details(&mut self, video_id: VideoId, cx: &mut Context<Self>) {
        if self.details_timers.contains_key(&video_id) {
            self.wait_to_send_details(video_id, cx);
        }
    }

    /// Sends the details of `video_id` waiting to go [`DETAILS_SEND_DELAY`] from
    /// now, instead of when an earlier wait would have.
    fn wait_to_send_details(&mut self, video_id: VideoId, cx: &mut Context<Self>) {
        let wait = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DETAILS_SEND_DELAY).await;
            this.update(cx, |this, cx| {
                this.details_timers.remove(&video_id);
                this.send_waiting_details(video_id, cx);
            })
            .ok();
        });
        // Replacing the wait cancels the one before.
        self.details_timers.insert(video_id, wait);
    }

    /// Sends the details of `video_id` to YouTube now: its `fields`, with those
    /// waiting to go.
    pub fn send_youtube_details(&mut self, video_id: VideoId, fields: DetailFields, cx: &mut Context<Self>) {
        if !fields.is_empty() {
            *self.details_waiting.entry(video_id).or_default() |= fields;
        }
        self.details_timers.remove(&video_id);
        self.send_waiting_details(video_id, cx);
    }

    /// Takes the details YouTube has into the publish settings of `video_id`,
    /// where they differ. Returns which it took.
    pub fn use_youtube_details(&mut self, video_id: VideoId, cx: &mut Context<Self>) -> DetailFields {
        let Some(mut video) = self.state.read(cx).video(video_id).cloned() else { return DetailFields::default() };
        let Some((fields, on_youtube)) = self.other_details_on_youtube(&video) else { return DetailFields::default() };
        let publish = &mut video.publish;
        if fields.title {
            publish.title = on_youtube.title;
        }
        if fields.description {
            publish.description = on_youtube.description;
        }
        if fields.tags {
            publish.tags = on_youtube.tags;
        }
        if fields.category {
            publish.category_id = on_youtube.category_id;
        }
        self.state.update(cx, |state, cx| state.update_video(video, cx));
        cx.notify();
        fields
    }

    /// Sends the details of `video_id` waiting to go, unless its upload under way
    /// or a change on its way goes first.
    fn send_waiting_details(&mut self, video_id: VideoId, cx: &mut Context<Self>) {
        if self.is_uploading_any(&[video_id]) {
            return;
        }
        let Some(video) = self.state.read(cx).video(video_id) else {
            self.details_waiting.remove(&video_id);
            return;
        };
        let Some(link) = video.youtube.clone() else {
            self.details_waiting.remove(&video_id);
            return;
        };
        let youtube_id = link.video_id;
        if self.details.get(&youtube_id) == Some(&DetailsSync::Sending) {
            return;
        }
        let Some(fields) = self.details_waiting.get(&video_id).copied().filter(|fields| !fields.is_empty()) else {
            return;
        };
        let change = fields.change(rules::details(video));
        let channel = link.channel_id;
        let client = self.client_for(&channel);
        if let Some((problem, reconnect)) = self.change_refused(client.as_deref(), &channel) {
            self.details.insert(youtube_id, DetailsSync::Failed { problem, reconnect: reconnect.then_some(channel) });
            cx.notify();
            return;
        }
        let Some(client) = client else { return };
        self.details_waiting.remove(&video_id);
        self.details.insert(youtube_id.clone(), DetailsSync::Sending);
        self.details_changed(&youtube_id);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let id = youtube_id.clone();
            let result = cx.background_spawn(async move { client.set_details(&id, &change) }).await;
            this.update(cx, |this, cx| this.on_details_sent(video_id, youtube_id, channel, fields, result, cx)).ok();
        })
        .detach();
    }

    fn on_details_sent(
        &mut self,
        video_id: VideoId,
        youtube_id: String,
        channel: String,
        sent: DetailFields,
        result: Result<Option<Details>, YoutubeError>,
        cx: &mut Context<Self>,
    ) {
        self.details.remove(&youtube_id);
        self.details_changed(&youtube_id);
        // The video may have gone, or uploaded again to another YouTube video.
        let linked = self
            .state
            .read(cx)
            .video(video_id)
            .and_then(|video| video.youtube.as_ref())
            .is_some_and(|link| link.video_id == youtube_id);
        // Changed while this one was on its way, and done waiting.
        let changed_since =
            self.details_waiting.contains_key(&video_id) && !self.details_timers.contains_key(&video_id);
        match result {
            Ok(Some(now)) => {
                if let Some(info) = self.remote.get_mut(&youtube_id) {
                    info.set_details(now.clone());
                }
                // For answers asked before this one, also before YouTube was first asked.
                self.details_answered.insert(youtube_id, now);
            }
            Ok(None) => {
                if linked {
                    self.details_waiting.remove(&video_id);
                }
                self.on_video_gone(&youtube_id, &channel, cx);
            }
            Err(error) => {
                // What didn't get there goes with the next try.
                if linked {
                    *self.details_waiting.entry(video_id).or_default() |= sent;
                }
                let problem = describe_error(&error);
                let reconnect = error.needs_sign_in().then(|| channel.clone());
                if reconnect.is_some() {
                    self.require_sign_in(&channel, cx);
                }
                self.details.insert(youtube_id, DetailsSync::Failed { problem, reconnect });
            }
        }
        if changed_since {
            self.send_waiting_details(video_id, cx);
        }
        cx.notify();
    }

    /// Sends the details of `video_id` to the video its upload just put on
    /// YouTube with `uploaded`, when they changed while the video uploaded (also
    /// before a resume, which keeps the details the upload started with).
    pub(super) fn send_details_after_upload(&mut self, video_id: VideoId, uploaded: &Details, cx: &mut Context<Self>) {
        let Some(video) = self.state.read(cx).video(video_id) else { return };
        let fields = DetailFields::between(&rules::details(video), uploaded);
        if !fields.is_empty() || self.details_waiting.contains_key(&video_id) {
            self.send_youtube_details(video_id, fields, cx);
        }
    }

    /// Sends again the changes that waited for the account `key` to sign in again.
    pub(super) fn send_details_after_sign_in(&mut self, key: &str, cx: &mut Context<Self>) {
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
                            self.details.get(&link.video_id),
                            Some(DetailsSync::Failed { reconnect: Some(_), .. })
                        )
                })
            })
            .map(|video| video.id)
            .collect();
        for video_id in waiting {
            self.send_waiting_details(video_id, cx);
        }
    }

    /// Forgets the details waiting to go for `video_id`: an upload starting takes
    /// them as they are.
    pub(super) fn details_taken_by_upload(&mut self, video_id: VideoId) {
        self.details_waiting.remove(&video_id);
        self.details_timers.remove(&video_id);
    }

    /// Counts a change sent to `youtube_id`: what YouTube said about it before
    /// the change is out of date.
    fn details_changed(&mut self, youtube_id: &str) {
        *self.details_changes.entry(youtube_id.to_owned()).or_default() += 1;
    }

    /// How many details changes went to `youtube_id`, to tell answers asked before one.
    pub(super) fn details_changes(&self, youtube_id: &str) -> u64 {
        self.details_changes.get(youtube_id).copied().unwrap_or_default()
    }

    /// Keeps the details a change left in `info`, which YouTube said after
    /// `changes` details changes: asked before the last change, or while one is on
    /// its way, it may predate it.
    pub(super) fn keep_sent_details(&self, info: &mut VideoInfo, changes: u64) {
        let current =
            self.details_changes(&info.id) == changes && self.details.get(&info.id) != Some(&DetailsSync::Sending);
        if current {
            return;
        }
        let known = self.remote.get(&info.id).map(VideoInfo::details);
        if let Some(known) = known.or_else(|| self.details_answered.get(&info.id).cloned()) {
            info.set_details(known);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn details(title: &str, description: &str, tags: &[&str], category_id: &str) -> Details {
        Details {
            title: title.into(),
            description: description.into(),
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            category_id: category_id.into(),
        }
    }

    #[test]
    fn details_differ_as_viewers_see_them() {
        let chosen = details("Rust Ownership", "Links below\n\n", &["rust"], "28");
        assert!(
            DetailFields::between(&chosen, &details(" Rust Ownership ", "Links below", &["rust"], "28")).is_empty()
        );
        assert!(
            DetailFields::between(&chosen, &details("Rust Ownership", "Links below\r\n", &["rust"], "28")).is_empty()
        );
        assert_eq!(
            DetailFields::between(&chosen, &details("Rust", "Links below", &["rust", "borrowing"], "28")),
            DetailFields { title: true, tags: true, ..DetailFields::default() }
        );
        assert_eq!(DetailFields::between(&chosen, &details("", "", &[], "22")), DetailFields::ALL);
    }

    #[test]
    fn only_the_changed_fields_go() {
        let chosen = details("Rust Ownership", "Links below", &["rust"], "28");
        let change = DetailFields { description: true, tags: true, ..DetailFields::default() }.change(chosen);
        assert_eq!(
            change,
            DetailsChange {
                title: None,
                description: Some("Links below".into()),
                tags: Some(vec!["rust".into()]),
                category_id: None,
            }
        );
    }

    #[test]
    fn fields_read_as_a_sentence() {
        let only = |fields: DetailFields| fields.sentence_subject();
        assert_eq!(only(DetailFields { description: true, ..DetailFields::default() }), "the description is");
        assert_eq!(only(DetailFields { tags: true, ..DetailFields::default() }), "the tags are");
        assert_eq!(only(DetailFields { title: true, tags: true, ..DetailFields::default() }), "the title and tags are");
        assert_eq!(only(DetailFields::ALL), "the title, description, tags and category are");
    }
}
