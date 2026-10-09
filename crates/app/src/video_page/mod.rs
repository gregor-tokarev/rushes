//! A video's page: icon, title, hook, properties and the Script / Notes /
//! Footage / Publish tabs. The Footage tab embeds `ai_edit::AiEditPanel` and
//! the Publish tab embeds `publish::PublishPanel`.

mod document;
mod emoji;
mod properties;

use chrono::Utc;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Enter, Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, Focusable as _, FontWeight,
    InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Rems, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    TestSupportExt as _, Window, div, rems,
};
use videocrm_core::{DocumentKind, Video, VideoId};

use crate::ai_edit::AiEditPanel;
use crate::publish::PublishPanel;
use crate::route::{Route, VideoTab};
use crate::shell::elements::empty_state;
use crate::state::{AppEvent, AppState};

pub use document::{DocumentTab, SAVE_DELAY, ScriptRail, ScriptStats};
pub use properties::PropertiesPanel;

/// Leading and trailing inset of the page column, matching the document
/// editor's gutters so headers line up with the script's text.
const COLUMN_INSET: Rems = Rems(3.5);
/// The editor's text width plus both gutters.
const COLUMN_MAX_WIDTH: Rems = Rems(46. + 2. * COLUMN_INSET.0);

pub struct VideoPage {
    state: Entity<AppState>,
    video_id: VideoId,
    tab: VideoTab,
    title: Entity<TextareaState>,
    hook: Entity<InputState>,
    properties: Entity<PropertiesPanel>,
    script: Option<Entity<DocumentTab>>,
    rail: Option<Entity<ScriptRail>>,
    notes: Option<Entity<DocumentTab>>,
    footage: Option<Entity<AiEditPanel>>,
    publish: Option<Entity<PublishPanel>>,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl VideoPage {
    pub fn new(video_id: VideoId, tab: VideoTab, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::global(cx);
        let video = state.read(cx).video(video_id).cloned();
        let title = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .soft_wrap(true)
                .placeholder("Untitled")
                .default_value(video.as_ref().map(|video| video.title.clone()).unwrap_or_default())
        });
        let hook = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("One-line hook — why would someone click?")
                .default_value(video.as_ref().map(|video| video.hook.clone()).unwrap_or_default())
        });
        let properties = cx.new(|cx| PropertiesPanel::new(video_id, window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| {
                if *event == AppEvent::DataChanged {
                    this.sync_header(window, cx);
                }
            }),
            cx.subscribe(&title, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Blur) {
                    this.commit_title(cx);
                }
            }),
            cx.subscribe(&hook, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    this.commit_hook(cx);
                }
            }),
            cx.observe(&state, |_, _, cx| cx.notify()),
        ];
        let mut page = Self {
            state: state.clone(),
            video_id,
            tab,
            title,
            hook,
            properties,
            script: None,
            rail: None,
            notes: None,
            footage: None,
            publish: None,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        if video.is_some() {
            page.ensure_tab(window, cx);
            let focus_script = state.update(cx, |state, _| state.take_script_focus(video_id));
            if focus_script && let Some(script) = page.script.clone() {
                window.defer(cx, move |window, cx| script.update(cx, |script, cx| script.focus_start(window, cx)));
            }
        }
        page
    }

    pub fn video_id(&self) -> VideoId {
        self.video_id
    }

    pub fn tab(&self) -> VideoTab {
        self.tab
    }

    pub fn script(&self) -> Option<&Entity<DocumentTab>> {
        self.script.as_ref()
    }

    pub fn notes(&self) -> Option<&Entity<DocumentTab>> {
        self.notes.as_ref()
    }

    pub fn properties(&self) -> &Entity<PropertiesPanel> {
        &self.properties
    }

    pub fn set_tab(&mut self, tab: VideoTab, window: &mut Window, cx: &mut Context<Self>) {
        if tab == self.tab {
            return;
        }
        self.flush(cx);
        self.tab = tab;
        self.ensure_tab(window, cx);
        cx.notify();
    }

    /// Saves pending edits of the title, hook and documents.
    pub fn flush(&mut self, cx: &mut Context<Self>) {
        self.commit_title(cx);
        self.commit_hook(cx);
        for document in [&self.script, &self.notes].into_iter().flatten() {
            document.update(cx, |document, cx| document.flush(cx));
        }
    }

    /// Creates the content of the current tab the first time it is shown.
    fn ensure_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.video_id;
        match self.tab {
            VideoTab::Script if self.script.is_none() => {
                let scroll = self.scroll.clone();
                let script = cx.new(|cx| DocumentTab::new(id, DocumentKind::Script, scroll, window, cx));
                self.rail = Some(cx.new(|cx| ScriptRail::new(script.clone(), cx)));
                self.script = Some(script);
            }
            VideoTab::Notes if self.notes.is_none() => {
                let scroll = self.scroll.clone();
                self.notes = Some(cx.new(|cx| DocumentTab::new(id, DocumentKind::Notes, scroll, window, cx)));
            }
            VideoTab::Footage if self.footage.is_none() => {
                self.footage = Some(cx.new(|cx| AiEditPanel::new(id, window, cx)));
            }
            VideoTab::Publish if self.publish.is_none() => {
                self.publish = Some(cx.new(|cx| PublishPanel::new(id, window, cx)));
            }
            _ => {}
        }
    }

    fn video<'a>(&self, cx: &'a App) -> Option<&'a Video> {
        self.state.read(cx).video(self.video_id)
    }

    fn edit(&self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Video)) {
        let Some(mut video) = self.video(cx).cloned() else { return };
        let before = video.clone();
        edit(&mut video);
        if video != before {
            video.updated_at = Utc::now();
            self.state.update(cx, |state, cx| state.update_video(video, cx));
        }
    }

    fn commit_title(&mut self, cx: &mut Context<Self>) {
        // Titles are one line; pasted line breaks become spaces.
        let title = self.title.read(cx).value().split_whitespace().collect::<Vec<_>>().join(" ");
        self.edit(cx, |video| video.title = title);
    }

    fn commit_hook(&mut self, cx: &mut Context<Self>) {
        let hook = self.hook.read(cx).value().trim().to_string();
        self.edit(cx, |video| video.hook = hook);
    }

    fn set_icon(&mut self, icon: Option<String>, cx: &mut Context<Self>) {
        self.edit(cx, |video| video.icon = icon);
    }

    /// Shows stored title and hook in the inputs that are not being edited.
    fn sync_header(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(video) = self.video(cx).cloned() else { return };
        if !self.title.read(cx).focus_handle(cx).is_focused(window) && self.title.read(cx).value() != video.title {
            self.title.update(cx, |input, cx| input.set_value(video.title.clone(), window, cx));
        }
        if !self.hook.read(cx).focus_handle(cx).is_focused(window) && self.hook.read(cx).value() != video.hook {
            self.hook.update(cx, |input, cx| input.set_value(video.hook.clone(), window, cx));
        }
    }

    fn on_title_enter(&mut self, _: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.commit_title(cx);
        self.hook.update(cx, |input, cx| input.focus(window, cx));
    }

    fn open_tab(&mut self, tab: VideoTab, cx: &mut Context<Self>) {
        let id = self.video_id;
        self.state.update(cx, |state, cx| state.open_video(id, tab, cx));
    }

    fn duplicate(&mut self, cx: &mut Context<Self>) {
        let id = self.video_id;
        self.flush(cx);
        self.state.update(cx, |state, cx| {
            if let Some(copy) = state.duplicate_video(id, cx) {
                state.open_video(copy, VideoTab::Script, cx);
            }
        });
    }

    fn open_media_folder(&mut self, cx: &mut Context<Self>) {
        let id = self.video_id;
        if let Some(folder) = self.state.update(cx, |state, cx| state.ensure_video_folder(id, cx)) {
            cx.open_with_system(&folder);
        }
    }

    fn copy_video_id(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.video_id.to_string()));
        window.push_notification("Video id copied.", cx);
    }

    fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(video) = self.video(cx) else { return };
        let title: SharedString = format!("Delete “{}”?", display_title(&video.title)).into();
        let (state, id) = (self.state.clone(), self.video_id);
        window.open_alert_dialog(cx, move |alert, _, _| {
            let state = state.clone();
            alert
                .title(title.clone())
                .description("Its script, notes and edit history are removed. Files in its media folder stay on disk.")
                .confirm()
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    state.update(cx, |state, cx| state.delete_videos(&[id], cx));
                    true
                })
        });
    }

    fn render_icon(&self, video: &Video, cx: &mut Context<Self>) -> impl IntoElement {
        let page = cx.weak_entity();
        let has_icon = video.icon.as_deref().is_some_and(|icon| !icon.is_empty());
        let trigger = match video.icon.clone().filter(|icon| !icon.is_empty()) {
            Some(icon) => Button::new("page-icon")
                .ghost()
                .h(rems(4.5))
                .w(rems(4.5))
                .accessibility_label("Change icon")
                .child(div().text_size(rems(3.25)).line_height(rems(4.)).child(icon)),
            None => Button::new("page-icon")
                .ghost()
                .xsmall()
                .icon(IconName::FaceSlightlySmilingPlus)
                .label("Add icon")
                .text_color(cx.theme().muted_foreground),
        };
        Popover::new("page-icon-picker").trigger(trigger).content(move |_, _, cx| {
            let popover = cx.entity().downgrade();
            let pick = |icon: Option<String>| {
                let (page, popover) = (page.clone(), popover.clone());
                move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                    _ = page.update(cx, |page, cx| page.set_icon(icon.clone(), cx));
                    _ = popover.update(cx, |popover, cx| popover.dismiss(window, cx));
                }
            };
            v_flex()
                .gap_2()
                .child(div().text_xs().font_medium().text_color(cx.theme().muted_foreground).child("Page icon"))
                .child(div().grid().grid_cols(8).gap_0p5().children(emoji::EMOJI.iter().map(|emoji| {
                    Button::new(SharedString::from(format!("emoji-{emoji}")))
                        .ghost()
                        .size(rems(2.))
                        .accessibility_label(*emoji)
                        .child(div().text_lg().child(*emoji))
                        .on_click(pick(Some(emoji.to_string())))
                })))
                .when(has_icon, |this| {
                    this.child(
                        Button::new("remove-icon")
                            .ghost()
                            .small()
                            .icon(IconName::Trash)
                            .label("Remove icon")
                            .on_click(pick(None)),
                    )
                })
        })
    }

    fn render_page_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let page = cx.weak_entity();
        Button::new("page-menu")
            .ghost()
            .small()
            .icon(IconName::Ellipsis)
            .accessibility_label("Page actions")
            .tooltip("Page actions")
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let item = |label: &'static str, icon: IconName, run: fn(&mut VideoPage, &mut Window, &mut Context<VideoPage>)| {
                    let page = page.clone();
                    PopupMenuItem::new(label).icon(Icon::new(icon)).on_click(move |_, window, cx| {
                        _ = page.update(cx, |page, cx| run(page, window, cx));
                    })
                };
                menu.item(item("Duplicate", IconName::Copy, |page, _, cx| page.duplicate(cx)))
                    .item(item("Open media folder", IconName::FolderOpen, |page, _, cx| page.open_media_folder(cx)))
                    .item(item("Copy video id", IconName::Hash, VideoPage::copy_video_id))
                    .separator()
                    .item(item("Delete video…", IconName::Trash, VideoPage::confirm_delete))
            })
    }

    fn render_header(&self, video: &Video, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_1()
            .child(h_flex().child(self.render_icon(video, cx)))
            .child(
                div()
                    .id("page-title")
                    .key_context("VideoTitle")
                    .capture_action(cx.listener(Self::on_title_enter))
                    .child(
                        Textarea::new(&self.title)
                            .appearance(false)
                            .text_size(rems(2.))
                            .line_height(rems(2.5))
                            .font_weight(FontWeight::BOLD)
                            .px_0(),
                    ),
            )
            .child(
                Input::new(&self.hook)
                    .id("page-hook")
                    .appearance(false)
                    .px_0()
                    .text_color(cx.theme().muted_foreground),
            )
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = VideoTab::ALL.iter().position(|tab| *tab == self.tab).unwrap_or(0);
        TabBar::new("video-tabs")
            .underline()
            .w_full()
            .selected_index(selected)
            .children(VideoTab::ALL.iter().map(|tab| {
                Tab::new().label(tab.label()).icon(Icon::new(match tab {
                    VideoTab::Script => IconName::ScrollText,
                    VideoTab::Notes => IconName::NotebookPen,
                    VideoTab::Footage => IconName::WandSparkles,
                    VideoTab::Publish => IconName::CloudUpload,
                }))
            }))
            .suffix(self.render_page_menu(cx))
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                if let Some(tab) = VideoTab::ALL.get(*ix) {
                    this.open_tab(*tab, cx);
                }
            }))
    }

    fn render_content(&self) -> AnyElement {
        let column = || v_flex().w_full().max_w(COLUMN_MAX_WIDTH).px(COLUMN_INSET);
        match self.tab {
            VideoTab::Script => self.script.clone().map(IntoElement::into_any_element),
            VideoTab::Notes => self.notes.clone().map(IntoElement::into_any_element),
            VideoTab::Footage => self.footage.clone().map(|panel| column().child(panel).into_any_element()),
            VideoTab::Publish => self.publish.clone().map(|panel| column().child(panel).into_any_element()),
        }
        .unwrap_or_else(|| div().into_any_element())
    }

    fn render_missing(&self, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.clone();
        v_flex()
            .id("video-missing")
            .test_support()
            .size_full()
            .items_center()
            .justify_center()
            .child(
                empty_state(
                    IconName::FileQuestionMark,
                    "This video isn’t here anymore",
                    "It may have been deleted. Your other videos are in the database.",
                )
                .child(Button::new("back-to-videos").outline().small().label("Back to videos").on_click(
                    move |_, _, cx| state.update(cx, |state, cx| state.navigate(Route::default(), cx)),
                )),
            )
            .bg(cx.theme().background)
            .into_any_element()
    }
}

