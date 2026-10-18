//! The Publish tab: YouTube metadata on the left, account, file, thumbnail and the
//! upload itself on the right.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDateTime, NaiveTime, TimeDelta, TimeZone as _, Utc};
use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::link::Link;
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IndexPath, Side, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Div, Entity, ExternalPaths, FontWeight, InteractiveElement as _,
    IntoElement, ObjectFit, ParentElement as _, PathPromptOptions, Pixels, Rems, Render, SharedString, Styled as _,
    StyledImage as _, Subscription, Task, Window, canvas, div, img, rems,
};
use rushes_core::document::DEFAULT_WPM;
use rushes_core::media::{self, MediaKind};
use rushes_core::settings::WeekStart;
use rushes_core::{
    DocumentKind, Privacy, PublishSettings, UploadJob, UploadStatus, Video, VideoFormat, VideoId, VideoStatus,
};
use rushes_pipeline::{MediaInfo, has_transcript, kept_cuts, list_thumbnails};
use rushes_youtube::Visibility;

use super::assist_dialogs::open_details_dialog;
use super::assistant::{AssistEvent, DetailSources, DetailsJob, PublishAssistant, ThumbnailJob};
use super::channel::{ChannelChoices, ChooseChannel};
use super::chapters::{MIN_CHAPTERS, chapters, chapters_text, insert_chapters, timestamp};
use super::rules::{
    self, AccountAccess, BlockingReason, CATEGORIES, Category, DESCRIPTION_MAX_BYTES, TAGS_MAX_CHARS,
    THUMBNAIL_MAX_BYTES, TITLE_MAX_CHARS, tags_length,
};
use super::service::{Connection, YoutubeAccount, YoutubeEvent, YoutubeService};
use super::thumbnail::ThumbnailSync;
use super::thumbnail_chat::open_thumbnail_chat;
use super::transfer::{format_bytes, format_duration, format_eta, format_speed};
use super::ui::{
    ConnectionPanel, FieldRow, card, file_name, help_text, section_heading, sign_in_progress, status_line,
    thumbnail_tile, youtube_link,
};
use super::upload::{ProcessingOutcome, Retired, UPLOAD_ACCOUNT_LOCKED, UPLOAD_FILE_LOCKED, UploadError};
use super::visibility::VisibilitySync;
use crate::ai_edit::PipelineService;
use crate::route::{Route, VideoTab};
use crate::state::{AppEvent, AppState};
use crate::ui::date_field::{DateField, DateFieldEvent, DateFieldState};
use crate::ui::path_prompt::{self, PathKind, PathRequest};
use crate::ui::{menu_item, notification, scrolling_menu, state_tooltip, warning_text};

/// Form edits are saved this long after the last keystroke.
const SAVE_DELAY: Duration = Duration::from_millis(500);
const PRIVACY_HELP: [&str; 3] =
    ["Only you can watch it.", "Anyone with the link can watch it.", "Everyone can find and watch it."];
/// What YouTube takes as a custom thumbnail.
const THUMBNAIL_EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];
const NOT_A_THUMBNAIL: &str = "Choose a JPG or PNG image.";
/// Wide enough for the cards' buttons; the form takes the rest of the page column.
const SIDE_COLUMN_WIDTH: Rems = Rems(17.);
/// The narrowest the form gets beside the side column.
const FORM_MIN_WIDTH: Rems = Rems(20.);
/// Form, gap and side column; a narrower panel stacks them.
const TWO_COLUMN_MIN_WIDTH: Rems = Rems(FORM_MIN_WIDTH.0 + 2. + SIDE_COLUMN_WIDTH.0);

#[derive(Clone)]
struct CategoryItem(Category);

impl SelectItem for CategoryItem {
    type Value = &'static str;

    fn title(&self) -> SharedString {
        self.0.label.into()
    }

    fn value(&self) -> &Self::Value {
        &self.0.id
    }
}

/// Why there are no chapters to insert.
const NO_SECTIONS: &str = "Add Heading 2 sections to the script to make chapters.";
const TOO_FEW_SECTIONS: &str =
    "YouTube shows chapters only for three or more sections of 10 seconds or longer. The script has fewer.";
const SHORTS_HAVE_NO_CHAPTERS: &str = "Shorts don’t have chapters on YouTube.";

/// Chapters YouTube will show, as description text.
#[derive(Clone, Debug, PartialEq)]
struct ChapterList {
    text: String,
    /// Start of the last chapter, which the video must outlast by 10 s.
    last_start: u32,
}

/// Which file a chooser, a typed path or a drop is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileTarget {
    Video,
    Thumbnail,
}

impl FileTarget {
    fn placeholder(self) -> &'static str {
        match self {
            FileTarget::Video => "~/Videos/Rushes/final.mp4",
            FileTarget::Thumbnail => "~/Pictures/thumbnail.png",
        }
    }
}

/// Where the chosen thumbnail stands on YouTube, for a video that is there.
#[derive(Clone, Debug, PartialEq)]
enum YoutubeThumbnail {
    Sending,
    Set,
    /// Chosen while YouTube couldn't be asked, or before Rushes set thumbnails of
    /// uploaded videos.
    NotSet,
    /// YouTube didn't take it; why, as the creator should read it.
    Failed(SharedString),
    /// No image is chosen; YouTube keeps the one it has.
    Kept,
}

/// A video file the creator is likely to upload.
#[derive(Clone, Debug, PartialEq)]
struct FileChoice {
    path: PathBuf,
    source: &'static str,
}

/// Size and media details of the chosen file, read in the background.
#[derive(Clone, Debug, PartialEq)]
struct FileDetails {
    path: PathBuf,
    /// `None` when the file is missing.
    size: Option<u64>,
    media: Option<MediaInfo>,
}

/// A part of the publish settings the form edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Title,
    Description,
    Tags,
    Category,
    /// Privacy and schedule, which change together.
    Visibility,
    Audience,
    File,
    Thumbnail,
    /// The account the video uploads to.
    Channel,
}

/// The fields the form changed since its last save.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Edited(u16);

impl Edited {
    fn insert(&mut self, field: Field) {
        self.0 |= 1 << field as u8;
    }

    fn contains(self, field: Field) -> bool {
        self.0 & (1 << field as u8) != 0
    }

    fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// `stored` with the edited fields taken from `draft`. The others may have changed
    /// elsewhere meanwhile, such as the file AI edit picks, and keep that change.
    fn merge(self, stored: &PublishSettings, draft: &PublishSettings) -> PublishSettings {
        let mut merged = stored.clone();
        if self.contains(Field::Title) {
            merged.title.clone_from(&draft.title);
        }
        if self.contains(Field::Description) {
            merged.description.clone_from(&draft.description);
        }
        if self.contains(Field::Tags) {
            merged.tags.clone_from(&draft.tags);
        }
        if self.contains(Field::Category) {
            merged.category_id.clone_from(&draft.category_id);
        }
        if self.contains(Field::Visibility) {
            merged.privacy = draft.privacy;
            merged.publish_at = draft.publish_at;
        }
        if self.contains(Field::Audience) {
            merged.made_for_kids = draft.made_for_kids;
        }
        if self.contains(Field::File) {
            merged.file.clone_from(&draft.file);
        }
        if self.contains(Field::Thumbnail) {
            merged.thumbnail.clone_from(&draft.thumbnail);
        }
        if self.contains(Field::Channel) {
            merged.channel.clone_from(&draft.channel);
        }
        merged
    }
}

/// The Publish tab of a video page.
pub struct PublishPanel {
    video_id: VideoId,
    state: Entity<AppState>,
    service: Entity<YoutubeService>,
    /// The form's copy of `video.publish`; its edited fields are saved after [`SAVE_DELAY`].
    draft: PublishSettings,
    edited: Edited,
    save_task: Option<Task<()>>,
    title: Entity<InputState>,
    /// The video title, shown while the YouTube title is empty.
    title_placeholder: String,
    description: Entity<TextareaState>,
    tag_input: Entity<InputState>,
    category: Entity<SelectState<Vec<CategoryItem>>>,
    schedule: Entity<DateFieldState>,
    /// The day the schedule's weeks start on.
    week_start: WeekStart,
    _schedule_changes: Subscription,
    choices: Vec<FileChoice>,
    file: Option<FileDetails>,
    /// Size of the chosen thumbnail, `None` while unknown or missing.
    thumbnail_size: Option<(PathBuf, Option<u64>)>,
    /// Chapter list from the script, or why YouTube wouldn't show one.
    chapters: Result<ChapterList, &'static str>,
    /// Who could watch before "Schedule" forced private, restored when it is turned off.
    unscheduled_privacy: Option<Privacy>,
    /// The panel's width in the last frame; below [`TWO_COLUMN_MIN_WIDTH`] the
    /// side cards go under the form.
    width: Rc<Cell<Option<Pixels>>>,
    /// Writes the details with the agent and draws thumbnails with Codex, when the
    /// app has one.
    assistant: Option<Entity<PublishAssistant>>,
    /// What the agent can write the details from.
    sources: DetailSources,
    /// The thumbnails drawn for the video, newest first.
    drawn: Vec<PathBuf>,
    _subscriptions: Vec<Subscription>,
}

