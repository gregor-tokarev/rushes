//! Settings page: General (theme, week start, folders), YouTube, AI editing and About.
//! Embeds `publish::YoutubeSettings` and `ai_edit::AiSettings` as sections.

mod media_folder;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::setting::{SelectIndex, SettingField, SettingGroup, SettingItem, SettingPage, Settings};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, Axis, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
};
use videocrm_core::input::display_user_path;
use videocrm_core::settings::{ThemeMode, WeekStart, data_dir};

use crate::ai_edit::AiSettings;
use crate::publish::YoutubeSettings;
use crate::route::SettingsSection;
use crate::state::{AppEvent, AppState};
use crate::ui::settings::{FIELD_MAX_WIDTH, setting_group};
use crate::update::{self, UpdateService};

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
        let youtube = cx.new(|cx| YoutubeSettings::new(window, cx));
        let ai = cx.new(|cx| AiSettings::new(window, cx));
        let mut subscriptions = vec![
            cx.subscribe(&state, |_, _, event: &AppEvent, cx| {
                if *event == AppEvent::SettingsChanged {
                    cx.notify();
                }
            }),
            // Their fields render inside this page's groups.
            cx.observe(&youtube, |_, _, cx| cx.notify()),
            cx.observe(&ai, |_, _, cx| cx.notify()),
        ];
        // About shows where updating stands.
        if let Some(updates) = UpdateService::try_global(cx) {
            subscriptions.push(cx.observe(&updates, |_, _, cx| cx.notify()));
        }
        Self {
            section,
            opened: 0,
            media_folder: cx.new(|cx| MediaFolderField::new("settings-media-root", window, cx)),
            youtube,
            ai,
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

    fn pages(&self, cx: &App) -> Vec<SettingPage> {
        let media_folder = self.media_folder.clone();
        let youtube_groups: Vec<SettingGroup> = self
            .youtube
            .read(cx)
            .sections(cx)
            .into_iter()
            .map(|section| section.setting_group(&self.youtube))
            .collect();
        let ai_groups: Vec<SettingGroup> =
            AiSettings::sections().into_iter().map(|section| section.setting_group(&self.ai)).collect();
        vec![
            SettingPage::new(SettingsSection::General.label())
                .icon(Icon::new(IconName::Settings2))
                .default_open(true)
                .groups(vec![
                    setting_group().title("Appearance").items(vec![
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
                        SettingItem::new(
                            "Week starts on",
                            SettingField::dropdown(
                                WeekStart::ALL
                                    .iter()
                                    .map(|day| (week_start_key(*day), SharedString::from(day.label())))
                                    .collect(),
                                |cx: &App| week_start_key(AppState::global(cx).read(cx).settings().week_start),
                                |key: SharedString, cx: &mut App| {
                                    let Some(day) = WeekStart::ALL.into_iter().find(|day| week_start_key(*day) == key)
                                    else {
                                        return;
                                    };
                                    AppState::global(cx).update(cx, |state, cx| {
                                        state.update_settings(cx, |settings| settings.week_start = day)
                                    });
                                },
                            ),
                        )
                        .keywords(["calendar", "first day", "monday", "sunday"]),
                    ]),
                    // Paths are long: their fields sit under the label at any width.
                    setting_group().title("Storage").items(vec![
                        SettingItem::new("Media folder", media_folder_field(media_folder))
                            .layout(Axis::Vertical)
                            .description("Each video gets a folder here for raw footage and AI edit outputs.")
                            .keywords(["footage", "path"]),
                        SettingItem::new("Data folder", data_folder_field())
                            .layout(Axis::Vertical)
                            .description("Where VideoCRM keeps its database and your YouTube sign-in.")
                            .keywords(["database", "path"]),
                    ]),
                ]),
            SettingPage::new(SettingsSection::Youtube.label())
                .icon(Icon::new(IconName::CirclePlay))
                .groups(youtube_groups),
            SettingPage::new(SettingsSection::AiEditing.label())
                .icon(Icon::new(IconName::WandSparkles))
                .groups(ai_groups),
            SettingPage::new(SettingsSection::About.label())
                .icon(Icon::new(IconName::Info))
                .group(about_group())
                .groups(update::settings_group(cx)),
        ]
    }
}

/// What VideoCRM is, and its version.
fn about_group() -> SettingGroup {
    setting_group().item(
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
                .child(div().max_w(FIELD_MAX_WIDTH).text_sm().text_color(cx.theme().muted_foreground).child(
                    "Plan videos in a Notion-like database, write scripts in rich documents, edit footage \
                 with AI and upload to YouTube, all on your own computer.",
                ))
        })
        .keywords(["version"]),
    )
}

fn theme_key(mode: ThemeMode) -> SharedString {
    mode.label().to_lowercase().into()
}

fn week_start_key(day: WeekStart) -> SharedString {
    day.label().to_lowercase().into()
}

fn media_folder_field(field: Entity<MediaFolderField>) -> SettingField<SharedString> {
    SettingField::render(move |_, _, _| div().w_full().max_w(FIELD_MAX_WIDTH).child(field.clone()))
}

fn data_folder_field() -> SettingField<SharedString> {
    SettingField::render(|options, _, cx| {
        let dir = data_dir();
        let shown: SharedString = display_user_path(&dir).into();
        h_flex()
            .w_full()
            .max_w(FIELD_MAX_WIDTH)
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
                Clipboard::new("copy-data-folder")
                    .with_size(options.size())
                    .value(dir.display().to_string())
                    .tooltip("Copy path")
                    .accessibility_label("Copy path"),
            )
            .child(
                Button::new("open-data-folder")
                    .ghost()
                    .with_size(options.size())
                    .icon(IconName::FolderOpen)
                    .label("Open folder")
                    .on_click(move |_, _, cx| cx.open_with_system(&dir)),
            )
    })
}

impl Render for SettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page_ix = SettingsSection::ALL.iter().position(|section| *section == self.section).unwrap_or(0);
        let sidebar_width = window.rem_size() * 14.;
        Settings::new(("settings", self.opened))
            .default_selected_index(SelectIndex { page_ix, group_ix: None })
            // One width at every window size: resizing the window widens or
            // narrows the content, not both panes in proportion.
            .sidebar_width(sidebar_width)
            .sidebar_size_range(sidebar_width..sidebar_width)
            .pages(self.pages(cx))
    }
}