/// A video title for confirmation copy, with a placeholder for untitled videos.
fn display_title(title: &str) -> &str {
    let title = title.trim();
    if title.is_empty() { "Untitled" } else { title }
}

impl Render for VideoPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(video) = self.video(cx).cloned() else { return self.render_missing(cx) };
        let column = || v_flex().w_full().max_w(COLUMN_MAX_WIDTH).px(COLUMN_INSET);
        let show_rail = self.tab == VideoTab::Script && self.script.is_some();

        h_flex()
            .size_full()
            .items_stretch()
            .child(
                div()
                    .id("video-page-scroll")
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        v_flex()
                            .w_full()
                            .items_center()
                            .pt_12()
                            .pb_24()
                            .child(column().child(self.render_header(&video, cx)))
                            .child(column().pt_4().child(self.properties.clone()))
                            .child(column().pt_6().child(self.render_tabs(cx)))
                            .child(div().w_full().pt_4().flex().justify_center().child(self.render_content())),
                    )
                    .vertical_scrollbar(&self.scroll),
            )
            .when_some(self.rail.clone().filter(|_| show_rail), |this, rail| {
                this.child(
                    div()
                        .flex_none()
                        .w_64()
                        .h_full()
                        .border_l_1()
                        .border_color(cx.theme().border)
                        .child(rail),
                )
            })
            .into_any_element()
    }
}
