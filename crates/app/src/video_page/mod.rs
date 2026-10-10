//! A video's page: icon, title, hook, properties and the Script / Notes /
//! Footage / Publish tabs. The Footage tab embeds `ai_edit::AiEditPanel` and
//! the Publish tab embeds `publish::PublishPanel`.

mod document;
mod emoji;
mod properties;
mod saves;

use std::collections::HashMap;

use chrono::Utc;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Enter, Input, InputEvent, InputState, MoveDown, Position, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _, Size, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, Focusable as _, FontWeight, Global,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Pixels, Rems, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _,
    Window, div, point, px, rems,
};
use videocrm_core::{DocumentKind, Video, VideoId};

use crate::ai_edit::AiEditPanel;
use crate::publish::PublishPanel;
use crate::route::{Route, VideoTab};
use crate::state::{AppEvent, AppState};
use crate::ui::dialog::confirm_delete_videos;
use crate::ui::{EmptyState, PageIcon};

pub use document::{DocumentTab, PathPurpose, SAVE_DELAY, ScriptRail, ScriptStats};
pub use emoji::ICON_CHOICES;
pub use properties::PropertiesPanel;
pub use saves::{DocumentSaves, SAVE_RETRY_DELAY};

/// Leading and trailing inset of the page column, matching the document
/// editor's gutters so headers line up with the script's text.
const COLUMN_INSET: Rems = Rems(3.5);
/// The editor's text width plus both gutters.
const COLUMN_MAX_WIDTH: Rems = Rems(46. + 2. * COLUMN_INSET.0);
/// Narrower windows leave the script rail out so the script keeps its width;
/// its content moves to the script menu next to the tabs.
const RAIL_MIN_WINDOW_WIDTH: Rems = Rems(72.);
/// Width of the script rail. Wide windows keep this lane free on every tab,
/// so switching tabs never moves the header or the tab strip.
const RAIL_WIDTH: Rems = Rems(16.);
/// Side of the page icon above the title.
const PAGE_ICON_SIZE: Rems = Rems(3.5);
/// The tab strip's index among the scrolled page's children: header,
/// properties, tab strip, tab content, page end.
const TAB_STRIP_ITEM: usize = 2;

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
    /// How far each tab was scrolled when it was left, restored when it is shown again.
    scrolled: HashMap<VideoTab, Pixels>,
    _subscriptions: Vec<Subscription>,
}

/// A pending request to show a tab of a video from its first section.
#[derive(Default)]
struct TabReveal(Option<(VideoId, VideoTab)>);

impl Global for TabReveal {}

/// Opens `tab` of a video scrolled so that the tab strip is at the top of the
/// page and the tab's first section is in view, whatever the tab was
/// scrolled to before. For actions that send the creator to a tab to act
/// there, such as "Go to Publish".
pub fn open_tab_at_top(id: VideoId, tab: VideoTab, cx: &mut App) {
    cx.set_global(TabReveal(Some((id, tab))));
    AppState::global(cx).update(cx, |state, cx| state.open_video(id, tab, cx));
}

