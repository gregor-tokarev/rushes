//! The Publish tab: YouTube metadata on the left, account, file, thumbnail and the
//! upload itself on the right.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Local, NaiveTime, TimeDelta, TimeZone as _, Utc};
use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::calendar::Matcher;
use gpui_kit::component::date_picker::{DatePicker, DatePickerEvent, DatePickerState, DateTime as PickerValue};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::link::Link;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::time_field::TimePrecision;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IndexPath, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, PathPromptOptions, Rems, Render, SharedString, Styled as _, StyledImage as _, Subscription,
    Task, Window, div, img, rems,
};
use videocrm_core::document::DEFAULT_WPM;
use videocrm_core::media::{self, MediaKind};
use videocrm_core::{DocumentKind, Privacy, PublishSettings, UploadJob, UploadStatus, Video, VideoId};
use videocrm_pipeline::MediaInfo;

use super::chapters::{chapters, chapters_text, insert_chapters};
use super::rules::{
    self, BlockingReason, CATEGORIES, Category, DESCRIPTION_MAX_BYTES, TAGS_MAX_CHARS, THUMBNAIL_MAX_BYTES,
    TITLE_MAX_CHARS, tags_length,
};
use super::service::YoutubeService;
use super::transfer::{format_bytes, format_duration, format_eta, format_speed};
use super::ui::{ConnectionPanel, FieldRow, card, file_name, help_text, section_heading, status_line};
use super::upload::ProcessingOutcome;
use crate::state::{AppEvent, AppState};

/// Form edits are saved this long after the last keystroke.
const SAVE_DELAY: Duration = Duration::from_millis(500);
const PRIVACY_HELP: [&str; 3] =
    ["Only you can watch it.", "Anyone with the link can watch it.", "Everyone can find and watch it."];
const THUMBNAIL_EXTENSIONS: [&str; 5] = ["jpg", "jpeg", "png", "gif", "bmp"];
/// Wide enough for the cards' buttons; the form takes the rest of the page column.
const SIDE_COLUMN_WIDTH: Rems = Rems(17.);

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

/// The Publish tab of a video page.
pub struct PublishPanel {
    video_id: VideoId,
    state: Entity<AppState>,
    service: Entity<YoutubeService>,
    /// The form's copy of `video.publish`; saved after [`SAVE_DELAY`].
    draft: PublishSettings,
    dirty: bool,
    save_task: Option<Task<()>>,
    title: Entity<InputState>,
    /// The video title, shown while the YouTube title is empty.
    title_placeholder: String,
    description: Entity<TextareaState>,
    tag_input: Entity<InputState>,
    category: Entity<SelectState<Vec<CategoryItem>>>,
    schedule: Entity<DatePickerState>,
    choices: Vec<FileChoice>,
    file: Option<FileDetails>,
    /// Size of the chosen thumbnail, `None` while unknown or missing.
    thumbnail_size: Option<(PathBuf, Option<u64>)>,
    /// Chapter list from the script, `None` when the script has no Heading 2.
    chapters: Option<String>,
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
        let today = Local::now().date_naive();
        let schedule = cx.new(|cx| {
            let mut picker = DatePickerState::new(window, cx)
                .time_precision(TimePrecision::Minute)
                .date_format("%a, %-d %b %Y, %H:%M")
                .disabled_matcher(Matcher::interval(Some(today), None));
            picker.set_date_time(local_time(draft.publish_at.unwrap_or_else(|| default_schedule(&video))), window, cx);
            picker
        });

