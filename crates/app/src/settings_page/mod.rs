//! Settings page: General (theme, folders), YouTube, AI editing and About.
//! Embeds `publish::YoutubeSettings` and `ai_edit::AiSettings` as sections.

mod media_folder;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::setting::{SelectIndex, SettingField, SettingGroup, SettingItem, SettingPage, Settings};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, Axis, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, rems,
};
use videocrm_core::input::display_user_path;
use videocrm_core::settings::{ThemeMode, data_dir};

use crate::ai_edit::AiSettings;
use crate::publish::YoutubeSettings;
use crate::route::SettingsSection;
use crate::state::{AppEvent, AppState};

pub use media_folder::{Commit, MediaFolderEvent, MediaFolderField};

pub struct SettingsPage {
    section: SettingsSection,
    /// Bumped by [`SettingsPage::open_section`]: the kit's settings keep their
    /// selected page in element state, so a new key is how a section is opened.
    opened: usize,
    media_folder: Entity<MediaFolderField>,
    youtube: Entity<YoutubeSettings>,
    ai: Entity<AiSettings>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsPage {
    pub fn new(section: SettingsSection, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::global(cx);
        let subscriptions = vec![cx.subscribe(&state, |_, _, event: &AppEvent, cx| {
            if *event == AppEvent::SettingsChanged {
                cx.notify();
            }
        })];
        Self {
            section,
            opened: 0,
            media_folder: cx.new(|cx| MediaFolderField::new("settings-media-root", window, cx)),
            youtube: cx.new(|cx| YoutubeSettings::new(window, cx)),
            ai: cx.new(|cx| AiSettings::new(window, cx)),
            _subscriptions: subscriptions,
        }
    }

    /// The section shown when the page was last opened; the creator can pick another in its list.
    pub fn section(&self) -> SettingsSection {
        self.section
    }

    /// Shows `section`, replacing the page the creator picked in the list.
    pub fn open_section(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        self.section = section;
        self.opened += 1;
        cx.notify();
    }

    pub fn media_folder(&self) -> &Entity<MediaFolderField> {
        &self.media_folder
    }

    /// Saves edits still in a field when the page is left or the app quits: a
    /// typed media folder that can't be used is put back, with a notification.
    pub fn commit_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.media_folder.update(cx, |field, cx| field.commit(Commit::Revert, false, window, cx).detach());
    }

    /// Saves edits still in a field as the app quits.
    pub fn commit_before_quit(&mut self, cx: &mut Context<Self>) {
        self.media_folder.update(cx, |field, cx| field.commit_before_quit(cx));
    }

    fn pages(&self) -> Vec<SettingPage> {
        let media_folder = self.media_folder.clone();
        let youtube = self.youtube.clone();
        let ai = self.ai.clone();
        vec![
            SettingPage::new(SettingsSection::General.label())
                .icon(Icon::new(IconName::Settings2))
                .default_open(true)
                .groups(vec![
                    SettingGroup::new().title("Appearance").item(
                        SettingItem::new(
                            "Theme",
                            SettingField::dropdown(
                                ThemeMode::ALL
                                    .iter()
                                    .map(|mode| (theme_key(*mode), SharedString::from(mode.label())))
                                    .collect(),
                                |cx: &App| theme_key(AppState::global(cx).read(cx).settings().theme),
                                |key: SharedString, cx: &mut App| {
                                    let Some(mode) = ThemeMode::ALL.into_iter().find(|mode| theme_key(*mode) == key)
                                    else {
                                        return;
                                    };
                                    AppState::global(cx).update(cx, |state, cx| {
                                        state.update_settings(cx, |settings| settings.theme = mode)
                                    });
                                },
                            ),
                        )
                        .description("System follows your operating system’s light or dark appearance."),
                    ),
                    // Paths are long: their fields sit under the label at any width.
                    SettingGroup::new().title("Storage").items(vec![
                        SettingItem::new("Media folder", media_folder_field(media_folder))
                            .layout(Axis::Vertical)
                            .description("Each video gets a folder here for raw footage and AI edit outputs.")
                            .keywords(["footage", "path"]),
                        SettingItem::new("Data folder", data_folder_field())
                            .layout(Axis::Vertical)
                            .description("The database and YouTube credentials. Set VIDEOCRM_DATA_DIR to move it.")
                            .keywords(["database", "path"]),
                    ]),
                ]),
            SettingPage::new(SettingsSection::Youtube.label()).icon(Icon::new(IconName::CirclePlay)).group(
                SettingGroup::new().item(
                    SettingItem::render(move |_, _, _| youtube.clone()).keywords(["account", "upload", "google"]),
                ),
            ),
            SettingPage::new(SettingsSection::AiEditing.label()).icon(Icon::new(IconName::WandSparkles)).group(
                SettingGroup::new().item(SettingItem::render(move |_, _, _| ai.clone()).keywords([
                    "agent",
                    "claude",
                    "video-use",
                ])),
            ),
            SettingPage::new(SettingsSection::About.label()).icon(Icon::new(IconName::Info)).group(
                SettingGroup::new().item(
                    SettingItem::render(|_, _, cx| {
                        v_flex()
                            .w_full()
                            .gap_2()
                            .child(
                                h_flex()
                                    .gap_3()
                                    .child(
                                        div()
                                            .flex()
                                            .size_10()
                                            .items_center()
                                            .justify_center()
                                            .rounded(cx.theme().radius_lg)
                                            .bg(cx.theme().primary)
                                            .text_color(cx.theme().primary_foreground)
                                            .child(Icon::new(IconName::Clapperboard)),
                                    )
                                    .child(
                                        v_flex().child(div().font_semibold().child("VideoCRM")).child(
                                            div()
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!("Version {}", env!("CARGO_PKG_VERSION"))),
                                        ),
                                    ),
                            )
                            .child(div().max_w(rems(40.)).text_sm().text_color(cx.theme().muted_foreground).child(
                                "Plan videos in a Notion-like database, write scripts in rich documents, edit footage \
                             with AI and upload to YouTube, all on your own computer.",
                            ))
                    })
                    .keywords(["version"]),
                ),
            ),
        ]
    }
}

fn theme_key(mode: ThemeMode) -> SharedString {
    mode.label().to_lowercase().into()
}

fn media_folder_field(field: Entity<MediaFolderField>) -> SettingField<SharedString> {
    SettingField::render(move |_, _, _| div().w_full().max_w(rems(36.)).child(field.clone()))
}

fn data_folder_field() -> SettingField<SharedString> {
    SettingField::render(|options, _, cx| {
        let dir = data_dir();
        let shown: SharedString = display_user_path(&dir).into();
        h_flex()
            .w_full()
            .max_w(rems(36.))
            .min_w_0()
            .gap_2()
            .child(
                div()
                    .id("data-folder-path")
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .tooltip({
                        let full: SharedString = dir.display().to_string().into();
                        move |window, cx| Tooltip::new(full.clone()).build(window, cx)
                    })
                    .child(shown),
            )
            .child(
                Button::new("open-data-folder")
                    .outline()
                    .with_size(options.size())
                    .icon(IconName::FolderOpen)
                    .label("Open")
                    .on_click(move |_, _, cx| cx.open_with_system(&dir)),
            )
    })
}

impl Render for SettingsPage {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let page_ix = SettingsSection::ALL.iter().position(|section| *section == self.section).unwrap_or(0);
        Settings::new(("settings", self.opened))
            .default_selected_index(SelectIndex { page_ix, group_ix: None })
            .sidebar_width(window.rem_size() * 14.)
            .pages(self.pages())
    }
}