impl PublishPanel {
    pub fn new(video_id: VideoId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::global(cx);
        let service = YoutubeService::global(cx);
        let video = state.read(cx).video(video_id).cloned().unwrap_or_else(|| Video::new(""));
        let draft = video.publish.clone();

        let title = cx
            .new(|cx| InputState::new(window, cx).placeholder(video.title.clone()).default_value(draft.title.clone()));
        let description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(6, 20)
                .placeholder("Tell viewers what the video is about")
                .default_value(draft.description.clone())
        });
        let tag_input = cx.new(|cx| InputState::new(window, cx).placeholder("Add a tag…"));
        let items: Vec<CategoryItem> = CATEGORIES.into_iter().map(CategoryItem).collect();
        let category_ix = category_index(&draft.category_id);
        let category = cx.new(|cx| SelectState::new(items, Some(IndexPath::new(category_ix)), window, cx));
        let week_start = state.read(cx).settings().week_start;
        let schedule = schedule_picker(
            local_time(draft.publish_at.unwrap_or_else(|| default_schedule(&video))),
            week_start,
            window,
            cx,
        );
        let schedule_changes = cx.subscribe(&schedule, Self::on_schedule_change);

        let subscriptions = vec![
            cx.subscribe_in(&state, window, Self::on_app_event),
            cx.observe(&service, |_, _, cx| cx.notify()),
            cx.subscribe(&service, |this, _, event: &YoutubeEvent, cx| {
                if *event == YoutubeEvent::ConnectionChanged {
                    this.refresh_youtube_video(cx);
                }
            }),
            cx.subscribe(&title, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.draft.title = input.read(cx).value().to_string();
                    this.schedule_save(Field::Title, cx);
                }
            }),
            cx.subscribe(&description, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.draft.description = input.read(cx).value().to_string();
                    this.schedule_save(Field::Description, cx);
                }
            }),
            cx.subscribe_in(&tag_input, window, Self::on_tag_input),
            cx.subscribe(&category, |this, _, event: &SelectEvent<Vec<CategoryItem>>, cx| {
                let SelectEvent::Confirm(id) = event;
                this.draft.category_id = id.map(str::to_owned).unwrap_or_default();
                this.schedule_save(Field::Category, cx);
            }),
            // Coming back from the file manager or another app may bring new files,
            // or a re-exported one that a stopped upload can no longer continue;
            // coming back from YouTube Studio, a video changed or deleted there.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.refresh_choices(cx);
                    this.refresh_file(cx);
                    this.refresh_drawn(cx);
                    this.service.update(cx, |service, cx| service.check_stopped_uploads(cx));
                    if this.is_shown(cx) {
                        this.refresh_youtube_video(cx);
                    }
                }
            }),
            // Leaving the page or quitting must not lose the last edits.
            cx.on_release(|this, cx| this.save_pending(cx)),
            cx.on_app_quit(|this, cx| {
                this.save_pending(cx);
                async {}
            }),
        ];

        let assistant = PublishAssistant::try_global(cx);
        let mut subscriptions = subscriptions;
        if let Some(assistant) = &assistant {
            subscriptions.push(cx.observe(assistant, |_, _, cx| cx.notify()));
            subscriptions.push(cx.subscribe(assistant, |this, _, event: &AssistEvent, cx| {
                let AssistEvent::ThumbnailsDrawn(video_id) = *event;
                if video_id == this.video_id {
                    this.refresh_drawn(cx);
                }
            }));
        }

        let mut panel = Self {
            video_id,
            state,
            service,
            draft,
            edited: Edited::default(),
            save_task: None,
            title,
            title_placeholder: video.title.clone(),
            description,
            tag_input,
            category,
            schedule,
            week_start,
            _schedule_changes: schedule_changes,
            choices: Vec::new(),
            file: None,
            thumbnail_size: None,
            chapters: Err(NO_SECTIONS),
            unscheduled_privacy: None,
            width: Rc::default(),
            assistant,
            sources: DetailSources { script: false, transcript: false },
            drawn: Vec::new(),
            _subscriptions: subscriptions,
        };
        panel.refresh_chapters(cx);
        panel.refresh_choices(cx);
        panel.refresh_file(cx);
        panel.refresh_thumbnail(cx);
        panel.refresh_drawn(cx);
        panel.refresh_youtube_video(cx);
        panel.service.update(cx, |service, cx| service.check_stopped_uploads(cx));
        panel
    }

    /// Whether the page shows this panel now: its video's Publish tab.
    fn is_shown(&self, cx: &App) -> bool {
        *self.state.read(cx).route() == Route::Video { id: self.video_id, tab: VideoTab::Publish }
    }

    /// Asks YouTube how the uploaded video stands, for the Upload card.
    fn refresh_youtube_video(&mut self, cx: &mut Context<Self>) {
        let Some(link) = self.state.read(cx).video(self.video_id).and_then(|video| video.youtube.clone()) else {
            return;
        };
        self.service.update(cx, |service, cx| service.refresh_youtube_video(&link, cx));
    }

    pub fn video_id(&self) -> VideoId {
        self.video_id
    }

    /// What stops the upload, as the form currently reads.
    pub fn blocking_reasons(&self, cx: &App) -> Vec<BlockingReason> {
        let Some(video) = self.draft_video(cx) else { return Vec::new() };
        // Until the chosen file has been looked at it counts as present; starting the
        // upload checks it again.
        let file_exists = match (&self.draft.file, &self.file) {
            (Some(path), Some(details)) if &details.path == path => details.size.is_some(),
            _ => true,
        };
        let access = self.service.read(cx).account_access_for(self.channel_key());
        rules::blocking_reasons(&video, access, file_exists, Utc::now())
    }

    /// Key of the account the video chose, `None` while it uploads to the default one.
    fn channel_key(&self) -> Option<&str> {
        self.draft.channel.as_ref().map(|channel| channel.id.as_str())
    }

    /// Lets the video upload to the account `key`.
    fn set_channel(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(channel) = self.service.read(cx).find_account(key).map(YoutubeAccount::publish_channel) else {
            return;
        };
        if self.draft.channel.as_ref() != Some(&channel) {
            self.draft.channel = Some(channel);
            self.schedule_save(Field::Channel, cx);
        }
    }

    /// Which account a running or stopped upload goes to, when that needs saying:
    /// with several accounts, or when it isn't the account the video uploads to now.
    fn upload_destination(&self, job: &UploadJob, cx: &App) -> Option<String> {
        let service = self.service.read(cx);
        let key = job.channel.as_deref()?;
        let current = self.draft_video(cx).and_then(|video| service.channel_for(&video));
        if service.accounts().len() < 2 && current.as_deref() == Some(key) {
            return None;
        }
        service
            .find_account(key)
            .map(YoutubeAccount::name)
            .or_else(|| self.draft.channel.as_ref().filter(|chosen| chosen.id == key).map(|chosen| chosen.name.clone()))
    }

    /// Signs another account in for this video: once signed in, the video uploads to it.
    fn add_account(&mut self, cx: &mut Context<Self>) {
        // A channel picked a moment ago must not overwrite the one the sign-in picks.
        self.save_pending(cx);
        let video_id = self.video_id;
        self.service.update(cx, |service, cx| service.connect_for(video_id, cx));
    }

    /// Saves pending form edits now instead of after the delay. Only the fields the form
    /// changed are written, so a file chosen elsewhere meanwhile stays.
    pub fn save_pending(&mut self, cx: &mut App) {
        self.save_edits(cx);
    }

    /// [`Self::save_pending`]; returns whether a new visibility went to YouTube with it.
    fn save_edits(&mut self, cx: &mut App) -> bool {
        if self.edited.is_empty() {
            return false;
        }
        let edited = std::mem::take(&mut self.edited);
        let Some(mut video) = self.state.read(cx).video(self.video_id).cloned() else { return false };
        let publish = edited.merge(&video.publish, &self.draft);
        if video.publish == publish {
            return false;
        }
        let visibility_changed =
            (video.publish.privacy, video.publish.publish_at) != (publish.privacy, publish.publish_at);
        video.publish = publish;
        self.state.update(cx, |state, cx| state.update_video(video, cx));
        // Who can watch the video changes on YouTube too, or on the video an upload
        // under way puts there.
        if visibility_changed {
            let video_id = self.video_id;
            self.service.update(cx, |service, cx| service.set_youtube_visibility(video_id, cx));
        }
        visibility_changed
    }

    /// Sends who can watch the video to YouTube, after it didn't get there or YouTube
    /// has another.
    fn send_visibility(&mut self, cx: &mut Context<Self>) {
        if !self.save_edits(cx) {
            let video_id = self.video_id;
            self.service.update(cx, |service, cx| service.set_youtube_visibility(video_id, cx));
        }
    }

    fn schedule_save(&mut self, field: Field, cx: &mut Context<Self>) {
        // Once the creator changes what the agent wrote, Undo would throw that away.
        if matches!(field, Field::Title | Field::Description | Field::Tags)
            && let Some(assistant) = &self.assistant
            && matches!(assistant.read(cx).details(self.video_id), Some(DetailsJob::Written(_)))
        {
            let video_id = self.video_id;
            assistant.update(cx, |assistant, cx| assistant.dismiss_details(video_id, cx));
        }
        self.edited.insert(field);
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            this.update(cx, |this, cx| this.save_pending(cx)).ok();
        }));
        cx.notify();
    }

    /// The stored video with the form's metadata.
    fn draft_video(&self, cx: &App) -> Option<Video> {
        let mut video = self.state.read(cx).video(self.video_id)?.clone();
        video.publish = self.draft.clone();
        Some(video)
    }

    fn on_app_event(&mut self, _: &Entity<AppState>, event: &AppEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The tab shown again, or the video opened again: the video may have
        // changed on YouTube meanwhile.
        if *event == AppEvent::RouteChanged && self.is_shown(cx) {
            self.refresh_youtube_video(cx);
        }
        if *event == AppEvent::SettingsChanged {
            self.sync_week_start(cx);
        }
        if *event != AppEvent::DataChanged {
            return;
        }
        let Some(video) = self.state.read(cx).video(self.video_id).cloned() else { return };
        if self.title_placeholder != video.title {
            self.title_placeholder = video.title.clone();
            self.title.update(cx, |input, cx| input.set_placeholder(video.title.clone(), window, cx));
        }
        // Changes made elsewhere (such as "Use for upload" in AI edit) show up, except in
        // fields with edits of the form's own waiting to be saved.
        let publish = self.edited.merge(&video.publish, &self.draft);
        if publish != self.draft {
            self.adopt(publish, window, cx);
        }
        self.refresh_chapters(cx);
        self.refresh_choices(cx);
    }

    fn on_schedule_change(&mut self, _: Entity<DateFieldState>, event: &DateFieldEvent, cx: &mut Context<Self>) {
        let DateFieldEvent::Change(value) = event;
        if self.draft.publish_at.is_some()
            && let Some(at) = value.and_then(utc_time)
        {
            self.draft.publish_at = Some(at);
            self.schedule_save(Field::Visibility, cx);
        }
    }

    fn sync_week_start(&mut self, cx: &mut Context<Self>) {
        let week_start = self.state.read(cx).settings().week_start;
        if week_start == self.week_start {
            return;
        }
        self.week_start = week_start;
        self.schedule.update(cx, |field, cx| field.set_first_day_of_week(week_start.weekday(), cx));
        cx.notify();
    }

    fn adopt(&mut self, publish: PublishSettings, window: &mut Window, cx: &mut Context<Self>) {
        let file_changed = publish.file != self.draft.file;
        let thumbnail_changed = publish.thumbnail != self.draft.thumbnail;
        self.title.update(cx, |input, cx| {
            if input.value().as_ref() != publish.title {
                input.set_value(publish.title.clone(), window, cx);
            }
        });
        self.description.update(cx, |input, cx| {
            if input.value().as_ref() != publish.description {
                input.set_value(publish.description.clone(), window, cx);
            }
        });
        if publish.category_id != self.draft.category_id {
            let ix = category_index(&publish.category_id);
            self.category.update(cx, |select, cx| select.set_selected_index(Some(IndexPath::new(ix)), window, cx));
        }
        if let Some(at) = publish.publish_at {
            self.schedule.update(cx, |field, cx| field.set_value(Some(local_time(at)), window, cx));
        }
        if (publish.privacy, publish.publish_at) != (self.draft.privacy, self.draft.publish_at) {
            // The privacy to restore belonged to the replaced settings.
            self.unscheduled_privacy = None;
        }
        self.draft = publish;
        if file_changed {
            self.refresh_file(cx);
        }
        if thumbnail_changed {
            self.refresh_thumbnail(cx);
        }
        cx.notify();
    }

    fn refresh_chapters(&mut self, cx: &mut Context<Self>) {
        let documents = self.state.read(cx).documents(self.video_id);
        let script = documents.iter().find(|document| document.kind == DocumentKind::Script);
        let found = script.map(|script| chapters(&script.blocks, DEFAULT_WPM)).unwrap_or_default();
        let has_script = script.is_some_and(|script| script.blocks.iter().any(|block| !block.text.trim().is_empty()));
        if has_script != self.sources.script {
            self.sources.script = has_script;
            cx.notify();
        }
        let short = self.state.read(cx).video(self.video_id).is_some_and(|video| video.format == VideoFormat::Short);
        // YouTube silently drops a list it can't use, so only usable ones are offered.
        let offer = match found.last() {
            _ if short => Err(SHORTS_HAVE_NO_CHAPTERS),
            None => Err(NO_SECTIONS),
            Some(_) if found.len() < MIN_CHAPTERS => Err(TOO_FEW_SECTIONS),
            Some(last) => Ok(ChapterList { text: chapters_text(&found), last_start: last.start_secs }),
        };
        if offer != self.chapters {
            self.chapters = offer;
            cx.notify();
        }
    }

    /// Lists the AI edit outputs (final cut first) and the footage of the video.
    fn refresh_choices(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.state.read(cx).video(self.video_id).and_then(media::footage_dir) else { return };
        // Only what an AI edit rendered, or the file chosen already: an unfinished
        // render an earlier version left in edit/cuts is no cut.
        let mut cuts = PipelineService::try_global(cx)
            .map(|service| service.read(cx).rendered_cuts(self.video_id, cx))
            .unwrap_or_default();
        cuts.extend(self.draft.file.clone());
        cx.spawn(async move |this, cx| {
            let (choices, transcript) =
                cx.background_spawn(async move { (file_choices(&dir, &cuts), has_transcript(&dir)) }).await;
            this.update(cx, |this, cx| {
                if this.choices != choices || this.sources.transcript != transcript {
                    this.choices = choices;
                    this.sources.transcript = transcript;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Lists the thumbnails drawn for the video.
    fn refresh_drawn(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.state.read(cx).video(self.video_id).and_then(media::thumbnails_dir) else { return };
        cx.spawn(async move |this, cx| {
            let drawn = cx.background_spawn(async move { list_thumbnails(&dir) }).await;
            this.update(cx, |this, cx| {
                if this.drawn != drawn {
                    this.drawn = drawn;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn details_job(&self, cx: &App) -> Option<DetailsJob> {
        self.assistant.as_ref()?.read(cx).details(self.video_id).cloned()
    }

    fn thumbnail_job(&self, cx: &App) -> Option<ThumbnailJob> {
        self.assistant.as_ref()?.read(cx).thumbnails(self.video_id).cloned()
    }

    /// Asks what to tell the agent, then has it write the details.
    fn open_write_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // What the creator typed a moment ago is what the agent replaces, and Undo restores.
        self.save_pending(cx);
        open_details_dialog(self.video_id, self.sources, window, cx);
    }

    /// Opens the chat with Codex about the video's thumbnails.
    fn open_thumbnail_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let has_video = self.draft.file.is_some() || self.choices.iter().any(|choice| choice.source == "Footage");
        open_thumbnail_chat(cx.entity().downgrade(), self.video_id, has_video, window, cx);
    }

    /// The image chosen as the video's thumbnail.
    pub(super) fn chosen_thumbnail(&self) -> Option<&Path> {
        self.draft.thumbnail.as_deref()
    }

    fn with_assistant(
        &self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut PublishAssistant, VideoId, &mut Context<PublishAssistant>),
    ) {
        if let Some(assistant) = &self.assistant {
            let video_id = self.video_id;
            assistant.update(cx, |assistant, cx| f(assistant, video_id, cx));
        }
    }

    fn refresh_file(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.draft.file.clone() else {
            self.file = None;
            return;
        };
        // The previous details stay up while the file is read again.
        cx.spawn(async move |this, cx| {
            let details = cx
                .background_spawn(async move {
                    let size = std::fs::metadata(&path).ok().filter(|meta| meta.is_file()).map(|meta| meta.len());
                    let media = size.and_then(|_| rushes_pipeline::probe_media(&path));
                    FileDetails { path, size, media }
                })
                .await;
            this.update(cx, |this, cx| {
                if this.draft.file.as_ref() == Some(&details.path) {
                    this.file = Some(details);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn refresh_thumbnail(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.draft.thumbnail.clone() else {
            self.thumbnail_size = None;
            return;
        };
        cx.spawn(async move |this, cx| {
            let lookup = path.clone();
            let size = cx.background_spawn(async move { std::fs::metadata(&lookup).ok().map(|meta| meta.len()) }).await;
            this.update(cx, |this, cx| {
                if this.draft.thumbnail.as_ref() == Some(&path) {
                    this.thumbnail_size = Some((path, size));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn set_file(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        self.draft.file = path;
        self.file = None;
        self.refresh_file(cx);
        self.schedule_save(Field::File, cx);
    }

    /// Makes `path` the video's thumbnail, or none; the thumbnail chat chooses too.
    pub(super) fn set_thumbnail(&mut self, path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if path.as_deref().is_some_and(|path| !is_thumbnail_image(path)) {
            window.push_notification(notification::warning(NOT_A_THUMBNAIL, cx), cx);
            return;
        }
        let chosen = path.is_some();
        self.draft.thumbnail = path;
        self.thumbnail_size = None;
        self.refresh_thumbnail(cx);
        self.schedule_save(Field::Thumbnail, cx);
        // An upload under way takes the image to YouTube when it finishes.
        if chosen && self.uploaded_before(cx) && !self.is_uploading(cx) {
            self.send_thumbnail(cx);
        }
    }

    /// Sets the chosen image as the thumbnail of the video on YouTube.
    fn send_thumbnail(&mut self, cx: &mut Context<Self>) {
        self.save_pending(cx);
        let video_id = self.video_id;
        self.service.update(cx, |service, cx| service.set_youtube_thumbnail(video_id, cx));
    }

    /// Where the chosen thumbnail stands on YouTube, while the video is there and no
    /// upload is under way: an upload takes the chosen image to its new video.
    fn youtube_thumbnail(&self, cx: &App) -> Option<YoutubeThumbnail> {
        if self.is_uploading(cx) {
            return None;
        }
        let link = self.state.read(cx).video(self.video_id)?.youtube.as_ref()?;
        let chosen = self.draft.thumbnail.as_ref();
        Some(match (self.service.read(cx).thumbnail_sync(&link.video_id), chosen) {
            (Some(ThumbnailSync::Sending(_)), _) => YoutubeThumbnail::Sending,
            (Some(ThumbnailSync::Failed { image, problem }), Some(chosen)) if image == chosen => {
                YoutubeThumbnail::Failed(problem.clone().into())
            }
            (_, Some(chosen)) if link.thumbnail.as_ref() == Some(chosen) => YoutubeThumbnail::Set,
            (_, Some(_)) => YoutubeThumbnail::NotSet,
            (_, None) if link.thumbnail.is_some() => YoutubeThumbnail::Kept,
            (_, None) => return None,
        })
    }

    fn choose_file(&mut self, window: &mut Window, cx: &mut Context<Self>, thumbnail: bool) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let chosen = match paths.await {
                Ok(Ok(paths)) => paths.and_then(|paths| paths.into_iter().next()),
                // The dialog went away with the window.
                Err(_) => None,
                // Linux needs the desktop portal for file dialogs; without it the path
                // can still be typed or the file dropped on the card.
                Ok(Err(error)) => {
                    log_line!("rushes: file chooser failed: {error}");
                    this.update_in(cx, |this, window, cx| {
                        let target = if thumbnail { FileTarget::Thumbnail } else { FileTarget::Video };
                        this.prompt_for_path(target, window, cx);
                    })
                    .ok();
                    None
                }
            };
            let Some(path) = chosen else { return };
            this.update_in(cx, |this, window, cx| {
                if thumbnail {
                    this.set_thumbnail(Some(path), window, cx);
                } else {
                    this.set_file(Some(path), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Asks for the path of the file in a dialog: what is left when no file chooser opens.
    fn prompt_for_path(&mut self, target: FileTarget, window: &mut Window, cx: &mut Context<Self>) {
        let request = match target {
            FileTarget::Video => PathRequest::new(
                PathKind::File,
                "Type the path of the video instead, or drop the file on the Video file card.",
                target.placeholder(),
            ),
            FileTarget::Thumbnail => PathRequest::new(
                PathKind::File,
                "Type the path of the image instead, or drop the file on the Thumbnail card.",
                target.placeholder(),
            )
            .check(|path| if is_thumbnail_image(path) { Ok(()) } else { Err(NOT_A_THUMBNAIL.into()) }),
        };
        let panel = cx.weak_entity();
        path_prompt::open(request, window, cx, move |path, _, _| {
            let panel = panel.clone();
            path_prompt::accept_now(move |window, cx| {
                _ = panel.update(cx, |panel, cx| match target {
                    FileTarget::Video => panel.set_file(Some(path), cx),
                    FileTarget::Thumbnail => panel.set_thumbnail(Some(path), window, cx),
                });
            })
        });
    }

    /// Whether this video is uploading: its file must stay the one being sent.
    fn is_uploading(&self, cx: &App) -> bool {
        self.service.read(cx).is_uploading_any(&[self.video_id])
    }

    /// Takes the first dropped file for the video file or the thumbnail.
    fn on_drop(&mut self, target: FileTarget, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = paths.paths().iter().find(|path| path.is_file()).cloned() else { return };
        if target == FileTarget::Video && self.is_uploading(cx) {
            window.push_notification(notification::warning(UPLOAD_FILE_LOCKED, cx), cx);
            return;
        }
        match target {
            FileTarget::Video => self.set_file(Some(path), cx),
            FileTarget::Thumbnail => self.set_thumbnail(Some(path), window, cx),
        }
    }

    fn on_tag_input(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = input.read(cx).value().to_string();
        let (complete, rest) = match event {
            InputEvent::PressEnter { .. } => (value.as_str(), ""),
            // A comma finishes a tag, so pasted "a, b, c" becomes three tags.
            InputEvent::Change if value.contains(',') => value.rsplit_once(',').unwrap_or_default(),
            _ => return,
        };
        let added = add_tags(&mut self.draft.tags, complete);
        let rest = rest.trim_start().to_owned();
        input.update(cx, |input, cx| input.set_value(rest, window, cx));
        if added {
            self.schedule_save(Field::Tags, cx);
        }
    }

    fn remove_tag(&mut self, tag: &str, cx: &mut Context<Self>) {
        self.draft.tags.retain(|existing| existing != tag);
        self.schedule_save(Field::Tags, cx);
    }

    fn set_privacy(&mut self, privacy: Privacy, cx: &mut Context<Self>) {
        self.draft.privacy = privacy;
        self.schedule_save(Field::Visibility, cx);
    }

    fn set_scheduled(&mut self, scheduled: bool, cx: &mut Context<Self>) {
        if scheduled {
            let picked = self.schedule.read(cx).value().and_then(utc_time);
            let video = self.state.read(cx).video(self.video_id).cloned().unwrap_or_else(|| Video::new(""));
            self.draft.publish_at =
                Some(picked.filter(|at| *at > Utc::now()).unwrap_or_else(|| default_schedule(&video)));
            // YouTube publishes scheduled videos by turning them public; until then they are private.
            self.unscheduled_privacy = Some(self.draft.privacy);
            self.draft.privacy = Privacy::Private;
        } else {
            self.draft.publish_at = None;
            if let Some(privacy) = self.unscheduled_privacy.take() {
                self.draft.privacy = privacy;
            }
        }
        self.schedule_save(Field::Visibility, cx);
    }

    fn insert_chapters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(chapters) = self.chapters.clone() else { return };
        let description = insert_chapters(&self.draft.description, &chapters.text);
        // Replacing keeps the change undoable and reports it like typing would.
        self.description.update(cx, |input, cx| input.replace_all(description, window, cx));
    }

    /// Whether the video is on YouTube already, so another upload makes a second copy
    /// and asks first.
    fn uploaded_before(&self, cx: &App) -> bool {
        self.state.read(cx).video(self.video_id).is_some_and(|video| video.youtube.is_some())
    }

    fn start_upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_pending(cx);
        if !self.uploaded_before(cx) {
            self.upload_now(window, cx);
            return;
        }
        let title = self.draft_video(cx).map(|video| video.upload_title().to_owned()).unwrap_or_default();
        let panel = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let panel = panel.clone();
            dialog
                .title(format!("Upload “{title}” again?"))
                .description("YouTube gets a second copy of the video. The first upload stays on your channel.")
                .confirm()
                .ok_text("Upload again")
                .on_ok(move |_, window, cx| {
                    panel.update(cx, |panel, cx| panel.upload_now(window, cx)).ok();
                    true
                })
        });
    }

    fn upload_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let video_id = self.video_id;
        let started = self.service.update(cx, |service, cx| service.start_upload(video_id, cx));
        if let Err(error) = started {
            self.on_upload_refused(&error, cx);
            window.push_notification(notification::error(error.to_string(), cx).title("Couldn’t start the upload"), cx);
        }
    }

    fn resume_upload(&mut self, job: &UploadJob, window: &mut Window, cx: &mut Context<Self>) {
        // A file just chosen in the form must count before the upload continues.
        self.save_pending(cx);
        let id = job.id;
        let resumed = self.service.update(cx, |service, cx| service.resume_upload(id, cx));
        if let Err(error) = resumed {
            self.on_upload_refused(&error, cx);
            window
                .push_notification(notification::error(error.to_string(), cx).title("Couldn’t resume the upload"), cx);
        }
    }

    /// A file found missing when the upload starts is read again, so the file card and
    /// the reasons under the button say so too.
    fn on_upload_refused(&mut self, error: &UploadError, cx: &mut Context<Self>) {
        if matches!(error, UploadError::Blocked(reasons) if reasons.contains(&BlockingReason::FileMissing)) {
            self.refresh_file(cx);
        }
    }

    fn render_details(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title_len = self.draft_video(cx).map_or(0, |video| video.upload_title().chars().count());
        let job = self.details_job(cx);
        // The agent replaces these fields; typing meanwhile would be lost.
        let writing = job == Some(DetailsJob::Writing);
        let mut heading = section_heading("Details");
        if let Some(button) = self.write_details_button(writing, cx) {
            heading = heading.action(button);
        }
        let chapters_button = Button::new("insert-chapters")
            .label("Insert chapters from script")
            .icon(IconName::ListOrdered)
            .small()
            .outline()
            .disabled(self.chapters.is_err() || writing)
            .on_click(cx.listener(|this, _, window, cx| this.insert_chapters(window, cx)));
        // The estimate may run past the end of the chosen file, where YouTube ignores it.
        let file_secs = self
            .file
            .as_ref()
            .filter(|file| Some(&file.path) == self.draft.file.as_ref())
            .and_then(|file| file.media.as_ref())
            .map(|media| media.duration_secs);
        let past_the_end = match (&self.chapters, file_secs) {
            (Ok(list), Some(secs)) if f64::from(list.last_start) + 10.0 > secs => Some(format!(
                "The chosen file is {} long, but these chapters run to {}. YouTube ignores chapters past the end.",
                format_duration(secs),
                timestamp(list.last_start)
            )),
            _ => None,
        };
        let chapters_help = match (&self.chapters, past_the_end) {
            (Err(reason), _) => help_text(*reason, cx).id("chapters-unavailable"),
            (Ok(_), Some(warning)) => help_text(warning, cx).text_color(warning_text(cx)).id("chapters-past-the-end"),
            (Ok(_), None) => {
                help_text(format!("Timestamps are estimated from the script at {DEFAULT_WPM} words per minute."), cx)
                    .id("chapters-estimate")
            }
        }
        // Tests check which note is on screen.
        .test_support();
        let uploaded = self.state.read(cx).video(self.video_id).is_some_and(|video| video.youtube.is_some());
        v_flex()
            .gap_5()
            .child(v_flex().gap_0p5().child(heading).when(uploaded, |this| {
                // The form doesn't reach the uploaded video; saying so avoids edits that go nowhere.
                // It reads as the section's description.
                this.child(
                    div()
                        .id("publish-next-upload-note")
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            "This video is on YouTube. Changes here apply to the next upload; change the \
                             uploaded one in YouTube Studio.",
                        )
                        .test_support(),
                )
            }))
            .children(job.map(|job| self.render_details_job(job, cx)))
            .child(
                FieldRow::new("Title")
                    .counter(title_len, TITLE_MAX_CHARS)
                    .help("Leave empty to use the video’s title.")
                    .control(Input::new(&self.title).id("publish-title").disabled(writing)),
            )
            .child(
                FieldRow::new("Description")
                    .counter(self.draft.description.len(), DESCRIPTION_MAX_BYTES)
                    .control(Textarea::new(&self.description).aria_label("Description").disabled(writing)),
            )
            .child(h_flex().gap_3().child(chapters_button).child(chapters_help.flex_1().min_w_0()))
            .child(
                FieldRow::new("Tags")
                    .counter(tags_length(&self.draft.tags), TAGS_MAX_CHARS)
                    .help("Press Enter or type a comma to add a tag.")
                    .control(self.render_tags(writing, cx)),
            )
            .child(
                FieldRow::new("Category").control(
                    div()
                        .max_w(rems(20.))
                        .child(Select::new(&self.category).id("publish-category").placeholder("Category")),
                ),
            )
    }

    /// "Write with AI…", unless the app has no assistant. Without a script or a
    /// transcript there is nothing to write from, which its tooltip says.
    fn write_details_button(&self, writing: bool, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        self.assistant.as_ref()?;
        let reason = (!self.sources.any())
            .then(|| SharedString::from("Write the script first, or run an AI edit to transcribe the footage."));
        let button = Button::new("write-details")
            .label("Write with AI…")
            .icon(IconName::Sparkles)
            .small()
            .outline()
            .disabled(writing || reason.is_some())
            .on_click(cx.listener(|this, _, window, cx| this.open_write_details(window, cx)));
        Some(state_tooltip("write-details-reason", reason, button))
    }

    /// Where the agent writing the details stands, with what can be done about it.
    fn render_details_job(&self, job: DetailsJob, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let cancelling = self.assistant.as_ref().is_some_and(|a| a.read(cx).is_cancelling_details(self.video_id));
        let (status, actions): (Div, Vec<Button>) = match job {
            DetailsJob::Writing => (
                h_flex().gap_2().child(Spinner::new().small().color(theme.muted_foreground)).child(
                    div().text_sm().child(if cancelling {
                        "Stopping…"
                    } else {
                        "Writing the title, description and tags…"
                    }),
                ),
                vec![
                    Button::new("cancel-write-details").label("Cancel").small().ghost().disabled(cancelling).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.with_assistant(cx, |assistant, id, cx| assistant.cancel_details(id, cx))
                        }),
                    ),
                ],
            ),
            DetailsJob::Failed(error) => (
                status_line(IconName::CircleAlert, theme.danger, "Couldn’t write the details", Some(error), cx),
                vec![
                    Button::new("retry-write-details")
                        .label("Try again…")
                        .small()
                        .outline()
                        .on_click(cx.listener(|this, _, window, cx| this.open_write_details(window, cx))),
                    Button::new("dismiss-write-details").label("Dismiss").small().ghost().on_click(cx.listener(
                        |this, _, _, cx| this.with_assistant(cx, |assistant, id, cx| assistant.dismiss_details(id, cx)),
                    )),
                ],
            ),
            DetailsJob::Written(_) => (
                status_line(
                    IconName::Sparkles,
                    theme.muted_foreground,
                    "Written with AI",
                    Some("Read them over before you upload.".into()),
                    cx,
                ),
                vec![
                    Button::new("undo-write-details").label("Undo").small().outline().on_click(cx.listener(
                        |this, _, _, cx| this.with_assistant(cx, |assistant, id, cx| assistant.undo_details(id, cx)),
                    )),
                    Button::new("dismiss-write-details").label("Dismiss").small().ghost().on_click(cx.listener(
                        |this, _, _, cx| this.with_assistant(cx, |assistant, id, cx| assistant.dismiss_details(id, cx)),
                    )),
                ],
            ),
        };
        h_flex()
            .id("details-job")
            .gap_3()
            .items_start()
            .justify_between()
            .child(status.flex_1().min_w_0())
            .child(h_flex().gap_1().flex_shrink_0().children(actions))
            // Tests check what the line says.
            .test_support()
            .into_any_element()
    }

    fn render_tags(&self, writing: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .flex_wrap()
            .gap_1p5()
            .children(self.draft.tags.iter().map(|tag| {
                let remove = tag.clone();
                h_flex()
                    .gap_0p5()
                    .pl_2()
                    .pr_0p5()
                    .rounded(theme.radius)
                    .bg(theme.secondary)
                    .text_color(theme.secondary_foreground)
                    .text_sm()
                    .child(tag.clone())
                    .child(
                        Button::new(SharedString::from(format!("remove-tag-{tag}")))
                            .icon(IconName::Close)
                            .ghost()
                            .xsmall()
                            .accessibility_label(format!("Remove {tag}"))
                            .disabled(writing)
                            .on_click(cx.listener(move |this, _, _, cx| this.remove_tag(&remove, cx))),
                    )
            }))
            .child(
                div()
                    .min_w(rems(12.))
                    .flex_1()
                    .child(Input::new(&self.tag_input).id("publish-tag-input").disabled(writing)),
            )
    }

    fn render_visibility(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let scheduled = self.draft.publish_at.is_some();
        let privacy_ix = Privacy::ALL.iter().position(|privacy| *privacy == self.draft.privacy).unwrap_or(0);
        let google = self.service.read(cx).config().is_ok_and(|config| config.is_google());
        let video = self.state.read(cx).video(self.video_id);
        let link = video.and_then(|video| video.youtube.clone());
        let sync = link.as_ref().and_then(|link| self.service.read(cx).visibility_sync(&link.video_id).cloned());
        // An upload under way takes the form's choice to its new video.
        let other = video
            .filter(|_| !self.is_uploading(cx))
            .and_then(|video| self.service.read(cx).other_visibility_on_youtube(video));
        let status = match (sync, other) {
            (Some(sync), _) => Some(self.render_visibility_sync(sync, cx)),
            (None, Some(other)) => Some(self.render_other_visibility(other, cx)),
            (None, None) => None,
        };
        v_flex()
            .gap_5()
            .child(v_flex().gap_0p5().child(section_heading("Visibility")).when(link.is_some(), |this| {
                // Unlike the details, these reach the uploaded video. It reads as the
                // section's description.
                this.child(
                    div()
                        .id("publish-visibility-note")
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Changes here go to the video on YouTube.")
                        .test_support(),
                )
            }))
            .child(
                FieldRow::new("Who can watch")
                    .help(if scheduled {
                        "Scheduled videos stay private until the publish time."
                    } else {
                        PRIVACY_HELP[privacy_ix]
                    })
                    .control(
                        RadioGroup::horizontal("publish-privacy")
                            .children(Privacy::ALL.map(Privacy::label))
                            .selected_index(Some(privacy_ix))
                            .disabled(scheduled)
                            .on_change(cx.listener(|this, ix: &usize, _, cx| {
                                if let Some(privacy) = Privacy::ALL.get(*ix) {
                                    this.set_privacy(*privacy, cx);
                                }
                            })),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        Switch::new("publish-schedule")
                            .label("Schedule")
                            .checked(scheduled)
                            .on_change(cx.listener(|this, checked: &bool, _, cx| this.set_scheduled(*checked, cx))),
                    )
                    .when(scheduled, |this| {
                        this.child(
                            v_flex()
                                .gap_1p5()
                                .pl_10()
                                .child(
                                    div()
                                        .id("publish-schedule-time")
                                        .test_support()
                                        .max_w(rems(20.))
                                        .child(
                                            DateField::new("publish-schedule-field", "Publish time", &self.schedule)
                                                .placeholder("Publish time"),
                                        ),
                                )
                                .child(help_text(
                                    format!(
                                        "YouTube makes the video public at this time ({}). Until then it stays private.",
                                        Local::now().format("UTC%:z")
                                    ),
                                    cx,
                                )),
                        )
                    }),
            )
            .children(status)
            .when(google, |this| {
                this.child(help_text(
                    "Until Google audits your API project, YouTube locks uploads from it to private.",
                    cx,
                ))
            })
    }

    /// YouTube has `other` instead of the form's choice: it didn't get there, or the
    /// video changed in YouTube Studio. The form's choice goes on request, unless
    /// its schedule is over: YouTube takes no schedule in the past.
    fn render_other_visibility(&self, other: Visibility, cx: &mut Context<Self>) -> AnyElement {
        let text = visibility_text(rules::core_privacy(other.privacy), other.publish_at);
        let mut chars = text.chars();
        let text: String = chars.next().map(|first| first.to_lowercase().chain(chars).collect()).unwrap_or_default();
        let over = self.draft.publish_at.is_some_and(|at| at <= Utc::now());
        let (title, detail) = if over {
            ("Changed on YouTube", format!("On YouTube it’s {text}. Turn off Schedule to choose who can watch it."))
        } else {
            ("Not on YouTube yet", format!("On YouTube it’s {text}."))
        };
        let body = v_flex()
            .gap_2()
            .child(status_line(IconName::TriangleAlert, cx.theme().warning, title, Some(detail.into()), cx))
            .when(!over, |this| {
                this.child(
                    h_flex().child(
                        Button::new("visibility-send")
                            .label("Change on YouTube")
                            .small()
                            .outline()
                            .on_click(cx.listener(|this, _, _, cx| this.send_visibility(cx))),
                    ),
                )
            });
        // Tests check what the section says.
        div().id("youtube-visibility-sync").child(body).test_support().into_any_element()
    }

    /// Where changing who can watch the uploaded video stands, with the way on when
    /// the change didn't get there.
    fn render_visibility_sync(&self, sync: VisibilitySync, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let body = match sync {
            VisibilitySync::Sending => h_flex()
                .gap_2()
                .child(Spinner::new().small().color(theme.muted_foreground))
                .child(div().text_sm().child("Sending to YouTube…")),
            VisibilitySync::Failed { problem, reconnect } => {
                let button = match reconnect {
                    Some(key) => {
                        // One browser sign-in at a time: another one must end first.
                        let busy = self.service.read(cx).is_signing_in();
                        let service = self.service.clone();
                        Button::new("visibility-reconnect")
                            .label("Reconnect")
                            .disabled(busy)
                            .on_click(move |_, _, cx| service.update(cx, |service, cx| service.reconnect(&key, cx)))
                    }
                    None => Button::new("visibility-try-again")
                        .label("Try again")
                        .on_click(cx.listener(|this, _, _, cx| this.send_visibility(cx))),
                };
                v_flex()
                    .gap_2()
                    .child(status_line(
                        IconName::TriangleAlert,
                        theme.warning,
                        "Not changed on YouTube",
                        Some(problem.into()),
                        cx,
                    ))
                    .child(h_flex().child(button.small().outline()))
            }
            VisibilitySync::Kept(privacy) => status_line(
                IconName::TriangleAlert,
                theme.warning,
                format!("YouTube kept the video {}", privacy.api_value()),
                Some("YouTube Studio shows why.".into()),
                cx,
            ),
        };
        // Tests check what the section says.
        div().id("youtube-visibility-sync").child(body).test_support().into_any_element()
    }

    fn render_audience(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().gap_5().child(section_heading("Audience")).child(
            FieldRow::new("Is this video made for kids?")
                .help("YouTube requires an answer for every video. Videos made for kids have comments and personalized ads turned off.")
                .control(
                    RadioGroup::vertical("publish-made-for-kids")
                        .children(["Yes, it’s made for kids", "No, it’s not made for kids"])
                        .selected_index(Some(if self.draft.made_for_kids { 0 } else { 1 }))
                        .on_change(cx.listener(|this, ix: &usize, _, cx| {
                            this.draft.made_for_kids = *ix == 0;
                            this.schedule_save(Field::Audience, cx);
                        })),
                ),
        )
    }

    fn render_upload(&self, cx: &mut Context<Self>) -> AnyElement {
        let reasons = self.blocking_reasons(cx);
        let service = self.service.read(cx);
        let job = service.latest_job(self.video_id).cloned();
        let destination = job.as_ref().and_then(|job| self.upload_destination(job, cx));
        let theme = cx.theme();
        let youtube_url = self
            .state
            .read(cx)
            .video(self.video_id)
            .and_then(|video| video.youtube.as_ref())
            .map(|link| link.url.clone());
        let open_link = |url: String| youtube_link("open-on-youtube", url);

        let body = match &job {
            Some(job) if service.is_active(job.id) => {
                let cancelling = service.is_cancelling(job.id);
                let retry = service.retry(job.id).filter(|_| !cancelling);
                let transfer = service.transfer(job.id);
                let fraction = service.progress(job.id).unwrap_or(0.0);
                // Amount and speed on one line, time left on the next: the column is too
                // narrow for one line, and a wrap in the middle of a figure reads badly.
                // While retrying, speed and time left mean nothing.
                let (amount, eta) = match transfer.filter(|transfer| transfer.total() > 0 && retry.is_none()) {
                    Some(transfer) => {
                        let mut amount =
                            format!("{} of {}", format_bytes(transfer.sent()), format_bytes(transfer.total()));
                        if let Some(speed) = transfer.bytes_per_second() {
                            amount = format!("{amount} · {}", format_speed(speed));
                        }
                        (Some(amount), transfer.eta().map(format_eta))
                    }
                    None => (None, None),
                };
                let id = job.id;
                v_flex()
                    .gap_2()
                    .child(match retry {
                        Some(retry) => div()
                            .id("upload-retrying")
                            .child(status_line(
                                IconName::TriangleAlert,
                                theme.warning,
                                retry.headline(),
                                Some(retry.detail().into()),
                                cx,
                            ))
                            // Tests check that the retry is on screen.
                            .test_support()
                            .into_any_element(),
                        None => h_flex()
                            .justify_between()
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(if cancelling {
                                "Stopping…"
                            } else {
                                "Uploading…"
                            }))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{}%", (fraction * 100.0).floor())),
                            )
                            .into_any_element(),
                    })
                    .child(
                        Progress::new("upload-progress").value(fraction * 100.0).accessibility_label("Upload progress"),
                    )
                    .child(
                        v_flex()
                            .when_some(amount, |this, amount| this.child(help_text(amount, cx)))
                            .when_some(eta, |this, eta| this.child(help_text(eta, cx)))
                            .when_some(destination, |this, name| {
                                this.child(help_text(format!("Uploading to {name}."), cx))
                            }),
                    )
                    .child(h_flex().pt_1().child(
                        Button::new("cancel-upload").label("Cancel").small().outline().disabled(cancelling).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.service.update(cx, |service, cx| service.cancel_upload(id, cx))
                            }),
                        ),
                    ))
                    .into_any_element()
            }
            Some(job @ UploadJob { status: UploadStatus::Processing { .. } | UploadStatus::Done { .. }, .. })
                if !service.is_off_youtube(job, cx) =>
            {
                let processing = matches!(job.status, UploadStatus::Processing { .. });
                let (icon, headline, detail) = match (processing, service.processing(job.id)) {
                    (true, _) | (false, Some(ProcessingOutcome::Processing)) => {
                        (None, "Uploaded", Some("YouTube is processing the video."))
                    }
                    (false, Some(ProcessingOutcome::Ready)) => {
                        (Some(IconName::CircleCheck), "Uploaded to YouTube", Some("Processed and ready."))
                    }
                    (false, Some(ProcessingOutcome::StillProcessing)) => (
                        Some(IconName::CircleCheck),
                        "Uploaded to YouTube",
                        Some("YouTube is still processing it. YouTube Studio shows when it’s done."),
                    ),
                    (false, None) => (Some(IconName::CircleCheck), "Uploaded to YouTube", None),
                };
                // Who can watch it on YouTube: what YouTube says once asked, else what was sent.
                let visibility = self.state.read(cx).video(self.video_id).and_then(|video| {
                    let link = video.youtube.as_ref()?;
                    Some(match service.youtube_video(&link.video_id) {
                        Some(info) => visibility_text(rules::core_privacy(info.privacy), info.publish_at),
                        None => visibility_text(
                            link.privacy,
                            video.publish.publish_at.filter(|_| video.status == VideoStatus::Scheduled),
                        ),
                    })
                });
                let studio_url = self
                    .state
                    .read(cx)
                    .video(self.video_id)
                    .and_then(|video| video.youtube.as_ref())
                    .map(|link| format!("https://studio.youtube.com/video/{}/edit", link.video_id));
                v_flex()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_start()
                            .child(div().pt_0p5().map(|this| match icon {
                                Some(icon) => this.child(Icon::new(icon).text_color(theme.success)),
                                None => this.child(Spinner::new().small().color(theme.muted_foreground)),
                            }))
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .gap_0p5()
                                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(headline))
                                    .when_some(visibility, |this, visibility| {
                                        this.child(
                                            div()
                                                .id("youtube-visibility")
                                                .child(help_text(visibility, cx))
                                                .test_support(),
                                        )
                                    })
                                    .when_some(detail, |this, detail| this.child(help_text(detail, cx))),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_x_4()
                            .gap_y_2()
                            .when_some(youtube_url.clone(), |this, url| this.child(open_link(url)))
                            .when_some(studio_url, |this, url| {
                                this.child(
                                    div().text_sm().child(
                                        Link::new("open-in-studio").href(url).child(
                                            h_flex()
                                                .gap_1()
                                                .child("YouTube Studio")
                                                .child(Icon::new(IconName::ExternalLink).xsmall()),
                                        ),
                                    ),
                                )
                            })
                            .child(
                                Button::new("upload-again")
                                    .label("Upload again…")
                                    .small()
                                    .outline()
                                    .disabled(!reasons.is_empty())
                                    .on_click(cx.listener(|this, _, window, cx| this.start_upload(window, cx))),
                            ),
                    )
                    .when(!reasons.is_empty(), |this| {
                        this.child(render_blockers(&reasons, &self.service, self.channel_key(), cx))
                    })
                    .into_any_element()
            }
            // Resume sends the rest of the file the upload started with, so only while
            // that is still the chosen file.
            Some(job) if service.is_resumable(job) && self.draft.file.as_ref() == Some(&job.file) => {
                let status = match &job.status {
                    UploadStatus::Failed { error } => status_line(
                        IconName::TriangleAlert,
                        theme.warning,
                        "Upload stopped",
                        Some(error.clone().into()),
                        cx,
                    ),
                    _ => status_line(
                        IconName::CirclePause,
                        theme.muted_foreground,
                        "Upload cancelled",
                        Some("Resume continues where it stopped.".into()),
                        cx,
                    ),
                };
                let resume_job = job.clone();
                // The session belongs to the account that opened it.
                let resume_key = job.channel.clone().or_else(|| self.draft.channel.as_ref().map(|c| c.id.clone()));
                let resume_access = service.account_access_for(resume_key.as_deref());
                v_flex()
                    .gap_3()
                    .child(status)
                    .when_some(destination, |this, name| {
                        this.child(
                            help_text(format!("Resume continues the upload to {name}."), cx)
                                .id("resume-destination")
                                // Tests check that the account is on screen.
                                .test_support(),
                        )
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("resume-upload")
                                    .label("Resume")
                                    .icon(IconName::Upload)
                                    .small()
                                    .primary()
                                    .disabled(resume_access != AccountAccess::Connected)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.resume_upload(&resume_job, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("upload-again")
                                    // Over a copy on YouTube, it asks first.
                                    .label(if self.uploaded_before(cx) { "Start over…" } else { "Start over" })
                                    .small()
                                    .ghost()
                                    .disabled(!reasons.is_empty())
                                    .on_click(cx.listener(|this, _, window, cx| this.start_upload(window, cx))),
                            ),
                    )
                    .map(|this| match resume_access {
                        AccountAccess::NotConnected => this.child(help_text("Connect YouTube to resume.", cx)),
                        AccountAccess::Gone => {
                            this.child(help_text("Connect the account this upload went to again to resume.", cx))
                        }
                        AccountAccess::SigningIn => {
                            this.child(help_text("Finish signing in with your browser to resume.", cx))
                        }
                        AccountAccess::NeedsSignIn => this.child(
                            v_flex().gap_1p5().child(help_text("Sign in to YouTube again to resume.", cx)).children(
                                reconnect_button(&self.service, resume_key.as_deref(), cx)
                                    .map(|button| h_flex().child(button)),
                            ),
                        ),
                        AccountAccess::Connected if reasons.is_empty() => this,
                        // Resume only needs the account; starting over needs the whole form.
                        AccountAccess::Connected => {
                            this.child(render_blockers(&reasons, &self.service, self.channel_key(), cx))
                        }
                    })
                    .into_any_element()
            }
            failed => {
                let last = failed.as_ref().and_then(|job| self.last_upload_note(job, service, cx));
                v_flex()
                    .gap_3()
                    .children(last)
                    .child(self.render_upload_button(reasons, cx))
                    .when_some(youtube_url, |this, url| this.child(open_link(url)))
                    .into_any_element()
            }
        };
        card("publish-upload", "Upload").child(body).into_any_element()
    }

    fn render_upload_button(&self, reasons: Vec<BlockingReason>, cx: &mut Context<Self>) -> impl IntoElement {
        // With several accounts, which one it goes to.
        let service = self.service.read(cx);
        let channel = (service.accounts().len() > 1)
            .then(|| self.draft_video(cx).and_then(|video| service.channel_for(&video)))
            .flatten()
            .and_then(|key| service.find_account(&key).map(YoutubeAccount::name));
        let summary = self.draft_video(cx).map(|video| upload_summary(&video, channel.as_deref()));
        v_flex()
            .gap_3()
            .child(
                h_flex().child(
                    Button::new("upload-to-youtube")
                        // The same command as on the done card: over a copy on YouTube,
                        // it makes a second one and asks first.
                        .label(if self.uploaded_before(cx) { "Upload again…" } else { "Upload to YouTube" })
                        .icon(IconName::Upload)
                        .primary()
                        .small()
                        .disabled(!reasons.is_empty())
                        .on_click(cx.listener(|this, _, window, cx| this.start_upload(window, cx))),
                ),
            )
            .map(|this| {
                if reasons.is_empty() {
                    this.when_some(summary, |this, summary| this.child(help_text(summary, cx)))
                } else {
                    this.child(render_blockers(&reasons, &self.service, self.channel_key(), cx))
                }
            })
    }

    /// What the card says about the last upload when it didn't reach YouTube: why
    /// it failed, or why a stopped one can't continue.
    fn last_upload_note(&self, job: &UploadJob, service: &YoutubeService, cx: &App) -> Option<Div> {
        let theme = cx.theme();
        match &job.status {
            UploadStatus::Failed { error } => Some(status_line(
                IconName::CircleAlert,
                theme.danger,
                "The last upload failed",
                Some(error.clone().into()),
                cx,
            )),
            UploadStatus::Cancelled => {
                let detail = if self.draft.file.as_ref() != Some(&job.file) {
                    format!("It was sending {}, not the file chosen now.", file_name(&job.file))
                } else if service.retired(job.id) == Some(Retired::FileChanged) {
                    format!("{} changed since, so the upload starts from the beginning.", file_name(&job.file))
                } else {
                    return None;
                };
                let note = status_line(
                    IconName::CirclePause,
                    theme.muted_foreground,
                    "Upload cancelled",
                    Some(detail.into()),
                    cx,
                );
                // Tests check that the note is on screen.
                Some(div().child(note.id("upload-cancelled-note").test_support()))
            }
            // It reached YouTube, and is gone from there since.
            UploadStatus::Processing { .. } | UploadStatus::Done { .. } => {
                let note = status_line(
                    IconName::CircleAlert,
                    theme.warning,
                    "Not on YouTube anymore",
                    Some("YouTube no longer has this video. It may have been deleted in YouTube Studio.".into()),
                    cx,
                );
                // Tests check that the note is on screen.
                Some(div().child(note.id("youtube-video-gone").test_support()))
            }
            _ => None,
        }
    }

    /// The account the video uploads to, where it stands, and the menu to choose
    /// another one or add one.
    fn render_account(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let service = self.service.read(cx);
        let theme = cx.theme();
        let chosen = self.draft.channel.clone();
        let key = chosen.as_ref().map(|channel| channel.id.clone());
        // The video chose an account that was disconnected since: it says so rather
        // than quietly uploading to another one.
        let gone = chosen.filter(|channel| service.find_account(&channel.id).is_none());
        let shown = match &gone {
            Some(_) => None,
            None => Some(service.connection_for(key.as_deref())),
        };
        // A sign-in that adds an account, while the panel shows another one.
        let adding = service.is_signing_in() && !matches!(shown, Some(Connection::Connecting { .. }));
        let signing_in = service.is_signing_in();
        // Why the last sign-in failed, also while the card shows an account: the
        // panel says it only without one.
        let problem =
            service.sign_in_problem().filter(|_| !signing_in && !matches!(shown, Some(Connection::Failed(_)))).cloned();
        let uploading = self.is_uploading(cx);
        // The form's choice, which may not be saved yet.
        let choices = self.draft_video(cx).map(|video| ChannelChoices::of(&video, cx));
        let panel = cx.weak_entity();
        let picker = choices.filter(|_| !service.accounts().is_empty()).map(|choices| {
            let button = Button::new("choose-account").label("Change").small().outline().dropdown_caret(true);
            let control = if uploading {
                // The upload goes to this account. A disabled button would still open
                // the menu, which owns the click.
                button.disabled(true).into_any_element()
            } else {
                let choose: ChooseChannel = {
                    let panel = panel.clone();
                    Rc::new(move |key, cx| {
                        panel.update(cx, |panel, cx| panel.set_channel(key, cx)).ok();
                    })
                };
                button
                    .dropdown_menu(move |menu, window, _| {
                        let panel = panel.clone();
                        choices
                            .menu(scrolling_menu(menu, window).check_side(Side::Right), choose.clone())
                            .separator()
                            .item(menu_item("Add account…").icon(IconName::Plus).disabled(signing_in).on_click(
                                move |_, _, cx| {
                                    panel.update(cx, |panel, cx| panel.add_account(cx)).ok();
                                },
                            ))
                    })
                    .into_any_element()
            };
            state_tooltip(
                "choose-account-reason",
                uploading.then_some(SharedString::from(UPLOAD_ACCOUNT_LOCKED)),
                control,
            )
        });
        let uses_default =
            key.is_none() && service.accounts().iter().filter(|account| account.has_channel()).count() > 1;
        v_flex()
            .gap_3()
            .map(|this| match &gone {
                Some(channel) => this.child(
                    div()
                        .id("publish-account-gone")
                        .child(status_line(
                            IconName::CircleAlert,
                            theme.warning,
                            format!("“{}” isn’t connected", channel.name),
                            Some("Connect it again, or choose another account.".into()),
                            cx,
                        ))
                        // Tests check that the note is on screen.
                        .test_support(),
                ),
                None => {
                    this.child(ConnectionPanel::new("publish-connection", self.service.clone()).account(key.clone()))
                }
            })
            .when(adding, |this| {
                this.child(div().id("publish-adding-account").child(sign_in_progress(
                    &self.service,
                    service.sign_in_url().cloned(),
                    cx,
                )))
            })
            .when_some(problem, |this, problem| {
                this.child(
                    div()
                        .id("publish-sign-in-failed")
                        .child(status_line(IconName::CircleAlert, theme.danger, "Couldn’t connect", Some(problem), cx))
                        // Tests check that the reason is on screen.
                        .test_support(),
                )
            })
            .when(gone.is_some() && !signing_in, |this| {
                this.child(
                    h_flex().child(
                        Button::new("connect-account")
                            .label("Connect")
                            .small()
                            .disabled(service.config().is_err())
                            .on_click(cx.listener(|this, _, _, cx| this.add_account(cx))),
                    ),
                )
            })
            .when(uses_default, |this| {
                this.child(
                    help_text("Uploads to the default account.", cx)
                        .id("publish-uses-default")
                        // Tests check that the note is on screen.
                        .test_support(),
                )
            })
            .children(picker.map(|picker| h_flex().child(picker)))
    }

    fn render_file(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let panel = cx.weak_entity();
        let uploading = self.is_uploading(cx);
        let current = self.draft.file.clone();
        let choices = self.choices.clone();
        let chosen = current.clone();
        let label = if current.is_some() { "Change" } else { "Choose" };
        let picker = if uploading {
            // The upload sends this file, so the card must keep showing it. A disabled
            // button would still open the menu, which owns the click.
            Button::new("choose-video-file")
                .label(label)
                .small()
                .outline()
                .dropdown_caret(true)
                .disabled(true)
                .into_any_element()
        } else {
            Button::new("choose-video-file")
                .label(label)
                .small()
                .outline()
                .dropdown_caret(true)
                .dropdown_menu(move |menu, window, _| {
                    // The film icons take the left side; the chosen file is ticked on the right.
                    let mut menu = scrolling_menu(menu, window).check_side(Side::Right);
                    for (ix, choice) in choices.iter().enumerate() {
                        if ix == 0 || choices[ix - 1].source != choice.source {
                            menu = menu.label(choice.source);
                        }
                        let (panel, path) = (panel.clone(), choice.path.clone());
                        menu = menu.item(
                            menu_item(file_name(&choice.path))
                                .icon(IconName::Film)
                                .checked(chosen.as_ref() == Some(&choice.path))
                                .on_click(move |_, _, cx| {
                                    panel.update(cx, |panel, cx| panel.set_file(Some(path.clone()), cx)).ok();
                                }),
                        );
                    }
                    if !choices.is_empty() {
                        menu = menu.separator();
                    }
                    let panel = panel.clone();
                    menu.item(menu_item("Choose file…").icon(IconName::FolderOpen).on_click(move |_, window, cx| {
                        panel.update(cx, |panel, cx| panel.choose_file(window, cx, false)).ok();
                    }))
                })
                .into_any_element()
        };

        let body = match &current {
            Some(path) => {
                let details = self.file.as_ref().filter(|file| &file.path == path);
                let source = self.choices.iter().find(|choice| &choice.path == path).map(|choice| choice.source);
                let meta = match details {
                    None => "Reading file details…".to_owned(),
                    Some(FileDetails { size: None, .. }) => "The file is missing.".to_owned(),
                    Some(FileDetails { size: Some(size), media, .. }) => file_meta(*size, media.as_ref()),
                };
                let missing = details.is_some_and(|details| details.size.is_none());
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(
                        div()
                            .flex_shrink_0()
                            .size_9()
                            .rounded(theme.radius)
                            .bg(theme.secondary)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(IconName::Film).text_color(theme.muted_foreground)),
                    )
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).truncate().child(file_name(path)))
                            .when_some(source, |this, source| {
                                this.child(div().text_xs().text_color(theme.muted_foreground).child(source))
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(if missing { theme.danger } else { theme.muted_foreground })
                                    .child(meta),
                            ),
                    )
                    .into_any_element()
            }
            None => help_text(
                "Choose the AI edit’s final cut, a take from the footage, or any video file. You can also drop one here.",
                cx,
            )
            .into_any_element(),
        };
        let (drop_border, drop_fill) = (theme.primary, theme.drop_target);
        card("publish-file", "Video file").child(
            v_flex()
                .id("video-file-drop")
                .gap_3()
                .rounded(theme.radius)
                .drag_over::<ExternalPaths>(move |style, _, _, _| {
                    style.border_1().border_color(drop_border).bg(drop_fill)
                })
                .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                    this.on_drop(FileTarget::Video, paths, window, cx)
                }))
                .child(body)
                .child(h_flex().child(state_tooltip(
                    "choose-video-file-reason",
                    uploading.then_some(UPLOAD_FILE_LOCKED.into()),
                    picker,
                )))
                // Tests drop files here.
                .test_support(),
        )
    }

    fn render_thumbnail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let job_line = self.thumbnail_job(cx).map(|job| self.render_thumbnail_job(job, cx));
        let drawn = self.render_drawn(cx);
        let generated = self.render_generated(job_line, drawn, cx);
        let theme = cx.theme();
        let current = self.draft.thumbnail.clone();
        let size =
            self.thumbnail_size.as_ref().filter(|(path, _)| Some(path) == current.as_ref()).map(|(_, size)| *size);
        let preview = match &current {
            // A rounded parent does not clip its children, so the image rounds itself.
            Some(path) => img(path.clone())
                .w_full()
                .h_full()
                .rounded(theme.radius)
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            None => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_1()
                .text_color(theme.muted_foreground)
                .child(Icon::new(IconName::Image).large())
                .child(div().text_xs().child("Drop an image here"))
                .into_any_element(),
        };
        let mut notes = vec!["1280 × 720 JPG or PNG, up to 2 MB.".to_owned()];
        match size {
            Some(None) => notes.insert(0, "The image is missing.".to_owned()),
            Some(Some(bytes)) if bytes > THUMBNAIL_MAX_BYTES => {
                notes.insert(0, format!("This image is {}; YouTube takes up to 2 MB.", format_bytes(bytes)))
            }
            _ => {}
        }
        let (drop_border, drop_fill) = (theme.primary, theme.drop_target);
        let on_youtube = self.youtube_thumbnail(cx).map(|state| render_youtube_thumbnail(state, cx));
        let theme = cx.theme();
        card("publish-thumbnail", "Thumbnail").child(
            v_flex()
                .gap_3()
                .child(
                    div()
                        .id("thumbnail-drop")
                        .w_full()
                        .aspect_ratio(16. / 9.)
                        .rounded(theme.radius)
                        .overflow_hidden()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.muted)
                        .drag_over::<ExternalPaths>(move |style, _, _, _| style.border_color(drop_border).bg(drop_fill))
                        .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                            this.on_drop(FileTarget::Thumbnail, paths, window, cx)
                        }))
                        .child(preview)
                        // Tests drop files here.
                        .test_support(),
                )
                .when_some(current.as_ref(), |this, path| {
                    this.child(div().text_xs().text_color(theme.muted_foreground).truncate().child(file_name(path)))
                })
                .when_some(on_youtube, |this, on_youtube| this.child(on_youtube))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("choose-thumbnail")
                                .label(if current.is_some() { "Change…" } else { "Choose image…" })
                                .small()
                                .outline()
                                .on_click(cx.listener(|this, _, window, cx| this.choose_file(window, cx, true))),
                        )
                        .when(current.is_some(), |this| {
                            this.child(
                                Button::new("remove-thumbnail")
                                    .label("Remove")
                                    .small()
                                    .ghost()
                                    .on_click(cx.listener(|this, _, window, cx| this.set_thumbnail(None, window, cx))),
                            )
                        }),
                )
                .child(help_text(notes.join(" "), cx))
                .children(generated),
        )
    }

    /// Thumbnails Codex drew, and the button that opens the chat with it: `None`
    /// without an assistant.
    fn render_generated(
        &self,
        job_line: Option<AnyElement>,
        drawn: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.assistant.as_ref()?;
        let empty = job_line.is_none() && drawn.is_none();
        let theme = cx.theme();
        Some(
            v_flex()
                .gap_3()
                .pt_4()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .justify_between()
                        .gap_2()
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Generated"))
                        .child(
                            Button::new("thumbnail-chat")
                                .label("Chat with Codex…")
                                .icon(IconName::Sparkles)
                                .small()
                                .outline()
                                .on_click(cx.listener(|this, _, window, cx| this.open_thumbnail_chat(window, cx))),
                        ),
                )
                .when(empty, |this| {
                    this.child(help_text(
                        "Codex draws them from the video’s title and script, and changes them as you ask: it \
                         remembers the chat.",
                        cx,
                    ))
                })
                .children(job_line)
                .children(drawn)
                .into_any_element(),
        )
    }

    /// Where Codex answering in the thumbnail chat stands, with what can be done
    /// about it.
    fn render_thumbnail_job(&self, job: ThumbnailJob, cx: &mut Context<Self>) -> AnyElement {
        let cancelling = self.assistant.as_ref().is_some_and(|a| a.read(cx).is_cancelling_thumbnails(self.video_id));
        let theme = cx.theme();
        let (status, actions): (Div, Vec<Button>) = match job {
            ThumbnailJob::Working { activity } => (
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(div().pt_0p5().child(Spinner::new().small().color(theme.muted_foreground)))
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(if cancelling {
                                "Stopping…"
                            } else {
                                "Codex is working…"
                            }))
                            .child(help_text(activity.unwrap_or_else(|| "Drawing takes a few minutes.".into()), cx)),
                    ),
                vec![Button::new("cancel-thumbnails").label("Cancel").small().ghost().disabled(cancelling).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.with_assistant(cx, |assistant, id, cx| assistant.cancel_thumbnails(id, cx))
                    }),
                )],
            ),
            ThumbnailJob::Failed(error) => (
                status_line(IconName::CircleAlert, theme.danger, "Codex couldn’t answer", Some(error), cx),
                vec![self.open_chat_button(cx), self.dismiss_thumbnails_button(cx)],
            ),
            ThumbnailJob::StoppedEarly(problem) => (
                status_line(IconName::TriangleAlert, theme.warning, "Codex stopped early", Some(problem), cx),
                vec![self.open_chat_button(cx), self.dismiss_thumbnails_button(cx)],
            ),
        };
        // The side column is narrow: the actions go under the status.
        v_flex()
            .id("thumbnail-job")
            .gap_2()
            .child(status)
            .child(h_flex().gap_1().children(actions))
            // Tests check what the card says.
            .test_support()
            .into_any_element()
    }

    fn open_chat_button(&self, cx: &mut Context<Self>) -> Button {
        Button::new("thumbnail-job-open-chat")
            .label("Open chat…")
            .small()
            .outline()
            .on_click(cx.listener(|this, _, window, cx| this.open_thumbnail_chat(window, cx)))
    }

    fn dismiss_thumbnails_button(&self, cx: &mut Context<Self>) -> Button {
        Button::new("dismiss-thumbnails").label("Dismiss").small().ghost().on_click(cx.listener(|this, _, _, cx| {
            this.with_assistant(cx, |assistant, id, cx| assistant.dismiss_thumbnails(id, cx))
        }))
    }

    /// The thumbnails drawn for the video, newest first. Choosing one makes it the
    /// thumbnail.
    fn render_drawn(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.drawn.is_empty() {
            return None;
        }
        let chosen = self.draft.thumbnail.as_ref();
        let tiles = self.drawn.iter().map(|path| {
            let target = path.clone();
            thumbnail_tile(SharedString::from(format!("drawn-{}", file_name(path))), path, chosen == Some(path), cx)
                .on_click(cx.listener(move |this, _, window, cx| this.set_thumbnail(Some(target.clone()), window, cx)))
                .into_any_element()
        });
        let none_chosen = !self.drawn.iter().any(|path| chosen == Some(path));
        Some(
            v_flex()
                .gap_2()
                .child(div().id("drawn-thumbnails").grid().grid_cols(2).gap_2().children(tiles))
                .when(none_chosen, |this| {
                    this.child(help_text("Choose one to use it. They’re in the video’s thumbnails folder.", cx))
                })
                .into_any_element(),
        )
    }
}

