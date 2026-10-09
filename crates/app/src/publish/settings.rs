//! The YouTube section of Settings and of onboarding: the account connection, then
//! which backend VideoCRM talks to and its OAuth client or emulator addresses.

use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::link::Link;
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Rems,
    Render, SharedString, Styled as _, Subscription, Task, Window, div,
};
use videocrm_core::settings::{DEFAULT_EMULATOR_GOOGLE_URL, DEFAULT_EMULATOR_YOUTUBE_URL, YoutubeBackend};

use super::service::{Connection, YoutubeService};
use super::ui::{ConnectionPanel, FieldRow, help_text, status_line};
use crate::state::{AppEvent, AppState};

/// Typing in a field is saved this long after the last keystroke.
const SAVE_DELAY: Duration = Duration::from_millis(500);
const CHECK_TIMEOUT: Duration = Duration::from_secs(3);
/// Fields stop growing here: an ID or a URL does not need the whole page width.
const FIELD_MAX_WIDTH: Rems = Rems(36.);
const CLOUD_CONSOLE_URL: &str = "https://console.cloud.google.com/";
const GOOGLE_STEPS: [&str; 5] = [
    "In the Google Cloud console, create a project.",
    "Enable the YouTube Data API v3 for it.",
    "Set up the OAuth consent screen with an External audience, and add your Google account as a test user.",
    "Create an OAuth client ID of type Desktop app.",
    "Paste its client ID and client secret here, then connect.",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackendKind {
    Google,
    Emulator,
}

/// What "Check" found out about the emulators.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmulatorCheck {
    Checking,
    Running,
    Unreachable(SharedString),
}

/// YouTube section used by the Settings page and the onboarding YouTube step:
/// backend choice, OAuth client fields, connect / disconnect.
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
            // Leaving the page must not lose the last edits.
            cx.on_release(|this, cx| this.save_pending(cx)),
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

    fn render_backend_choice(&self, locked: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let option = |id: &'static str, label: &'static str, description: &'static str| {
            Radio::new(id).label(label).child(help_text(description, cx))
        };
        RadioGroup::vertical("youtube-backend")
            .child(option(
                "youtube-backend-google",
                "YouTube account (Google)",
                "Upload to your channel through your own Google OAuth client.",
            ))
            .child(option(
                "youtube-backend-emulator",
                "Local emulator (testing)",
                "Try connecting and uploading without a Google account. Nothing leaves this computer.",
            ))
            .selected_index(Some(match self.kind {
                BackendKind::Google => 0,
                BackendKind::Emulator => 1,
            }))
            .disabled(locked)
            .on_change(cx.listener(|this, ix: &usize, _, cx| {
                this.set_kind(if *ix == 0 { BackendKind::Google } else { BackendKind::Emulator }, cx);
            }))
    }

    fn render_google(&self, locked: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let steps = GOOGLE_STEPS.iter().enumerate().map(|(ix, step)| {
            h_flex()
                .gap_2()
                .items_start()
                .text_sm()
                .child(div().w_4().flex_shrink_0().text_color(theme.muted_foreground).child(format!("{}.", ix + 1)))
                .child(div().flex_1().min_w_0().child(*step))
        });
        v_flex()
            .max_w(FIELD_MAX_WIDTH)
            .gap_4()
            .child(
                v_flex()
                    .gap_2()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Set up your OAuth client"))
                    .child(v_flex().gap_1().children(steps))
                    .child(
                        div().text_sm().child(
                            Link::new("google-cloud-console").href(CLOUD_CONSOLE_URL).child(
                                h_flex()
                                    .gap_1()
                                    .child("Open the Google Cloud console")
                                    .child(Icon::new(IconName::ExternalLink).xsmall()),
                            ),
                        ),
                    ),
            )
            .child(
                FieldRow::new("Client ID")
                    .control(Input::new(&self.client_id).id("youtube-client-id").disabled(locked)),
            )
            .child(
                FieldRow::new("Client secret")
                    .help("Until Google audits your project, YouTube locks the videos it uploads to private.")
                    .control(
                        Input::new(&self.client_secret).id("youtube-client-secret").mask_toggle().disabled(locked),
                    ),
            )
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
                FieldRow::new("Google emulator")
                    .control(Input::new(&self.google_url).id("youtube-google-url").disabled(locked)),
            )
            .child(
                FieldRow::new("YouTube emulator")
                    .help("Start both with scripts/emulators.sh. They keep accounts and uploads in memory until they stop.")
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
        // Tokens belong to one backend: changing it would sign out, so it waits for a disconnect.
        let locked = matches!(service.connection(), Connection::Connected(_) | Connection::Connecting { .. });
        let override_target = service
            .is_env_override()
            .then(|| service.config().map_or_else(|_| "the emulators".to_owned(), |config| config.backend_label()));
        let border = cx.theme().border;
        v_flex()
            .id("youtube-settings")
            .w_full()
            .gap_6()
            .child(ConnectionPanel::new("youtube-connection", self.service.clone()).manage(true))
            .child(div().h_px().bg(border))
            .child(
                v_flex()
                    .gap_4()
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Connect with"))
                    .child(self.render_backend_choice(locked, cx))
                    .when(locked, |this| {
                        this.child(help_text("Disconnect the account to change how VideoCRM connects to YouTube.", cx))
                    })
                    .when_some(override_target, |this, target| {
                        this.child(help_text(
                            format!(
                                "VIDEOCRM_GOOGLE_URL or VIDEOCRM_YOUTUBE_URL is set, so VideoCRM connects to {target} whatever you choose here."
                            ),
                            cx,
                        ))
                    }),
            )
            .map(|this| match self.kind {
                BackendKind::Google => this.child(self.render_google(locked, cx).into_any_element()),
                BackendKind::Emulator => this.child(self.render_emulator(locked, cx).into_any_element()),
            })
    }
}

/// `Running`, or which emulator did not answer.
fn probe_emulators(google_url: &str, youtube_url: &str) -> EmulatorCheck {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(CHECK_TIMEOUT)).build().into();
    let answers = |url: String| agent.get(&url).call().is_ok();
    let google = answers(format!("{google_url}/.well-known/openid-configuration"));
    let youtube = answers(format!("{youtube_url}/_emulator/uploads"));
    match (google, youtube) {
        (true, true) => EmulatorCheck::Running,
        (false, true) => {
            EmulatorCheck::Unreachable(format!("The Google emulator isn't answering at {google_url}.").into())
        }
        (true, false) => {
            EmulatorCheck::Unreachable(format!("The YouTube emulator isn't answering at {youtube_url}.").into())
        }
        (false, false) => {
            EmulatorCheck::Unreachable("Neither emulator is answering. Start them with scripts/emulators.sh.".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probing_reports_which_emulator_is_down() {
        // Nothing listens on port 9 (discard) of the loopback interface in a test run.
        let check = probe_emulators("http://127.0.0.1:9", "http://127.0.0.1:9");
        assert_eq!(
            check,
            EmulatorCheck::Unreachable("Neither emulator is answering. Start them with scripts/emulators.sh.".into())
        );
    }
}