        let subscriptions = vec![
            cx.subscribe_in(&state, window, Self::on_app_event),
            cx.observe(&service, |_, _, cx| cx.notify()),
            cx.subscribe(&title, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.draft.title = input.read(cx).value().to_string();
                    this.schedule_save(cx);
                }
            }),
            cx.subscribe(&description, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.draft.description = input.read(cx).value().to_string();
                    this.schedule_save(cx);
                }
            }),
            cx.subscribe_in(&tag_input, window, Self::on_tag_input),
            cx.subscribe(&category, |this, _, event: &SelectEvent<Vec<CategoryItem>>, cx| {
                let SelectEvent::Confirm(id) = event;
                this.draft.category_id = id.map(str::to_owned).unwrap_or_default();
                this.schedule_save(cx);
            }),
            cx.subscribe(&schedule, |this, _, event: &DatePickerEvent, cx| {
                let DatePickerEvent::Change(value) = event;
                if this.draft.publish_at.is_some()
                    && let Some(at) = value.start().and_then(utc_time)
                {
                    this.draft.publish_at = Some(at);
                    this.schedule_save(cx);
                }
            }),
            // Coming back from the file manager or another app may bring new files.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.refresh_choices(cx);
                    this.refresh_file(cx);
                }
            }),
            // Leaving the page must not lose the last edits.
            cx.on_release(|this, cx| this.save_pending(cx)),
        ];

        let mut panel = Self {
            video_id,
            state,
            service,
            draft,
            dirty: false,
            save_task: None,
            title,
            title_placeholder: video.title.clone(),
            description,
            tag_input,
            category,
            schedule,
            choices: Vec::new(),
            file: None,
            thumbnail_size: None,
            chapters: None,
            _subscriptions: subscriptions,
        };
        panel.refresh_chapters(cx);
        panel.refresh_choices(cx);
        panel.refresh_file(cx);
        panel.refresh_thumbnail(cx);
        panel
    }

    pub fn video_id(&self) -> VideoId {
        self.video_id
    }

    /// What stops the upload, as the form currently reads.
    pub fn blocking_reasons(&self, cx: &App) -> Vec<BlockingReason> {
        let Some(video) = self.draft_video(cx) else { return Vec::new() };
        let file_exists = self.file.as_ref().is_some_and(|file| file.size.is_some());
        rules::blocking_reasons(&video, self.service.read(cx).is_connected(), file_exists, Utc::now())
    }

    /// Saves pending form edits now instead of after the delay.
    pub fn save_pending(&mut self, cx: &mut App) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let Some(mut video) = self.state.read(cx).video(self.video_id).cloned() else { return };
        if video.publish == self.draft {
            return;
        }
        video.publish = self.draft.clone();
        self.state.update(cx, |state, cx| state.update_video(video, cx));
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.dirty = true;
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
        if *event != AppEvent::DataChanged {
            return;
        }
        let Some(video) = self.state.read(cx).video(self.video_id).cloned() else { return };
        if self.title_placeholder != video.title {
            self.title_placeholder = video.title.clone();
            self.title.update(cx, |input, cx| input.set_placeholder(video.title.clone(), window, cx));
        }
        // Changes made elsewhere (such as "Use for upload" in AI edit) win unless the
        // form has edits of its own waiting.
        if !self.dirty && video.publish != self.draft {
            self.adopt(video.publish, window, cx);
        }
        self.refresh_chapters(cx);
        self.refresh_choices(cx);
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
            self.schedule.update(cx, |picker, cx| picker.set_date_time(local_time(at), window, cx));
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
        let text = (!found.is_empty()).then(|| chapters_text(&found));
        if text != self.chapters {
            self.chapters = text;
            cx.notify();
        }
    }

    /// Lists the AI edit outputs (final cut first) and the footage of the video.
    fn refresh_choices(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.state.read(cx).video(self.video_id).and_then(media::footage_dir) else { return };
        cx.spawn(async move |this, cx| {
            let choices = cx.background_spawn(async move { file_choices(&dir) }).await;
            this.update(cx, |this, cx| {
                if this.choices != choices {
                    this.choices = choices;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
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
                    let media = size.and_then(|_| videocrm_pipeline::probe_media(&path));
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
        self.schedule_save(cx);
    }

    fn set_thumbnail(&mut self, path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = &path {
            let extension = path.extension().and_then(|ext| ext.to_str()).map(str::to_ascii_lowercase);
            if !extension.is_some_and(|ext| THUMBNAIL_EXTENSIONS.contains(&ext.as_str())) {
                window.push_notification(Notification::warning("Choose a JPG, PNG, GIF or BMP image."), cx);
                return;
            }
        }
        self.draft.thumbnail = path;
        self.thumbnail_size = None;
        self.refresh_thumbnail(cx);
        self.schedule_save(cx);
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
                // Linux needs the desktop portal for file dialogs.
                Ok(Err(error)) => {
                    this.update_in(cx, |_, window, cx| {
                        let message = format!("Couldn't open the file dialog: {error}");
                        window.push_notification(Notification::error(message), cx);
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
            self.schedule_save(cx);
        }
    }

    fn remove_tag(&mut self, tag: &str, cx: &mut Context<Self>) {
        self.draft.tags.retain(|existing| existing != tag);
        self.schedule_save(cx);
    }

    fn set_privacy(&mut self, privacy: Privacy, cx: &mut Context<Self>) {
        self.draft.privacy = privacy;
        self.schedule_save(cx);
    }

    fn set_scheduled(&mut self, scheduled: bool, cx: &mut Context<Self>) {
        if scheduled {
            let picked = self.schedule.read(cx).date_time().start().and_then(utc_time);
            let video = self.state.read(cx).video(self.video_id).cloned().unwrap_or_else(|| Video::new(""));
            self.draft.publish_at =
                Some(picked.filter(|at| *at > Utc::now()).unwrap_or_else(|| default_schedule(&video)));
            // YouTube publishes scheduled videos by turning them public; until then they are private.
            self.draft.privacy = Privacy::Private;
        } else {
            self.draft.publish_at = None;
        }
        self.schedule_save(cx);
    }

    fn insert_chapters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chapters) = self.chapters.clone() else { return };
        let description = insert_chapters(&self.draft.description, &chapters);
        // Replacing keeps the change undoable and reports it like typing would.
        self.description.update(cx, |input, cx| input.replace_all(description, window, cx));
    }

    fn start_upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_pending(cx);
        let uploaded_before = self.state.read(cx).video(self.video_id).is_some_and(|video| video.youtube.is_some());
        if !uploaded_before {
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
            window.push_notification(Notification::error(error.to_string()).title("Couldn't start the upload"), cx);
        }
    }

    fn resume_upload(&mut self, job: &UploadJob, window: &mut Window, cx: &mut Context<Self>) {
        let id = job.id;
        let resumed = self.service.update(cx, |service, cx| service.resume_upload(id, cx));
        if let Err(error) = resumed {
            window.push_notification(Notification::error(error.to_string()).title("Couldn't resume the upload"), cx);
        }
    }

    fn render_details(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title_len = self.draft_video(cx).map_or(0, |video| video.upload_title().chars().count());
        let chapters_button = Button::new("insert-chapters")
            .label("Insert chapters from script")
            .icon(IconName::ListOrdered)
            .small()
            .outline()
            .disabled(self.chapters.is_none())
            .when(self.chapters.is_none(), |this| this.tooltip("Add Heading 2 sections to the script first"))
            .on_click(cx.listener(|this, _, window, cx| this.insert_chapters(window, cx)));
        v_flex()
            .gap_5()
            .child(section_heading("Details", cx))
            .child(
                FieldRow::new("Title")
                    .counter(title_len, TITLE_MAX_CHARS)
                    .help("Leave empty to use the video's title.")
                    .control(Input::new(&self.title).id("publish-title")),
            )
            .child(
                FieldRow::new("Description")
                    .counter(self.draft.description.len(), DESCRIPTION_MAX_BYTES)
                    .control(Textarea::new(&self.description).aria_label("Description")),
            )
            .child(
                h_flex().gap_3().child(chapters_button).child(
                    help_text(
                        format!("Timestamps are estimated from the script at {DEFAULT_WPM} words per minute."),
                        cx,
                    )
                    .flex_1()
                    .min_w_0(),
                ),
            )
            .child(
                FieldRow::new("Tags")
                    .counter(tags_length(&self.draft.tags), TAGS_MAX_CHARS)
                    .help("Press Enter or type a comma to add a tag.")
                    .control(self.render_tags(cx)),
            )
            .child(
                FieldRow::new("Category").control(
                    div()
                        .max_w(rems(20.))
                        .child(Select::new(&self.category).id("publish-category").placeholder("Category")),
                ),
            )
    }

    fn render_tags(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
                            .on_click(cx.listener(move |this, _, _, cx| this.remove_tag(&remove, cx))),
                    )
            }))
            .child(div().min_w(rems(12.)).flex_1().child(Input::new(&self.tag_input).id("publish-tag-input")))
    }

    fn render_visibility(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let scheduled = self.draft.publish_at.is_some();
        let privacy_ix = Privacy::ALL.iter().position(|privacy| *privacy == self.draft.privacy).unwrap_or(0);
        let google = self.service.read(cx).config().is_ok_and(|config| config.is_google());
        v_flex()
            .gap_5()
            .child(section_heading("Visibility", cx))
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
                                .child(div().max_w(rems(20.)).child(DatePicker::new(&self.schedule).placeholder("Publish time")))
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
            .when(google, |this| {
                this.child(help_text(
                    "Until Google audits your API project, YouTube locks uploads from it to private.",
                    cx,
                ))
            })
    }

    fn render_audience(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().gap_5().child(section_heading("Audience", cx)).child(
            FieldRow::new("Is this video made for kids?")
                .help("YouTube requires an answer for every video. Videos made for kids have comments and personalized ads turned off.")
                .control(
                    RadioGroup::vertical("publish-made-for-kids")
                        .children(["Yes, it's made for kids", "No, it's not made for kids"])
                        .selected_index(Some(if self.draft.made_for_kids { 0 } else { 1 }))
                        .on_change(cx.listener(|this, ix: &usize, _, cx| {
                            this.draft.made_for_kids = *ix == 0;
                            this.schedule_save(cx);
                        })),
                ),
        )
    }

    fn render_upload(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let service = self.service.read(cx);
        let job = service.latest_job(self.video_id).cloned();
        let theme = cx.theme();
        let youtube_url = self
            .state
            .read(cx)
            .video(self.video_id)
            .and_then(|video| video.youtube.as_ref())
            .map(|link| link.url.clone());
        let open_link =
            |url: String| {
                div()
                    .id("open-on-youtube")
                    .text_sm()
                    .child(Link::new("open-on-youtube-link").href(url).child(
                        h_flex().gap_1().child("Open on YouTube").child(Icon::new(IconName::ExternalLink).xsmall()),
                    ))
                    // Links are not observable on their own; tests find the link through this frame.
                    .test_support()
            };

        let body = match &job {
            Some(job) if service.is_active(job.id) => {
                let cancelling = service.is_cancelling(job.id);
                let transfer = service.transfer(job.id);
                let fraction = transfer.map_or(0.0, |transfer| transfer.fraction());
                // Amount and speed on one line, time left on the next: the column is too
                // narrow for one line, and a wrap in the middle of a figure reads badly.
                let (amount, eta) = match transfer.filter(|transfer| transfer.total() > 0) {
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
                    .child(
                        h_flex()
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
                            ),
                    )
                    .child(
                        Progress::new("upload-progress").value(fraction * 100.0).accessibility_label("Upload progress"),
                    )
                    .child(
                        v_flex()
                            .when_some(amount, |this, amount| this.child(help_text(amount, cx)))
                            .when_some(eta, |this, eta| this.child(help_text(eta, cx))),
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
            Some(job @ UploadJob { status: UploadStatus::Processing { .. } | UploadStatus::Done { .. }, .. }) => {
                let processing = matches!(job.status, UploadStatus::Processing { .. });
                let (icon, headline, detail) = match (processing, service.processing(job.id)) {
                    (true, _) | (false, Some(ProcessingOutcome::Processing)) => {
                        (None, "Uploaded", Some("YouTube is processing the video."))
                    }
                    (false, Some(ProcessingOutcome::Ready)) => {
                        (Some(IconName::CircleCheck), "Uploaded to YouTube", Some("Processed and ready to watch."))
                    }
                    (false, Some(ProcessingOutcome::StillProcessing)) => (
                        Some(IconName::CircleCheck),
                        "Uploaded to YouTube",
                        Some("YouTube is still processing it. YouTube Studio shows when it's done."),
                    ),
                    (false, None) => (Some(IconName::CircleCheck), "Uploaded to YouTube", None),
                };
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
                                    .gap_0p5()
                                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(headline))
                                    .when_some(detail, |this, detail| this.child(help_text(detail, cx))),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_x_4()
                            .gap_y_2()
                            .when_some(youtube_url.clone(), |this, url| this.child(open_link(url)))
                            .child(
                                Button::new("upload-again")
                                    .label("Upload again")
                                    .small()
                                    .outline()
                                    .disabled(!self.blocking_reasons(cx).is_empty())
                                    .on_click(cx.listener(|this, _, window, cx| this.start_upload(window, cx))),
                            ),
                    )
                    .into_any_element()
            }
            Some(job) if service.is_resumable(job) => {
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
                v_flex()
                    .gap_3()
                    .child(status)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("resume-upload")
                                    .label("Resume")
                                    .icon(IconName::Upload)
                                    .small()
                                    .primary()
                                    .disabled(!service.is_connected())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.resume_upload(&resume_job, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("upload-again")
                                    .label("Start over")
                                    .small()
                                    .ghost()
                                    .disabled(!self.blocking_reasons(cx).is_empty())
                                    .on_click(cx.listener(|this, _, window, cx| this.start_upload(window, cx))),
                            ),
                    )
                    .when(!service.is_connected(), |this| this.child(help_text("Connect the account to resume.", cx)))
                    .into_any_element()
            }
            failed => {
                let failure = match failed.as_ref().map(|job| &job.status) {
                    Some(UploadStatus::Failed { error }) => Some(error.clone()),
                    _ => None,
                };
                v_flex()
                    .gap_3()
                    .when_some(failure, |this, error| {
                        this.child(status_line(
                            IconName::CircleAlert,
                            theme.danger,
                            "The last upload failed",
                            Some(error.into()),
                            cx,
                        ))
                    })
                    .child(self.render_upload_button(window, cx))
                    .when_some(youtube_url, |this, url| this.child(open_link(url)))
                    .into_any_element()
            }
        };
        card("publish-upload", "Upload").child(body).into_any_element()
    }

    fn render_upload_button(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let reasons = self.blocking_reasons(cx);
        let summary = self.draft_video(cx).map(|video| upload_summary(&video));
        v_flex()
            .gap_3()
            .child(
                Button::new("upload-to-youtube")
                    .label("Upload to YouTube")
                    .icon(IconName::Upload)
                    .primary()
                    .w_full()
                    .disabled(!reasons.is_empty())
                    .on_click(cx.listener(|this, _, window, cx| this.start_upload(window, cx))),
            )
            .map(|this| {
                if reasons.is_empty() {
                    this.when_some(summary, |this, summary| this.child(help_text(summary, cx)))
                } else {
                    this.child(v_flex().id("upload-blockers").gap_1().children(reasons.iter().map(|reason| {
                        h_flex()
                            .gap_2()
                            .items_start()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(div().pt_0p5().child(Icon::new(IconName::Circle).xsmall()))
                            .child(reason.to_string())
                    })))
                }
            })
    }

    fn render_file(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let panel = cx.weak_entity();
        let current = self.draft.file.clone();
        let choices = self.choices.clone();
        let chosen = current.clone();
        let picker = Button::new("choose-video-file")
            .label(if current.is_some() { "Change" } else { "Choose" })
            .small()
            .outline()
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                for (ix, choice) in choices.iter().enumerate() {
                    if ix == 0 || choices[ix - 1].source != choice.source {
                        menu = menu.label(choice.source);
                    }
                    let (panel, path) = (panel.clone(), choice.path.clone());
                    menu = menu.item(
                        PopupMenuItem::new(file_name(&choice.path))
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
                menu.item(PopupMenuItem::new("Choose file…").icon(IconName::FolderOpen).on_click(
                    move |_, window, cx| {
                        panel.update(cx, |panel, cx| panel.choose_file(window, cx, false)).ok();
                    },
                ))
            });

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
            None => help_text("Choose the AI edit's final cut, a take from the footage, or any video file.", cx)
                .into_any_element(),
        };
        card("publish-file", "Video file").child(v_flex().gap_3().child(body).child(h_flex().child(picker)))
    }

    fn render_thumbnail(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
                .child(div().text_xs().child("No thumbnail"))
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
        card("publish-thumbnail", "Thumbnail").child(
            v_flex()
                .gap_3()
                .child(
                    div()
                        .w_full()
                        .aspect_ratio(16. / 9.)
                        .rounded(theme.radius)
                        .overflow_hidden()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.muted)
                        .child(preview),
                )
                .when_some(current.as_ref(), |this, path| {
                    this.child(div().text_xs().text_color(theme.muted_foreground).truncate().child(file_name(path)))
                })
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
                .child(help_text(notes.join(" "), cx)),
        )
    }
}

