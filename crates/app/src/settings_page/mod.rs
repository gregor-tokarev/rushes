//! Settings page: General (theme, folders), YouTube, AI editing and About.
//! Embeds `publish::YoutubeSettings` and `ai_edit::AiSettings` as sections.

use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement as _, PathPromptOptions, Render,
    SharedString, Styled as _, Subscription, WeakEntity, Window, div,
};
use videocrm_core::media::default_media_root;
use videocrm_core::settings::{ThemeMode, data_dir};

use crate::ai_edit::AiSettings;
use crate::publish::YoutubeSettings;
use crate::state::{AppEvent, AppState};

pub struct SettingsPage {
    state: Entity<AppState>,
    media_root: Entity<InputState>,
    youtube: Entity<YoutubeSettings>,
    ai: Entity<AiSettings>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsPage {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::global(cx);
        let root = state.read(cx).settings().resolved_media_root();
        let media_root = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(default_media_root().display().to_string())
                .default_value(root.display().to_string())
        });
        let subscriptions = vec![
            cx.subscribe(&media_root, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    this.save_media_root(cx);
                }
            }),
            cx.subscribe_in(&state, window, |this, state, event: &AppEvent, window, cx| {
                if *event == AppEvent::SettingsChanged && !this.media_root.read(cx).focus_handle(cx).is_focused(window)
                {
                    let root = state.read(cx).settings().resolved_media_root().display().to_string();
                    if this.media_root.read(cx).value() != root {
                        this.media_root.update(cx, |input, cx| input.set_value(root, window, cx));
                    }
                }
                cx.notify();
            }),
        ];
        Self {
            state,
            media_root,
            youtube: cx.new(|cx| YoutubeSettings::new(window, cx)),
            ai: cx.new(|cx| AiSettings::new(window, cx)),
            _subscriptions: subscriptions,
        }
    }

    /// Stores the media root; the platform default stays unset so it follows the platform.
    fn save_media_root(&mut self, cx: &mut Context<Self>) {
        let text = self.media_root.read(cx).value().trim().to_string();
        let root = (!text.is_empty()).then(|| PathBuf::from(text)).filter(|root| *root != default_media_root());
        self.state.update(cx, |state, cx| state.update_settings(cx, |settings| settings.media_root = root));
    }

    fn choose_media_root(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let chosen = match paths.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) | Err(_) => None,
                Ok(Err(error)) => {
                    let message = format!("Couldn’t open the folder picker: {error}. Type the path instead.");
                    _ = this.update(cx, |this, cx| this.state.update(cx, |state, cx| state.report_error(message, cx)));
                    None
                }
            };
            if let Some(path) = chosen {
                _ = this.update_in(cx, |this, window, cx| {
                    this.media_root.update(cx, |input, cx| input.set_value(path.display().to_string(), window, cx));
                    this.save_media_root(cx);
                });
            }
        })
        .detach();
    }

    fn pages(&self, cx: &mut Context<Self>) -> Vec<SettingPage> {
        let page = cx.weak_entity();
        let media_root = self.media_root.clone();
        let youtube = self.youtube.clone();
        let ai = self.ai.clone();
        vec![
            SettingPage::new("General").icon(Icon::new(IconName::Settings2)).default_open(true).groups(vec![
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
                                let Some(mode) = ThemeMode::ALL.into_iter().find(|mode| theme_key(*mode) == key) else {
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
                SettingGroup::new().title("Storage").items(vec![
                    SettingItem::new("Media folder", media_folder_field(media_root, page))
                        .description("Each video gets a folder here for raw footage and AI edit outputs.")
                        .keywords(["footage", "path"]),
                    SettingItem::new("Data folder", data_folder_field())
                        .description("The database and YouTube credentials. Set VIDEOCRM_DATA_DIR to move it.")
                        .keywords(["database", "path"]),
                ]),
            ]),
            SettingPage::new("YouTube").icon(Icon::new(IconName::CirclePlay)).group(
                SettingGroup::new().item(
                    SettingItem::render(move |_, _, _| youtube.clone()).keywords(["account", "upload", "google"]),
                ),
            ),
            SettingPage::new("AI editing").icon(Icon::new(IconName::WandSparkles)).group(
                SettingGroup::new().item(SettingItem::render(move |_, _, _| ai.clone()).keywords([
                    "agent",
                    "claude",
                    "video-use",
                ])),
            ),
            SettingPage::new("About").icon(Icon::new(IconName::Info)).group(
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
                            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(
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

fn media_folder_field(media_root: Entity<InputState>, page: WeakEntity<SettingsPage>) -> SettingField<SharedString> {
    SettingField::render(move |options, _, _| {
        let page = page.clone();
        h_flex()
            .w_96()
            .gap_2()
            .child(Input::new(&media_root).id("settings-media-root").with_size(options.size()).flex_1())
            .child(
                Button::new("settings-choose-media-root")
                    .outline()
                    .with_size(options.size())
                    .label("Choose…")
                    .on_click(move |_, window, cx| {
                        _ = page.update(cx, |page, cx| page.choose_media_root(window, cx));
                    }),
            )
    })
}

fn data_folder_field() -> SettingField<SharedString> {
    SettingField::render(|options, _, cx| {
        let dir = data_dir();
        h_flex()
            .gap_2()
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(dir.display().to_string()))
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Settings::new("settings").sidebar_width(window.rem_size() * 14.).pages(self.pages(cx))
    }
}