impl Render for PublishPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let two_columns = TWO_COLUMN_MIN_WIDTH.to_pixels(window.rem_size());
        let stacked = self.width.get().is_some_and(|width| width < two_columns);
        let form = v_flex()
            .flex_1()
            .min_w(FORM_MIN_WIDTH)
            .gap_10()
            .child(self.render_details(cx))
            .child(self.render_visibility(cx))
            .child(self.render_audience(cx));
        let upload = self.render_upload(cx);
        // The cards that clear the upload's blocking reasons follow it.
        let cards = [
            card("publish-account", "YouTube account").child(self.render_account(cx)).into_any_element(),
            self.render_file(cx).into_any_element(),
            self.render_thumbnail(cx).into_any_element(),
        ];
        // Too narrow for both columns (a small window, the rail beside it), the
        // Upload card (state, blockers, the upload button) comes first, so the tab
        // opens on the next step; the other cards follow the form.
        let measure = {
            let width = self.width.clone();
            canvas(
                move |bounds, window, _| {
                    let previous = width.replace(Some(bounds.size.width));
                    let was_stacked = previous.is_some_and(|width| width < two_columns);
                    if previous.is_none() || was_stacked != (bounds.size.width < two_columns) {
                        window.request_animation_frame();
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full()
        };
        // The video page owns scrolling and the page inset.
        div()
            .id("publish-panel")
            .test_support()
            .relative()
            .w_full()
            .map(|this| {
                if stacked {
                    this.flex()
                        .flex_col()
                        .gap_10()
                        .child(upload)
                        .child(form)
                        .child(v_flex().w_full().gap_6().children(cards))
                } else {
                    let side = v_flex().w(SIDE_COLUMN_WIDTH).flex_shrink_0().gap_6().child(upload).children(cards);
                    this.flex().flex_row().items_start().gap_8().child(form).child(side)
                }
            })
            .child(measure)
    }
}

/// What stops the upload, one line each, so a disabled upload button never goes unexplained.
/// An expired sign-in comes with the way back.
fn render_blockers(
    reasons: &[BlockingReason],
    service: &Entity<YoutubeService>,
    key: Option<&str>,
    cx: &App,
) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    v_flex()
        .id("upload-blockers")
        .gap_1()
        .children(reasons.iter().enumerate().map(|(ix, reason)| {
            let reconnect =
                (*reason == BlockingReason::NeedsSignIn).then(|| reconnect_button(service, key, cx)).flatten();
            h_flex()
                .gap_2()
                .items_start()
                .text_sm()
                .text_color(muted)
                .child(div().pt_0p5().child(Icon::new(IconName::Circle).xsmall()))
                // Wraps inside the narrow side column instead of running past the card.
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1p5()
                        .child(div().id(("upload-blocker", ix)).child(reason.to_string()).test_support())
                        .children(reconnect.map(|button| h_flex().child(button))),
                )
        }))
        // Tests check that the reasons are on screen.
        .test_support()
}

