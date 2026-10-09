//! The YouTube section of Settings and of onboarding. A creator sets up their own
//! Google OAuth client, connects, and learns what Google will show them. The local
//! emulators are a developer option behind a disclosure.

use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::collapsible::Collapsible;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::link::Link;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Rems,
    Render, SharedString, Styled as _, Subscription, Task, Window, div,
};
use videocrm_core::settings::{DEFAULT_EMULATOR_GOOGLE_URL, DEFAULT_EMULATOR_YOUTUBE_URL, YoutubeBackend};

use super::service::{Connection, EmulatorCheck, YoutubeService, probe_emulators};
use super::ui::{ConnectionPanel, FieldRow, help_text, status_line};
use crate::state::{AppEvent, AppState};

/// Typing in a field is saved this long after the last keystroke.
const SAVE_DELAY: Duration = Duration::from_millis(500);
/// Fields stop growing here: an ID or a URL does not need the whole page width.
const FIELD_MAX_WIDTH: Rems = Rems(36.);

/// One step of the Google Cloud setup, with the console page it happens on.
struct SetupStep {
    text: &'static str,
    link: Option<(&'static str, &'static str)>,
}

/// Google's current flow for a personal Desktop app client (docs/youtube.md has the long form).
const GOOGLE_STEPS: [SetupStep; 6] = [
    SetupStep {
        text: "Create a project in Google Cloud, named for example VideoCRM.",
        link: Some(("New project", "https://console.cloud.google.com/projectcreate")),
    },
    SetupStep {
        text: "Enable the YouTube Data API v3 for the project.",
        link: Some(("YouTube Data API v3", "https://console.cloud.google.com/apis/library/youtube.googleapis.com")),
    },
    SetupStep {
        text: "In Google Auth Platform, click Get started: app name VideoCRM, your email, audience External.",
        link: Some(("Google Auth Platform", "https://console.cloud.google.com/auth/overview")),
    },
    SetupStep {
        text: "Under Audience, leave the app in Testing and add the Google account of your channel as a test user.",
        link: Some(("Audience", "https://console.cloud.google.com/auth/audience")),
    },
    SetupStep {
        text: "Under Clients, create a client of type Desktop app. Copy its client ID and client secret.",
        link: Some(("Clients", "https://console.cloud.google.com/auth/clients")),
    },
    SetupStep { text: "Paste both here, then click Connect.", link: None },
];

