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
    AnyElement, App, AppContext as _, Context, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, Window, div,
};
use videocrm_core::settings::{DEFAULT_EMULATOR_GOOGLE_URL, DEFAULT_EMULATOR_YOUTUBE_URL, YoutubeBackend};

use super::service::{EmulatorCheck, YoutubeAccount, YoutubeService, probe_emulators};
use super::ui::{
    AccountSummary, ConnectionPanel, confirm_disconnect, help_text, needs_sign_in_line, reconnect_button,
    sign_in_progress, status_line,
};
use crate::state::{AppEvent, AppState};
use crate::ui::settings::{FIELD_MAX_WIDTH, SettingsField, SettingsSection};

/// Typing in a field is saved this long after the last keystroke.
const SAVE_DELAY: Duration = Duration::from_millis(500);

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
    "Google warns that it hasn’t verified the app. That’s expected for your own client: click Continue.",
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

    /// The Google Cloud setup steps, folded behind a toggle.
    fn render_setup_steps(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
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
        Collapsible::new()
            .open(open)
            .child(h_flex().child(toggle))
            .content(v_flex().id("google-setup-steps").max_w(FIELD_MAX_WIDTH).gap_2().pt_2().children(steps))
    }

    /// What Google will show, right below Connect.
    fn render_expectations(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .id("youtube-expectations")
            .max_w(FIELD_MAX_WIDTH)
            .gap_1p5()
            // Labelled like a field: it sits in the account's group.
            .child(div().text_sm().child("When you connect"))
            .children(GOOGLE_EXPECTATIONS.iter().map(|note| {
                h_flex()
                    .gap_2()
                    .items_start()
                    // A plain dot: an outlined circle reads as a radio button.
                    .child(div().mt_2p5().size_1().flex_shrink_0().rounded_full().bg(muted))
                    .child(div().flex_1().min_w_0().text_sm().text_color(muted).child(*note))
            }))
            // Tests check that the notes are on screen.
            .test_support()
    }

    /// The signed-in accounts, the sign-in under way, and adding another account.
    fn render_account(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let service = self.service.read(cx);
        if service.accounts().is_empty() {
            let note = service.is_signing_in().then_some("Cancel the sign-in to change these settings.");
            return v_flex()
                .w_full()
                .gap_3()
                .child(ConnectionPanel::new("youtube-connection", self.service.clone()).manage(true))
                .children(note.map(|note| help_text(note, cx)));
        }
        let theme = cx.theme();
        let default = service.default_account().map(|account| account.key().to_owned());
        let rows: Vec<AnyElement> = service
            .accounts()
            .iter()
            .enumerate()
            .map(|(ix, account)| {
                account_row(&self.service, account, default.as_deref() == Some(account.key()), ix == 0, cx)
                    .into_any_element()
            })
            .collect();
        // A sign-in of an account the list doesn't show yet: it adds one.
        let adding =
            service.is_signing_in() && service.reconnecting().is_none_or(|key| service.find_account(key).is_none());
        let problem = service.sign_in_problem().filter(|_| !service.is_signing_in()).cloned();
        let add = {
            let service = self.service.clone();
            Button::new("add-account")
                .label("Add account")
                .icon(IconName::Plus)
                .small()
                .outline()
                .disabled(self.service.read(cx).is_signing_in() || self.service.read(cx).config().is_err())
                .on_click(move |_, _, cx| service.update(cx, |service, cx| service.connect(cx)))
        };
        v_flex()
            .w_full()
            .gap_4()
            .child(
                v_flex()
                    .id("youtube-accounts")
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .children(rows)
                    // Tests find the rows here.
                    .test_support(),
            )
            .when(adding, |this| {
                this.child(div().id("youtube-adding-account").child(sign_in_progress(
                    &self.service,
                    self.service.read(cx).sign_in_url().cloned(),
                    cx,
                )))
            })
            .when_some(problem, |this, problem| {
                this.child(status_line(IconName::CircleAlert, theme.danger, "Couldn’t connect", Some(problem), cx))
            })
            .child(h_flex().child(add))
            .child(help_text(
                "Videos upload to the default account unless their Publish tab chooses another. Disconnect every \
                 account to change the OAuth client or how VideoCRM connects to YouTube.",
                cx,
            ))
    }

    /// The developer switch: the local emulators instead of YouTube.
    fn render_emulator_switch(&self, locked: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let env_override = self.service.read(cx).is_env_override();
        Switch::new("youtube-use-emulator")
            .checked(self.kind == BackendKind::Emulator || env_override)
            .disabled(locked || env_override)
            .on_change(cx.listener(|this, on: &bool, _, cx| {
                this.set_kind(if *on { BackendKind::Emulator } else { BackendKind::Google }, cx)
            }))
    }

    fn render_emulator_check(&self, locked: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
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
        h_flex()
            .gap_3()
            .items_start()
            .child(
                Button::new("check-emulators")
                    .label("Check")
                    .small()
                    .outline()
                    .loading(checking)
                    .disabled(checking || locked)
                    .on_click(cx.listener(|this, _, _, cx| this.check_emulators(cx))),
            )
            .children(result.map(|result| div().pt_0p5().min_w_0().child(result)))
    }

    /// The settings as the Settings page groups them, for the connection as it is now.
    pub fn sections(&self, cx: &App) -> Vec<SettingsSection<Self>> {
        let service = self.service.read(cx);
        let env_override = service.is_env_override();
        let emulator = self.kind == BackendKind::Emulator || env_override;
        // Accounts asking for a new sign-in hold tokens of this client too.
        let connected = !service.accounts().is_empty();
        let connecting = service.is_signing_in();
        let ready = service.config().is_ok();
        let locked = connected || connecting;
        let mut sections = Vec::new();
        // Tokens belong to one client and backend: changing either waits for a disconnect.
        match (self.kind, env_override, connected) {
            // The environment decides the addresses; nothing to fill in.
            (_, true, _) | (_, _, true) => {}
            (BackendKind::Google, false, false) => sections.push(
                SettingsSection::new()
                    .title("OAuth client")
                    .keywords(["google", "cloud", "credentials"])
                    .description(
                        "VideoCRM uploads through an OAuth client in your own Google Cloud project. Creating one is \
                         free.",
                    )
                    .element(|this: &mut Self, _, cx| this.render_setup_steps(cx))
                    .field(
                        SettingsField::new("Client ID", move |this: &mut Self, _, _| {
                            Input::new(&this.client_id).id("youtube-client-id").disabled(locked)
                        })
                        .stacked(),
                    )
                    .field(
                        SettingsField::new("Client secret", move |this: &mut Self, _, _| {
                            Input::new(&this.client_secret).id("youtube-client-secret").mask_toggle().disabled(locked)
                        })
                        .stacked(),
                    ),
            ),
            (BackendKind::Emulator, false, false) => sections.push(
                SettingsSection::new()
                    .title("Local emulators")
                    .description("Empty fields use the default addresses.")
                    .field(
                        SettingsField::new("Google emulator", move |this: &mut Self, _, _| {
                            Input::new(&this.google_url).id("youtube-google-url").disabled(locked)
                        })
                        .stacked(),
                    )
                    .field(
                        SettingsField::new("YouTube emulator", move |this: &mut Self, _, _| {
                            Input::new(&this.youtube_url).id("youtube-youtube-url").disabled(locked)
                        })
                        .stacked(),
                    )
                    .element(move |this: &mut Self, _, cx| this.render_emulator_check(locked, cx)),
            ),
        }
        let mut account = SettingsSection::new()
            .title("Accounts")
            .keywords(["account", "connect", "sign in", "google", "youtube", "channel", "upload", "default"])
            .element(|this: &mut Self, _, cx| this.render_account(cx));
        // In the account's group rather than a group of its own: a group that
        // comes and goes as the client ID is typed would scroll the Settings
        // page back to the top.
        if !emulator && !locked && ready {
            account = account.element(|this: &mut Self, _, cx| this.render_expectations(cx));
        }
        sections.push(account);
        if !connected || emulator {
            let mut developer = SettingsSection::new().title("Developer").field(
                SettingsField::new("Local emulators", move |this: &mut Self, _, cx| {
                    this.render_emulator_switch(locked, cx)
                })
                .description(
                    "Connect to the local emulators instead of YouTube, for development and demos without a Google \
                     account. Start them with scripts/emulators.sh; they keep accounts and uploads in memory until \
                     they stop.",
                ),
            );
            if env_override {
                let target =
                    service.config().map_or_else(|_| "the emulators".to_owned(), |config| config.backend_label());
                developer = developer.element(move |_: &mut Self, _, cx| {
                    help_text(
                        format!(
                            "VIDEOCRM_GOOGLE_URL or VIDEOCRM_YOUTUBE_URL is set, so VideoCRM connects to {target} \
                             whatever this says."
                        ),
                        cx,
                    )
                });
            }
            sections.push(developer);
        }
        sections
    }
}