/// "Reconnect" for the expired sign-in of the account `key` (the default account
/// for `None`); `None` while the settings can't sign in (the account card then
/// offers to open them).
fn reconnect_button(service: &Entity<YoutubeService>, key: Option<&str>, cx: &App) -> Option<Button> {
    let Connection::NeedsSignIn(account) = service.read(cx).connection_for(key) else { return None };
    // One browser sign-in at a time: another one must end first.
    let busy = service.read(cx).is_signing_in();
    let service = service.clone();
    service.read(cx).config().is_ok().then(|| {
        Button::new("upload-reconnect")
            .label("Reconnect")
            .small()
            .outline()
            .disabled(busy)
            .on_click(move |_, _, cx| service.update(cx, |service, cx| service.reconnect(account.key(), cx)))
    })
}

/// `Private · goes public Mon, 12 Oct at 09:00`, or just who can watch.
fn visibility_text(privacy: Privacy, publish_at: Option<DateTime<Utc>>) -> String {
    match publish_at.filter(|at| privacy == Privacy::Private && *at > Utc::now()) {
        Some(at) => format!("Private · goes public {}", at.with_timezone(&Local).format("%a, %-d %b at %H:%M")),
        None => privacy.label().to_owned(),
    }
}

/// "Uploads as unlisted." / "Uploads to Second Channel as private and goes public
/// on Fri, 10 Oct at 09:00."
fn upload_summary(video: &Video, channel: Option<&str>) -> String {
    let uploads = match channel {
        Some(channel) => format!("Uploads to {channel} as"),
        None => "Uploads as".to_owned(),
    };
    match video.publish.publish_at {
        Some(at) => {
            format!("{uploads} private and goes public {}.", at.with_timezone(&Local).format("on %a, %-d %b at %H:%M"))
        }
        None => format!("{uploads} {}.", video.publish.privacy.label().to_lowercase()),
    }
}