impl VideoPage {
    pub fn new(video_id: VideoId, tab: VideoTab, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::global(cx);
        let saves = DocumentSaves::global(cx);
        if !cx.has_global::<TabReveal>() {
            cx.set_global(TabReveal::default());
        }
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
            cx.subscribe_in(&title, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Blur) {
                    this.commit_title(cx);
                    this.sync_header(window, cx);
                }
            }),
            cx.subscribe_in(&hook, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Blur => this.commit_hook(cx),
                // The hook is one line: Enter continues in the document below it.
                InputEvent::PressEnter { .. } => {
                    this.commit_hook(cx);
                    this.focus_document_start(window, cx);
                }
                _ => {}
            }),
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.observe(&saves, |_, _, cx| cx.notify()),
            cx.observe_global::<TabReveal>(|this, cx| {
                if take_tab_reveal(this.video_id, this.tab, cx) {
                    this.reveal_tab_strip();
                    cx.notify();
                }
            }),
            // The page writes its own edits when the app quits: the save queue
            // writes what it holds then, and the page's last keystrokes must be in it.
            cx.on_app_quit(|this, cx| {
                this.flush(cx);
                let saves = DocumentSaves::global(cx);
                saves.update(cx, |saves, cx| saves.write_now(cx));
                async {}
            }),
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
            scrolled: HashMap::new(),
            _subscriptions: subscriptions,
        };
        if video.is_some() {
            page.ensure_tab(window, cx);
            if take_tab_reveal(video_id, tab, cx) {
                page.reveal_tab_strip();
            }
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

    pub fn title_field(&self) -> &Entity<TextareaState> {
        &self.title
    }

    /// The page's scroll position: how far it is scrolled down.
    pub fn scroll_top(&self) -> Pixels {
        -self.scroll.offset().y
    }

    /// Scrolls the page down by `distance` from the top, e.g. to restore a position.
    pub fn set_scroll_top(&mut self, distance: Pixels, cx: &mut Context<Self>) {
        self.scroll.set_offset(point(px(0.), -distance));
        cx.notify();
    }

    /// Where the tab strip starts, as a scroll position that puts it at the top.
    pub fn tab_strip_top(&self) -> Pixels {
        self.scroll
            .bounds_for_item(TAB_STRIP_ITEM)
            .map_or(Pixels::ZERO, |strip| (strip.top() - self.scroll.bounds().top()).max(Pixels::ZERO))
    }

    /// Shows `tab`. Each tab keeps its own scroll position; a tab shown for
    /// the first time, or one that was left above its content, opens where
    /// the tab strip stays put, so the strip doesn't move under the pointer.
    pub fn set_tab(&mut self, tab: VideoTab, window: &mut Window, cx: &mut Context<Self>) {
        let reveal = take_tab_reveal(self.video_id, tab, cx);
        if tab != self.tab {
            self.flush(cx);
            let scrolled = self.scroll_top();
            self.scrolled.insert(self.tab, scrolled);
            self.tab = tab;
            self.ensure_tab(window, cx);
            let strip = self.tab_strip_top();
            let target = match self.scrolled.get(&tab) {
                Some(saved) if *saved > strip => *saved,
                _ => scrolled.min(strip),
            };
            self.scroll.set_offset(point(px(0.), -target));
        }
        if reveal {
            self.reveal_tab_strip();
        }
        cx.notify();
    }

    /// Scrolls the tab strip to the top of the page on the next layout.
    fn reveal_tab_strip(&mut self) {
        self.scroll.scroll_to_top_of_item(TAB_STRIP_ITEM);
    }

    /// Saves pending edits of the title, hook, properties and documents.
    pub fn flush(&mut self, cx: &mut Context<Self>) {
        self.commit_title(cx);
        self.commit_hook(cx);
        self.properties.update(cx, |properties, cx| properties.flush(cx));
        for document in [&self.script, &self.notes].into_iter().flatten() {
            document.update(cx, |document, cx| document.flush(cx));
        }
    }

    /// Creates the content of the current tab the first time it is shown. A
    /// deleted video (reached through history) gets none.
    fn ensure_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.video(cx).is_none() {
            return;
        }
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

    /// The document the current tab edits, if it is Script or Notes.
    fn document(&self) -> Option<&Entity<DocumentTab>> {
        match self.tab {
            VideoTab::Script => self.script.as_ref(),
            VideoTab::Notes => self.notes.as_ref(),
            VideoTab::Footage | VideoTab::Publish => None,
        }
    }

    fn focus_document_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(document) = self.document().cloned() {
            document.update(cx, |document, cx| document.focus_start(window, cx));
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

    /// Saves the typed title. An emptied title is not saved, as the New video
    /// dialog requires one; the field shows the stored title again.
    fn commit_title(&mut self, cx: &mut Context<Self>) {
        // Titles are one line; pasted line breaks become spaces.
        let title = self.title.read(cx).value().split_whitespace().collect::<Vec<_>>().join(" ");
        if title.is_empty() {
            return;
        }
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

    /// Enter ends the title: it is saved and typing continues at the end of the hook.
    fn on_title_enter(&mut self, _: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.commit_title(cx);
        self.hook.update(cx, |input, cx| {
            let end = input.value().chars().count() as u32;
            input.set_cursor_position(Position::new(0, end), window, cx);
        });
    }

    /// Down in the one-line hook continues in the document below it.
    fn on_hook_down(&mut self, _: &MoveDown, window: &mut Window, cx: &mut Context<Self>) {
        if self.document().is_none() {
            return;
        }
        cx.stop_propagation();
        self.commit_hook(cx);
        self.focus_document_start(window, cx);
    }

    /// A click in the space below a document puts the caret after its last
    /// block, like a click right under it.
    fn on_page_end_click(&mut self, _: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(document) = self.document().cloned() else { return };
        cx.stop_propagation();
        window.prevent_default();
        document.update(cx, |document, cx| document.focus_end(window, cx));
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
        let (state, id) = (self.state.clone(), self.video_id);
        confirm_delete_videos(vec![(id, video.title.clone())], window, cx, move |ids, cx| {
            state.update(cx, |state, cx| state.delete_videos(&ids, cx));
        });
    }

    fn render_icon(&self, video: &Video, cx: &mut Context<Self>) -> impl IntoElement {
        let page = cx.weak_entity();
        let current = video.icon.clone().filter(|icon| !icon.is_empty());
        let has_icon = current.is_some();
        let trigger = match current.clone() {
            // The icon sits on the column's edge; its hover surface bleeds past it.
            Some(icon) => Button::new("page-icon")
                .ghost()
                .h(rems(4.5))
                .w(rems(4.5))
                .ml(rems(-0.5))
                .accessibility_label("Change icon")
                .child(PageIcon::new(Some(&icon)).size(PAGE_ICON_SIZE)),
            None => Button::new("page-icon")
                .ghost()
                .small()
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
                .child(div().grid().grid_cols(8).gap_0p5().children(ICON_CHOICES.iter().map(|emoji| {
                    Button::new(SharedString::from(format!("emoji-{emoji}")))
                        .ghost()
                        .size(rems(2.25))
                        .accessibility_label(*emoji)
                        .selected(current.as_deref() == Some(*emoji))
                        .child(PageIcon::new(Some(emoji)).size(rems(1.375)))
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
                let item = |label: &'static str,
                            icon: IconName,
                            run: fn(&mut VideoPage, &mut Window, &mut Context<VideoPage>)| {
                    let page = page.clone();
                    PopupMenuItem::new(label).icon(Icon::new(icon)).on_click(move |_, window, cx| {
                        _ = page.update(cx, |page, cx| run(page, window, cx));
                    })
                };
                menu.item(item("Duplicate", IconName::Copy, |page, _, cx| page.duplicate(cx)))
                    .item(item("Open media folder", IconName::FolderOpen, |page, _, cx| page.open_media_folder(cx)))
                    .item(item("Copy video id", IconName::Hash, VideoPage::copy_video_id))
                    .separator()
                    .item(item("Delete…", IconName::Trash, VideoPage::confirm_delete))
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
                    .test_support()
                    .key_context("VideoTitle")
                    .capture_action(cx.listener(Self::on_title_enter))
                    .child(
                        // A textarea pads its text by its size's input inset;
                        // the margin puts the title on the column's edge.
                        Textarea::new(&self.title)
                            .appearance(false)
                            .xsmall()
                            .ml(-Size::XSmall.input_px())
                            .text_size(rems(2.))
                            .line_height(rems(2.5))
                            .font_weight(FontWeight::BOLD),
                    ),
            )
            .child(div().capture_action(cx.listener(Self::on_hook_down)).child(
                Input::new(&self.hook).id("page-hook").appearance(false).px_0().text_color(cx.theme().muted_foreground),
            ))
    }

    fn render_tabs(&self, wide: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = VideoTab::ALL.iter().position(|tab| *tab == self.tab).unwrap_or(0);
        // Without the rail, its content is one menu away next to the tabs.
        let script_menu = self.script.as_ref().filter(|_| !wide && self.tab == VideoTab::Script);
        let unsaved = self.state.read(cx).is_save_failing(self.video_id)
            || [&self.script, &self.notes].into_iter().flatten().any(|tab| tab.read(cx).is_save_failing(cx));
        TabBar::new("video-tabs")
            .underline()
            .w_full()
            .selected_index(selected)
            .children(VideoTab::ALL.iter().map(|tab| Tab::new().label(tab.label())))
            .suffix(
                h_flex()
                    .gap_1()
                    .when(unsaved, |this| {
                        this.child(
                            h_flex()
                                .id("not-saved")
                                .test_support()
                                .gap_1()
                                .px_2()
                                .text_xs()
                                .text_color(cx.theme().warning)
                                .child(Icon::new(IconName::CircleAlert).xsmall())
                                .child("Not saved — retrying"),
                        )
                    })
                    .when_some(script_menu, |this, script| this.child(document::script_menu_button(script, cx)))
                    .child(self.render_page_menu(cx)),
            )
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
        div()
            .size_full()
            .bg(cx.theme().background)
            .child(
                EmptyState::new(
                    "video-missing",
                    IconName::FileQuestionMark,
                    "This video isn’t here anymore",
                    "It may have been deleted. Your other videos are in the database.",
                )
                .action(
                    Button::new("back-to-videos")
                        .outline()
                        .small()
                        .label("Back to videos")
                        .on_click(move |_, _, cx| state.update(cx, |state, cx| state.navigate(Route::default(), cx))),
                ),
            )
            .into_any_element()
    }
}

/// Whether someone asked to show `tab` of `video` from the top; asking clears
/// a matching request.
fn take_tab_reveal(video: VideoId, tab: VideoTab, cx: &mut App) -> bool {
    let requested = cx.try_global::<TabReveal>().and_then(|reveal| reveal.0) == Some((video, tab));
    if requested {
        cx.set_global(TabReveal(None));
    }
    requested
}

impl Render for VideoPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(video) = self.video(cx).cloned() else { return self.render_missing(cx) };
        let column = || v_flex().flex_none().w_full().max_w(COLUMN_MAX_WIDTH).px(COLUMN_INSET);
        let wide = window.viewport_size().width >= RAIL_MIN_WINDOW_WIDTH.to_pixels(window.rem_size());
        let rail = self.rail.clone().filter(|_| wide && self.tab == VideoTab::Script);
        let has_document = self.document().is_some();

        div()
            .relative()
            .size_full()
            .child(
                // The page scrolls as a whole; its direct children are the
                // items `TAB_STRIP_ITEM` counts.
                v_flex()
                    .id("video-page-scroll")
                    .test_support()
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .items_center()
                    .pt_12()
                    .when(wide, |this| this.pr(RAIL_WIDTH))
                    .child(column().child(self.render_header(&video, cx)))
                    .child(column().pt_4().child(self.properties.clone()))
                    .child(column().pt_6().child(self.render_tabs(wide, cx)))
                    .child(div().flex_none().w_full().pt_4().flex().justify_center().child(self.render_content()))
                    .child(div().id("page-end").test_support().w_full().flex_1().min_h_24().when(
                        has_document,
                        |this| {
                            this.cursor_text().on_mouse_down(MouseButton::Left, cx.listener(Self::on_page_end_click))
                        },
                    )),
            )
            .child(
                // The scrollbar sits against the rail when it is shown.
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .right(if rail.is_some() { RAIL_WIDTH } else { Rems(0.) })
                    .child(Scrollbar::vertical(&self.scroll).id("video-page-scrollbar").viewport_from_layout()),
            )
            .when_some(rail, |this, rail| {
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .w(RAIL_WIDTH)
                        .occlude()
                        .border_l_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().background)
                        .child(rail),
                )
            })
            .into_any_element()
    }
}