impl Render for PublishPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let form = v_flex()
            .flex_1()
            .min_w(rems(20.))
            .gap_10()
            .child(self.render_details(cx))
            .child(self.render_visibility(cx))
            .child(self.render_audience(cx));
        let side = v_flex()
            .w(SIDE_COLUMN_WIDTH)
            .flex_shrink_0()
            .gap_6()
            // The cards that clear the upload's blocking reasons follow it.
            .child(self.render_upload(window, cx))
            .child(
                card("publish-account", "YouTube account")
                    .child(ConnectionPanel::new("publish-connection", self.service.clone())),
            )
            .child(self.render_file(cx))
            .child(self.render_thumbnail(cx));
        // The video page owns scrolling and the page inset.
        h_flex().id("publish-panel").w_full().items_start().gap_8().child(form).child(side)
    }
}

/// "Uploads as unlisted." / "Publishes Fri, 10 Oct at 09:00."
fn upload_summary(video: &Video) -> String {
    match video.publish.publish_at {
        Some(at) => {
            format!("Uploads as private and goes public {}.", at.with_timezone(&Local).format("on %a, %-d %b at %H:%M"))
        }
        None => format!("Uploads as {}.", video.publish.privacy.label().to_lowercase()),
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

/// AI edit outputs first (final cut, then preview), then the footage videos.
fn file_choices(footage_dir: &Path) -> Vec<FileChoice> {
    let outputs = media::edit_outputs(footage_dir);
    let mut choices: Vec<FileChoice> = [outputs.final_cut, outputs.preview]
        .into_iter()
        .flatten()
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

fn local_time(at: DateTime<Utc>) -> PickerValue {
    PickerValue::Single(Some(at.with_timezone(&Local).naive_local()))
}

fn utc_time(local: chrono::NaiveDateTime) -> Option<DateTime<Utc>> {
    Local.from_local_datetime(&local).earliest().map(|at| at.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        std::fs::create_dir_all(dir.join("edit")).unwrap();
        for file in ["edit/preview.mp4", "edit/final.mp4", "take1.mov", "notes.txt", "voice.wav"] {
            std::fs::write(dir.join(file), b"x").unwrap();
        }
        let choices = file_choices(dir);
        let names: Vec<(String, &str)> =
            choices.iter().map(|choice| (file_name(&choice.path), choice.source)).collect();
        assert_eq!(
            names,
            [
                ("final.mp4".to_owned(), "AI edit"),
                ("preview.mp4".to_owned(), "AI edit"),
                ("take1.mov".to_owned(), "Footage")
            ]
        );
    }
}