fn file_meta(size: u64, media: Option<&MediaInfo>) -> String {
    let mut parts = vec![format_bytes(size)];
    if let Some(media) = media {
        parts.push(format_duration(media.duration_secs));
        if let (Some(width), Some(height)) = (media.width, media.height) {
            parts.push(format!("{width} × {height}"));
        }
    }
    parts.join(" · ")
}

/// The kept cuts among `cuts` (newest first), then the footage videos.
fn file_choices(footage_dir: &Path, cuts: &[PathBuf]) -> Vec<FileChoice> {
    let mut choices: Vec<FileChoice> = kept_cuts(footage_dir)
        .into_iter()
        .filter(|path| cuts.contains(path))
        .map(|path| FileChoice { path, source: "AI edit" })
        .collect();
    // An unreadable footage folder just offers fewer choices.
    let footage = media::list_footage(footage_dir).unwrap_or_default();
    choices.extend(
        footage
            .into_iter()
            .filter(|file| file.kind == MediaKind::Video)
            .map(|file| FileChoice { path: file.path, source: "Footage" }),
    );
    choices
}

/// The chosen thumbnail on YouTube, under the image on the Thumbnail card.
fn render_youtube_thumbnail(state: YoutubeThumbnail, cx: &mut Context<PublishPanel>) -> AnyElement {
    let theme = cx.theme();
    let send = |label: &'static str| {
        h_flex().child(
            Button::new("set-thumbnail-on-youtube")
                .label(label)
                .small()
                .outline()
                .on_click(cx.listener(|this, _, _, cx| this.send_thumbnail(cx))),
        )
    };
    let body = match state {
        YoutubeThumbnail::Sending => h_flex()
            .gap_2()
            .child(Spinner::new().small().color(theme.muted_foreground))
            .child(div().text_sm().child("Sending to YouTube…")),
        YoutubeThumbnail::Set => status_line(IconName::CircleCheck, theme.success, "On YouTube", None, cx),
        YoutubeThumbnail::NotSet => {
            v_flex().gap_2().child(help_text("YouTube doesn’t have this image yet.", cx)).child(send("Set on YouTube"))
        }
        YoutubeThumbnail::Failed(problem) => v_flex()
            .gap_2()
            .child(status_line(IconName::TriangleAlert, theme.warning, "Not set on YouTube", Some(problem), cx))
            .child(send("Try again")),
        YoutubeThumbnail::Kept => help_text("YouTube keeps its thumbnail until you choose another image.", cx),
    };
    // Tests check what the card says.
    div().id("youtube-thumbnail").child(body).test_support().into_any_element()
}