impl Render for YoutubeSettings {
    /// Outside the Settings page (onboarding) the sections stack, and the
    /// developer option stays folded behind a toggle.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut sections = self.sections(cx);
        let developer = sections.pop_if(|section| section.has_title("Developer"));
        let shown: Vec<AnyElement> = sections.iter().map(|section| section.render(self, window, cx)).collect();
        let developer = developer.map(|section| {
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
            let content = div().pt_3().child(section.render(self, window, cx));
            Collapsible::new().open(open).child(h_flex().child(toggle)).content(content)
        });
        v_flex().id("youtube-settings").w_full().gap_8().children(shown).children(
            developer.map(|developer| div().pt_4().border_t_1().border_color(cx.theme().border).child(developer)),
        )
    }
}

/// One signed-in account: who it is, whether it is the default, and what can be
/// done with it. A rejected sign-in shows the way back under it.
fn account_row(
    service: &Entity<YoutubeService>,
    account: &YoutubeAccount,
    is_default: bool,
    first: bool,
    cx: &App,
) -> impl IntoElement + use<> {
    let youtube = service.read(cx);
    let key = account.key().to_owned();
    let reconnecting = youtube.reconnecting() == Some(key.as_str());
    let make_default = {
        let (service, key) = (service.clone(), key.clone());
        Button::new("make-default")
            .label("Make default")
            .small()
            .ghost()
            .on_click(move |_, _, cx| service.update(cx, |service, cx| service.set_default(&key, cx)))
    };
    let disconnect = {
        let (service, key) = (service.clone(), key.clone());
        Button::new("disconnect")
            .label("Disconnect…")
            .small()
            .outline()
            .on_click(move |_, window, cx| confirm_disconnect(service.clone(), key.clone(), window, cx))
    };
    v_flex()
        .id(ElementId::Name(format!("youtube-account-{key}").into()))
        .gap_3()
        .p_3()
        // The list's border closes the first row; a hairline separates the others.
        .when(!first, |row| row.border_t_1().border_color(cx.theme().border))
        .child(
            h_flex()
                .gap_3()
                .items_start()
                .child(
                    div().flex_1().min_w_0().child(
                        AccountSummary::new(account.account().clone(), account.avatar().map(ToOwned::to_owned))
                            .tag(is_default.then_some("Default")),
                    ),
                )
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .gap_2()
                        // Without a channel there is nowhere to upload to.
                        .when(!is_default && account.has_channel(), |this| this.child(make_default))
                        .child(disconnect),
                ),
        )
        .map(|this| {
            if reconnecting {
                this.child(sign_in_progress(service, youtube.sign_in_url().cloned(), cx))
            } else if !account.is_connected() {
                let reconnect = reconnect_button(service, account.account()).disabled(youtube.is_signing_in());
                this.child(needs_sign_in_line(account.account(), youtube, cx)).child(h_flex().child(reconnect))
            } else {
                this
            }
        })
        // Tests find an account's buttons through its row.
        .test_support()
}
