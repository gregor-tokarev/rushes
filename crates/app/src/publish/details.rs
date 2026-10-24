//! What viewers read about videos on YouTube: the title, description, tags and
//! category the Publish tab's Details section chooses go to the video once it is
//! there when the creator saves them. Only the details that changed go; the others
//! stay as YouTube has them, also when they were changed in YouTube Studio. The
//! section keeps the creator's choice, and says so when YouTube has another.

use std::ops::{BitOr, BitOrAssign};

use gpui_kit::{AppContext as _, Context};
use rushes_core::{Video, VideoId};
use rushes_youtube::{Details, DetailsChange, Error as YoutubeError, VideoInfo};

use super::rules;
use super::service::{YoutubeService, describe_error};

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

    /// These fields but `other`'s.
    fn without(self, other: Self) -> Self {
        Self {
            title: self.title && !other.title,
            description: self.description && !other.description,
            tags: self.tags && !other.tags,
            category: self.category && !other.category,
        }
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
    /// settings of `video` choose, and what YouTube has: changes the form hasn't
    /// saved to YouTube yet (also in an earlier run of the app). `None` until
    /// YouTube said what it has.
    pub fn other_details_on_youtube(&self, video: &Video) -> Option<(DetailFields, Details)> {
        let on_youtube = self.details_on_youtube(video)?;
        let fields = DetailFields::between(&rules::details(video), &on_youtube);
        (!fields.is_empty()).then_some((fields, on_youtube))
    }

    /// The details of `video` its video on YouTube doesn't have: what saving sends.
    /// Until YouTube said what it has, the details changed since they last went.
    pub fn unsaved_details(&self, video: &Video) -> DetailFields {
        if video.youtube.is_none() {
            return DetailFields::default();
        }
        match self.details_on_youtube(video) {
            Some(on_youtube) => DetailFields::between(&rules::details(video), &on_youtube),
            None => self.details_waiting.get(&video.id).copied().unwrap_or_default(),
        }
    }

    /// The details of the YouTube video `video` links to, as YouTube last said:
    /// as the video recorded them, or as YouTube said this run.
    fn details_on_youtube(&self, video: &Video) -> Option<Details> {
        let link = video.youtube.as_ref()?;
        link.details.as_ref().map(rules::recorded_details).or_else(|| {
            let youtube_id = &link.video_id;
            self.remote
                .get(youtube_id)
                .map(VideoInfo::details)
                .or_else(|| self.details_answered.get(youtube_id).cloned())
        })
    }

    /// The publish settings of `video_id` changed its `fields`: once it is on
    /// YouTube, they wait there for the creator to save them.
    pub fn change_youtube_details(&mut self, video_id: VideoId, fields: DetailFields, cx: &mut Context<Self>) {
        let uploaded = self.state.read(cx).video(video_id).is_some_and(|video| video.youtube.is_some());
        let uploading = self.is_uploading_any(&[video_id]);
        // Until then, the first upload takes them.
        if fields.is_empty() || !(uploaded || uploading) {
            return;
        }
        *self.details_waiting.entry(video_id).or_default() |= fields;
        cx.notify();
    }

    /// Sends the details of `video_id` to YouTube now: its `fields`, with those
    /// waiting to go.
    pub fn send_youtube_details(&mut self, video_id: VideoId, fields: DetailFields, cx: &mut Context<Self>) {
        if !fields.is_empty() {
            *self.details_waiting.entry(video_id).or_default() |= fields;
        }
        self.send_waiting_details(video_id, cx);
    }

    /// Takes the details YouTube has into the publish settings of `video_id`,
    /// where they differ. Returns which it took.
    pub fn use_youtube_details(&mut self, video_id: VideoId, cx: &mut Context<Self>) -> DetailFields {
        let Some(mut video) = self.state.read(cx).video(video_id).cloned() else { return DetailFields::default() };
        let Some((fields, on_youtube)) = self.other_details_on_youtube(&video) else { return DetailFields::default() };
        take_details(&mut video, fields, on_youtube);
        self.state.update(cx, |state, cx| state.update_video(video, cx));
        // YouTube has them: nothing of them is left to save.
        if let Some(waiting) = self.details_waiting.get_mut(&video_id) {
            *waiting = waiting.without(fields);
        }
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
            self.details_again.insert(video_id);
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
        // Saved again while this one was on its way.
        let again = self.details_again.remove(&video_id);
        match result {
            Ok(Some(now)) => {
                if let Some(info) = self.remote.get_mut(&youtube_id) {
                    info.set_details(now.clone());
                }
                // What it changed there was the form's; edits made since stay.
                if linked
                    && let Some(mut video) = self.state.read(cx).video(video_id).cloned()
                    && record_details(&mut video, &now, sent)
                {
                    self.state.update(cx, |state, cx| state.update_video(video, cx));
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
        if again && linked {
            self.send_waiting_details(video_id, cx);
        }
        cx.notify();
    }

    /// Sends again the saved changes that waited for the account `key` to sign in again.
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
        self.details_again.remove(&video_id);
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

/// Takes `fields` of `on_youtube` into the publish settings of `video`.
fn take_details(video: &mut Video, fields: DetailFields, on_youtube: Details) {
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
}

/// Records that YouTube has `on_youtube` for `video`, after a save of the form's
/// `saved` fields (none for a lookup). What else changed there since the video
/// last recorded it (in YouTube Studio) comes into its publish settings, unless
/// the form changed it too and hasn't saved it to YouTube yet. Returns whether
/// `video` changed.
pub(super) fn record_details(video: &mut Video, on_youtube: &Details, saved: DetailFields) -> bool {
    let Some(link) = &video.youtube else { return false };
    let recorded = link.details.as_ref().map(rules::recorded_details);
    if recorded.as_ref() == Some(on_youtube) {
        return false;
    }
    if let Some(recorded) = recorded {
        let changed_there = DetailFields::between(&recorded, on_youtube).without(saved);
        let changed_here = DetailFields::between(&rules::details(video), &recorded);
        take_details(video, changed_there.without(changed_here), on_youtube.clone());
    }
    if let Some(link) = video.youtube.as_mut() {
        link.details = Some(rules::youtube_details(on_youtube.clone()));
    }
    true
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