fn is_thumbnail_image(path: &Path) -> bool {
    let extension = path.extension().and_then(|ext| ext.to_str()).map(str::to_ascii_lowercase);
    extension.is_some_and(|ext| THUMBNAIL_EXTENSIONS.contains(&ext.as_str()))
}

/// Adds the comma-separated tags in `text`, skipping duplicates. Returns whether any was new.
fn add_tags(tags: &mut Vec<String>, text: &str) -> bool {
    let mut added = false;
    for tag in text.split(',').map(str::trim).filter(|tag| !tag.is_empty()) {
        if !tags.iter().any(|existing| existing.eq_ignore_ascii_case(tag)) {
            tags.push(tag.to_owned());
            added = true;
        }
    }
    added
}

fn category_index(id: &str) -> usize {
    let id = if id.is_empty() { rules::DEFAULT_CATEGORY } else { id };
    CATEGORIES.iter().position(|category| category.id == id).unwrap_or_default()
}

/// The video's planned publish date at 09:00 local time, or tomorrow at 09:00.
fn default_schedule(video: &Video) -> DateTime<Utc> {
    let tomorrow = Local::now().date_naive() + TimeDelta::days(1);
    let date = video.publish_date.filter(|date| *date >= tomorrow).unwrap_or(tomorrow);
    utc_time(date.and_time(NaiveTime::from_hms_opt(9, 0, 0).unwrap_or_default())).unwrap_or_else(Utc::now)
}