/// What Google shows during and after the sign-in, so none of it comes as a surprise.
const GOOGLE_EXPECTATIONS: [&str; 4] = [
    "Google warns that it hasn't verified the app. That's expected for your own client: click Continue.",
    "Leave every permission box ticked, especially “Manage your YouTube videos”.",
    "While the app is in Testing, Google ends the sign-in after 7 days. Reconnect here when VideoCRM asks.",
    "Until Google audits your project, YouTube keeps the videos it uploads private.",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackendKind {
    Google,
    Emulator,
}

/// YouTube section used by the Settings page and the onboarding YouTube step: the
/// OAuth client, connect / disconnect, and the developer emulator option.
pub struct YoutubeSettings {
    state: Entity<AppState>,
    service: Entity<YoutubeService>,
    kind: BackendKind,
    client_id: Entity<InputState>,
    client_secret: Entity<InputState>,
    google_url: Entity<InputState>,
    youtube_url: Entity<InputState>,
    /// Field edits wait for [`SAVE_DELAY`] before they are saved.
    dirty: bool,
    save_task: Option<Task<()>>,
    check: Option<EmulatorCheck>,
    /// The Google Cloud setup steps are unfolded.
    steps_open: bool,
    /// The developer options are unfolded.
    developer_open: bool,
    _subscriptions: Vec<Subscription>,
}

impl YoutubeSettings {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::global(cx);
        let service = YoutubeService::global(cx);
        let backend = state.read(cx).settings().youtube.clone();
        let (kind, [client_id, client_secret, google_url, youtube_url]) = match backend {
            YoutubeBackend::Google { client_id, client_secret } => {
                (BackendKind::Google, [client_id, client_secret, String::new(), String::new()])
            }
            YoutubeBackend::Emulator { google_url, youtube_url } => {
                (BackendKind::Emulator, [String::new(), String::new(), google_url, youtube_url])
            }
        };
        let mut input = |value: String, placeholder: &str, masked: bool| {
            cx.new(|cx| {
                InputState::new(window, cx).placeholder(placeholder.to_owned()).masked(masked).default_value(value)
            })
        };
        let client_id = input(client_id, "1234567890-abc.apps.googleusercontent.com", false);
        let client_secret = input(client_secret, "GOCSPX-…", true);
        let google_url = input(google_url, DEFAULT_EMULATOR_GOOGLE_URL, false);
        let youtube_url = input(youtube_url, DEFAULT_EMULATOR_YOUTUBE_URL, false);

        let mut subscriptions = vec![
            cx.observe(&service, |_, _, cx| cx.notify()),
            cx.subscribe_in(&state, window, Self::on_app_event),
            // Leaving the page or quitting must not lose the last edits.
            cx.on_release(|this, cx| this.save_pending(cx)),
            cx.on_app_quit(|this, cx| {
                this.save_pending(cx);
                async {}
            }),
        ];
        for field in [&client_id, &client_secret, &google_url, &youtube_url] {
            subscriptions.push(cx.subscribe(field, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    // A check result is about the addresses as they were.
                    this.check = None;
                    this.schedule_save(cx);
                }
            }));
        }
        let developer_open = kind == BackendKind::Emulator || service.read(cx).is_env_override();
        Self {
            state,
            service,
            kind,
            client_id,
            client_secret,
            google_url,
            youtube_url,
            dirty: false,
            save_task: None,
            check: None,
            steps_open: false,
            developer_open,
            _subscriptions: subscriptions,
        }
    }

    /// The outcome of the last emulator check, while the addresses are unchanged.
    pub fn emulator_check(&self) -> Option<&EmulatorCheck> {
        self.check.as_ref()
    }

    /// Saves pending field edits now instead of after the delay.
    pub fn save_pending(&mut self, cx: &mut App) {
        if self.dirty {
            self.save(cx);
        }
    }

    fn save(&mut self, cx: &mut App) {
        self.dirty = false;
        let backend = self.form_backend(cx);
        self.state.update(cx, |state, cx| state.update_settings(cx, |settings| settings.youtube = backend));
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.dirty = true;
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            this.update(cx, |this, cx| this.save_pending(cx)).ok();
        }));
        cx.notify();
    }

    /// The backend the form describes.
    fn form_backend(&self, cx: &App) -> YoutubeBackend {
        let value = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();
        match self.kind {
            BackendKind::Google => {
                YoutubeBackend::Google { client_id: value(&self.client_id), client_secret: value(&self.client_secret) }
            }
            BackendKind::Emulator => {
                YoutubeBackend::Emulator { google_url: value(&self.google_url), youtube_url: value(&self.youtube_url) }
            }
        }
    }

    fn set_kind(&mut self, kind: BackendKind, cx: &mut Context<Self>) {
        if self.kind != kind {
            self.kind = kind;
            self.check = None;
            self.save(cx);
            cx.notify();
        }
    }

    /// Settings changed elsewhere (the other embedding of this section) show up here
    /// unless this form has edits of its own waiting.
    fn on_app_event(
        &mut self,
        state: &Entity<AppState>,
        event: &AppEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if *event != AppEvent::SettingsChanged || self.dirty {
            return;
        }
        let backend = state.read(cx).settings().youtube.clone();
        if backend == self.form_backend(cx) {
            return;
        }
        let set = |input: &Entity<InputState>, value: &str, window: &mut Window, cx: &mut Context<Self>| {
            input.update(cx, |input, cx| {
                if input.value().as_ref() != value {
                    input.set_value(value.to_owned(), window, cx);
                }
            });
        };
        match &backend {
            YoutubeBackend::Google { client_id, client_secret } => {
                self.kind = BackendKind::Google;
                set(&self.client_id.clone(), client_id, window, cx);
                set(&self.client_secret.clone(), client_secret, window, cx);
            }
            YoutubeBackend::Emulator { google_url, youtube_url } => {
                self.kind = BackendKind::Emulator;
                self.developer_open = true;
                set(&self.google_url.clone(), google_url, window, cx);
                set(&self.youtube_url.clone(), youtube_url, window, cx);
            }
        }
        // Adopting stored values is not an edit.
        self.dirty = false;
        self.save_task = None;
        cx.notify();
    }

    /// Asks both emulators whether they are up.
    fn check_emulators(&mut self, cx: &mut Context<Self>) {
        let url = |input: &Entity<InputState>, default: &str| {
            let value = input.read(cx).value().trim().trim_end_matches('/').to_owned();
            if value.is_empty() { default.to_owned() } else { value }
        };
        let google = url(&self.google_url, DEFAULT_EMULATOR_GOOGLE_URL);
        let youtube = url(&self.youtube_url, DEFAULT_EMULATOR_YOUTUBE_URL);
        self.check = Some(EmulatorCheck::Checking);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { probe_emulators(&google, &youtube) }).await;
            this.update(cx, |this, cx| {
                // Edited addresses dropped the check; this answer is about the old ones.
                if this.check == Some(EmulatorCheck::Checking) {
                    this.check = Some(result);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Setup steps and the client fields.
    fn render_google(&self, locked: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let steps = GOOGLE_STEPS.iter().enumerate().map(|(ix, step)| {
            h_flex()
                .gap_2()
                .items_start()
                .text_sm()
                .child(div().w_4().flex_shrink_0().text_color(theme.muted_foreground).child(format!("{}.", ix + 1)))
                .child(v_flex().flex_1().min_w_0().gap_0p5().child(step.text).when_some(
                    step.link,
                    |this, (label, url)| {
                        let id = format!("google-step-{}", ix + 1);
                        this.child(
                            h_flex().child(
                                div()
                                    .id(SharedString::from(id.clone()))
                                    .child(Link::new(SharedString::from(format!("{id}-link"))).href(url).child(
                                        h_flex().gap_1().child(label).child(Icon::new(IconName::ExternalLink).xsmall()),
                                    ))
                                    // Links are not observable on their own; tests find them through this frame.
                                    .test_support(),
                            ),
                        )
                    },
                ))
        });
        let open = self.steps_open;
        let toggle = Button::new("youtube-setup-steps")
            .label("How to create one (about ten minutes)")
            .icon(if open { IconName::ChevronDown } else { IconName::ChevronRight })
            .ghost()
            .small()
            .on_click(cx.listener(|this, _, _, cx| {
                this.steps_open = !this.steps_open;
                cx.notify();
            }));
        v_flex()
            .max_w(FIELD_MAX_WIDTH)
            .gap_4()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Your Google OAuth client"))
                    .child(help_text(
                        "VideoCRM uploads through an OAuth client in your own Google Cloud project. Creating one is free.",
                        cx,
                    )),
            )
            .child(
                Collapsible::new()
                    .open(open)
                    .child(h_flex().child(toggle))
                    .content(v_flex().id("google-setup-steps").gap_2().pt_2().children(steps)),
            )
            .child(
                FieldRow::new("Client ID")
                    .control(Input::new(&self.client_id).id("youtube-client-id").disabled(locked)),
            )
            .child(
                FieldRow::new("Client secret").control(
                    Input::new(&self.client_secret).id("youtube-client-secret").mask_toggle().disabled(locked),
                ),
            )
    }

    /// What Google will show, right below Connect.
    fn render_expectations(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .id("youtube-expectations")
            .max_w(FIELD_MAX_WIDTH)
            .gap_1p5()
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("When you connect"))
            .children(GOOGLE_EXPECTATIONS.iter().map(|note| {
                h_flex()
                    .gap_2()
                    .items_start()
                    // A plain dot: an outlined circle reads as a radio button.
                    .child(div().mt_1p5().size_1().flex_shrink_0().rounded_full().bg(muted))
                    .child(help_text(*note, cx).flex_1().min_w_0())
            }))
            // Tests check that the notes are on screen.
            .test_support()
    }

    /// The developer option: the local emulators instead of YouTube.
    fn render_developer(&self, locked: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let service = self.service.read(cx);
        let env_target = service
            .is_env_override()
            .then(|| service.config().map_or_else(|_| "the emulators".to_owned(), |config| config.backend_label()));
        let emulator = self.kind == BackendKind::Emulator;
        let open = self.developer_open;
        let toggle = Button::new("youtube-developer")
            .label("Developer: local emulator")
            .icon(if open { IconName::ChevronDown } else { IconName::ChevronRight })
            .ghost()
            .small()
            .on_click(cx.listener(|this, _, _, cx| {
                this.developer_open = !this.developer_open;
                cx.notify();
            }));
        let content = v_flex()
            .max_w(FIELD_MAX_WIDTH)
            .gap_4()
            .pt_3()
            .child(
                v_flex()
                    .gap_1p5()
                    .child(
                        Switch::new("youtube-use-emulator")
                            .label("Connect to the local emulators instead of YouTube")
                            .checked(emulator || env_target.is_some())
                            .disabled(locked || env_target.is_some())
                            .on_change(cx.listener(|this, on: &bool, _, cx| {
                                this.set_kind(if *on { BackendKind::Emulator } else { BackendKind::Google }, cx)
                            })),
                    )
                    .child(help_text(
                        "For development and demos without a Google account. Start them with \
                         scripts/emulators.sh. They keep accounts and uploads in memory until they stop.",
                        cx,
                    )),
            )
            .when_some(env_target, |this, target| {
                this.child(help_text(
                    format!(
                        "VIDEOCRM_GOOGLE_URL or VIDEOCRM_YOUTUBE_URL is set, so VideoCRM connects to {target} \
                         whatever this says."
                    ),
                    cx,
                ))
            });
        Collapsible::new().open(open).child(h_flex().child(toggle)).content(content)
    }

    fn render_emulator(&self, locked: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let checking = self.check == Some(EmulatorCheck::Checking);
        let result = match &self.check {
            Some(EmulatorCheck::Running) => {
                Some(status_line(IconName::CircleCheck, theme.success, "Both emulators are running", None, cx))
            }
            Some(EmulatorCheck::Unreachable(problem)) => {
                Some(status_line(IconName::CircleAlert, theme.danger, "Not reachable", Some(problem.clone()), cx))
            }
            Some(EmulatorCheck::Checking) | None => None,
        };
        v_flex()
            .max_w(FIELD_MAX_WIDTH)
            .gap_4()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Local emulators"))
                    .child(help_text("Empty fields use the default addresses.", cx)),
            )
            .child(
                FieldRow::new("Google emulator")
                    .control(Input::new(&self.google_url).id("youtube-google-url").disabled(locked)),
            )
            .child(
                FieldRow::new("YouTube emulator")
                    .control(Input::new(&self.youtube_url).id("youtube-youtube-url").disabled(locked)),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(
                        Button::new("check-emulators")
                            .label("Check")
                            .small()
                            .outline()
                            .loading(checking)
                            .disabled(checking)
                            .on_click(cx.listener(|this, _, _, cx| this.check_emulators(cx))),
                    )
                    .children(result.map(|result| div().pt_0p5().min_w_0().child(result))),
            )
    }
}

impl Render for YoutubeSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let service = self.service.read(cx);
        let connection = service.connection().clone();
        let env_override = service.is_env_override();
        let emulator = self.kind == BackendKind::Emulator || env_override;
        let border = cx.theme().border;
        let panel = ConnectionPanel::new("youtube-connection", self.service.clone()).manage(true);
        let section = v_flex().id("youtube-settings").w_full().gap_6();
        match connection {
            // Signed in: the account first. Tokens belong to one client and backend, so
            // changing either waits for a disconnect.
            Connection::Connected(_) => section
                .child(panel)
                .child(help_text("Disconnect to change the OAuth client or how VideoCRM connects to YouTube.", cx))
                .when(emulator, |this| this.child(div().h_px().bg(border)).child(self.render_developer(true, cx))),
            // Not yet: fill in the client, connect, then the status. What Google will show
            // matters once Connect can be clicked.
            connection => {
                let connecting = matches!(connection, Connection::Connecting { .. });
                let ready = service.config().is_ok();
                section
                    .map(|this| match (self.kind, env_override) {
                        // The environment decides the addresses; nothing to fill in.
                        (_, true) => this,
                        (BackendKind::Google, false) => {
                            this.child(self.render_google(connecting, cx).into_any_element())
                        }
                        (BackendKind::Emulator, false) => {
                            this.child(self.render_emulator(connecting, cx).into_any_element())
                        }
                    })
                    .child(
                        v_flex()
                            .gap_4()
                            .child(panel)
                            .when(connecting, |this| {
                                this.child(help_text("Cancel the sign-in to change these settings.", cx))
                            })
                            .when(!emulator && !connecting && ready, |this| this.child(self.render_expectations(cx))),
                    )
                    .child(div().h_px().bg(border))
                    .child(self.render_developer(connecting, cx))
            }
        }
    }
}