/// The scheduled publish time's field: today onwards, to the minute.
fn schedule_picker(
    value: NaiveDateTime,
    week_start: WeekStart,
    window: &mut Window,
    cx: &mut App,
) -> Entity<DateFieldState> {
    let today = Local::now().date_naive();
    cx.new(|cx| {
        let mut field = DateFieldState::date_time(window, cx)
            .format("%a, %-d %b %Y, %H:%M")
            .disabled_before(today, window, cx)
            .first_day_of_week(week_start.weekday());
        field.set_value(Some(value), window, cx);
        field
    })
}

fn local_time(at: DateTime<Utc>) -> NaiveDateTime {
    at.with_timezone(&Local).naive_local()
}

fn utc_time(local: NaiveDateTime) -> Option<DateTime<Utc>> {
    Local.from_local_datetime(&local).earliest().map(|at| at.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_done_card_says_who_can_watch() {
        let monday = Utc::now() + TimeDelta::days(2);
        let scheduled = visibility_text(Privacy::Private, Some(monday));
        assert!(scheduled.starts_with("Private · goes public "), "{scheduled}");
        assert_eq!(visibility_text(Privacy::Unlisted, None), "Unlisted");
        // Once the time has passed, YouTube made it public; until asked, private is all we know.
        assert_eq!(visibility_text(Privacy::Private, Some(Utc::now() - TimeDelta::days(1))), "Private");
    }

    #[test]
    fn saving_keeps_fields_changed_elsewhere() {
        let draft = PublishSettings { title: "My title".into(), ..PublishSettings::default() };
        let stored = PublishSettings { file: Some("/media/edit/final.mp4".into()), ..PublishSettings::default() };
        let mut edited = Edited::default();
        assert!(edited.is_empty());
        edited.insert(Field::Title);
        let merged = edited.merge(&stored, &draft);
        assert_eq!(merged.title, "My title");
        assert_eq!(merged.file, stored.file, "the final cut AI edit chose stays");
        edited.insert(Field::File);
        assert_eq!(edited.merge(&stored, &draft).file, None, "unless the form changed the file too");
    }

    #[test]
    fn tags_are_added_once_and_trimmed() {
        let mut tags = vec!["Rust".to_owned()];
        assert!(add_tags(&mut tags, " rust, desktop app ,, GPUI "));
        assert_eq!(tags, ["Rust", "desktop app", "GPUI"]);
        assert!(!add_tags(&mut tags, "gpui"));
    }

    #[test]
    fn file_details_read_like_a_player() {
        let media = MediaInfo { duration_secs: 725.2, width: Some(1920), height: Some(1080), has_audio: true };
        assert_eq!(file_meta(1_270_000_000, Some(&media)), "1.3 GB · 12:05 · 1920 × 1080");
        assert_eq!(file_meta(512_000, None), "512 KB");
    }

    #[test]
    fn categories_default_to_people_and_blogs() {
        assert_eq!(CATEGORIES[category_index("")].label, "People & Blogs");
        assert_eq!(CATEGORIES[category_index("27")].label, "Education");
        assert_eq!(category_index("nope"), 0);
    }

    #[test]
    fn edit_outputs_come_before_footage() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        std::fs::create_dir_all(dir.join("edit/cuts")).unwrap();
        let files = [
            "edit/cuts/final-1.mp4",
            "edit/cuts/final-2.mp4",
            "edit/cuts/final-3.mp4",
            "edit/preview.mp4",
            "take1.mov",
            "notes.txt",
        ];
        for file in files {
            std::fs::write(dir.join(file), b"x").unwrap();
        }
        // final-3 is no render's: an earlier version moved a cancelled render there.
        let rendered = [dir.join("edit/cuts/final-1.mp4"), dir.join("edit/cuts/final-2.mp4")];
        let choices = file_choices(dir, &rendered);
        let names: Vec<(String, &str)> =
            choices.iter().map(|choice| (file_name(&choice.path), choice.source)).collect();
        assert_eq!(
            names,
            [
                ("final-2.mp4".to_owned(), "AI edit"),
                ("final-1.mp4".to_owned(), "AI edit"),
                ("take1.mov".to_owned(), "Footage")
            ]
        );
    }
}
