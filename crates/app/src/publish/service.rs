//! [`YoutubeService`]: the app-wide YouTube connection. Settings decide the backend,
//! a browser sign-in produces credentials, and the credentials file keeps them (with
//! the account) across restarts. Uploads live in `upload.rs`.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, AppContext as _, Context, Entity, EventEmitter, Global, SharedString, Subscription};
use videocrm_core::settings::{self, YoutubeBackend};
use videocrm_core::{UploadId, UploadJob, UploadStatus, Video};
use videocrm_youtube::{
    Account, CredentialStore, Credentials, Error as YoutubeError, SavedLogin, VideoInfo, YoutubeClient, YoutubeConfig,
    begin_authorization, check_client,
};

use super::rules::AccountAccess;
use super::upload::{ActiveUpload, ProcessingOutcome, Retired};
use crate::state::{AppEvent, AppState, Problem};

const CREDENTIALS_FILE: &str = "credentials.json";
const AVATAR_DIR: &str = "avatars";
/// A browser sign-in left open longer than this is abandoned.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// How long "Check" and the check before an emulator sign-in wait for an answer.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// Every Google OAuth client ID ends like this.
const CLIENT_ID_SUFFIX: &str = ".apps.googleusercontent.com";
const UPLOAD_SCOPE: &str = "https://www.googleapis.com/auth/youtube.upload";
const AVATAR_TIMEOUT: Duration = Duration::from_secs(15);

/// Where the YouTube account stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Connection {
    Disconnected,
    /// Waiting for the browser sign-in. `authorize_url` is the consent page once it exists,
    /// for machines where no browser opens.
    Connecting {
        authorize_url: Option<SharedString>,
    },
    Connected(Account),
    /// YouTube no longer accepts the saved sign-in of this account (expired, revoked, or a
    /// permission unticked): connecting again keeps uploading as it.
    NeedsSignIn(Account),
    /// The last sign-in failed.
    Failed(SharedString),
}

/// What the emulator "Check" found out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmulatorCheck {
    Checking,
    Running,
    Unreachable(SharedString),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum YoutubeEvent {
    ConnectionChanged,
    /// An upload job was created or changed status.
    UploadsChanged,
}

/// App-wide YouTube connection: settings -> client, OAuth sign-in, credentials, uploads.
pub struct YoutubeService {
    pub(super) state: Entity<AppState>,
    data_dir: PathBuf,
    credentials: CredentialStore,
    config: Result<YoutubeConfig, SharedString>,
    /// The config comes from `VIDEOCRM_GOOGLE_URL` / `VIDEOCRM_YOUTUBE_URL`, not settings.
    env_override: bool,
    connection: Connection,
    pub(super) client: Option<Arc<YoutubeClient>>,
    /// Where `client` saves refreshed tokens.
    tokens: Option<TokenSink>,
    /// Cancel flag of the sign-in in progress.
    sign_in: Option<Arc<AtomicBool>>,
    sign_in_attempt: u64,
    avatar: Option<PathBuf>,
    /// Every upload job, newest first, as stored.
    pub(super) jobs: Vec<UploadJob>,
    pub(super) active: HashMap<UploadId, ActiveUpload>,
    pub(super) outcomes: HashMap<UploadId, ProcessingOutcome>,
    /// Why the custom thumbnail of a finished upload wasn't set, for this run of the app.
    pub(super) thumbnail_problems: HashMap<UploadId, String>,
    /// Why stopped uploads stopped offering Resume, for this run of the app.
    pub(super) retired: HashMap<UploadId, Retired>,
    /// A background check of the files of stopped uploads is running.
    pub(super) checking_files: bool,
    /// What YouTube last said about uploaded videos, by YouTube video id.
    pub(super) remote: HashMap<String, VideoInfo>,
    /// Uploaded videos YouTube says it doesn't have (deleted in YouTube Studio, or
    /// removed by YouTube), by YouTube video id.
    gone: HashSet<String>,
    remote_requests: HashSet<String>,
    _subscription: Subscription,
}

struct GlobalYoutubeService(Entity<YoutubeService>);

impl Global for GlobalYoutubeService {}

impl EventEmitter<YoutubeEvent> for YoutubeService {}

enum SignInStep {
    Opened(String),
    Finished(Box<Result<SignedIn, SignInError>>),
}

/// Why a sign-in ended without an account.
enum SignInError {
    Cancelled,
    /// What to tell the creator.
    Failed(String),
}

impl SignInError {
    fn new(error: YoutubeError, config: &YoutubeConfig) -> Self {
        match error {
            YoutubeError::Cancelled { .. } => SignInError::Cancelled,
            error => SignInError::Failed(sign_in_message(&error, config)),
        }
    }
}

/// A finished browser sign-in, not saved yet: the service saves it only if it still
/// wants it, so a cancelled sign-in leaves nothing behind.
struct SignedIn {
    client: Arc<YoutubeClient>,
    tokens: TokenSink,
    account: Account,
}

/// Saves a client's refreshed tokens to the credentials file while its sign-in lasts.
/// A request still running after a sign-out must not write the tokens back.
#[derive(Clone)]
struct TokenSink {
    store: CredentialStore,
    signed_in: Arc<Mutex<bool>>,
}

impl TokenSink {
    fn new(store: CredentialStore, signed_in: bool) -> Self {
        Self { store, signed_in: Arc::new(Mutex::new(signed_in)) }
    }

    fn signed_in(&self) -> MutexGuard<'_, bool> {
        self.signed_in.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts the sign-in by saving `login`.
    fn sign_in(&self, login: &SavedLogin) -> Result<(), YoutubeError> {
        let mut signed_in = self.signed_in();
        self.store.save(login)?;
        *signed_in = true;
        Ok(())
    }

    /// Replaces the saved login, unless the sign-in has ended.
    fn update(&self, login: &SavedLogin) -> Result<(), YoutubeError> {
        let signed_in = self.signed_in();
        if *signed_in { self.store.save(login) } else { Ok(()) }
    }

    fn save_credentials(&self, credentials: &Credentials) {
        let signed_in = self.signed_in();
        if *signed_in {
            // A lost save only means one more token refresh after the next start.
            let _ = self.store.save_credentials(credentials);
        }
    }

    /// Ends the sign-in and removes the saved login.
    fn sign_out(&self) -> Result<(), YoutubeError> {
        let mut signed_in = self.signed_in();
        *signed_in = false;
        self.store.clear()
    }
}

impl YoutubeService {
    /// Creates the service and registers it globally. Call once at startup, after
    /// `AppState::set_global`.
    pub fn init(state: Entity<AppState>, cx: &mut App) {
        Self::init_in(state, settings::data_dir(), cx);
    }

    /// [`YoutubeService::init`] with the credentials file and cached avatars in `data_dir`.
    pub fn init_in(state: Entity<AppState>, data_dir: PathBuf, cx: &mut App) -> Entity<YoutubeService> {
        let service = cx.new(|cx| Self::new(state, data_dir, cx));
        cx.set_global(GlobalYoutubeService(service.clone()));
        service
    }

    pub fn global(cx: &App) -> Entity<YoutubeService> {
        cx.global::<GlobalYoutubeService>().0.clone()
    }

    /// The service, when the app has one (views tested on their own may not).
    pub fn try_global(cx: &App) -> Option<Entity<YoutubeService>> {
        cx.try_global::<GlobalYoutubeService>().map(|global| global.0.clone())
    }

    fn new(state: Entity<AppState>, data_dir: PathBuf, cx: &mut Context<Self>) -> Self {
        let (config, env_override) = backend_config(&state.read(cx).settings().youtube);
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| match event {
            AppEvent::SettingsChanged => this.on_settings_changed(cx),
            AppEvent::DataChanged => {
                this.forget_deleted_videos(cx);
                this.check_stopped_uploads(cx);
            }
            _ => {}
        });
        let mut service = Self {
            state,
            credentials: CredentialStore::new(data_dir.join(CREDENTIALS_FILE)),
            data_dir,
            config,
            env_override,
            connection: Connection::Disconnected,
            client: None,
            tokens: None,
            sign_in: None,
            sign_in_attempt: 0,
            avatar: None,
            jobs: Vec::new(),
            active: HashMap::new(),
            outcomes: HashMap::new(),
            thumbnail_problems: HashMap::new(),
            retired: HashMap::new(),
            checking_files: false,
            remote: HashMap::new(),
            gone: HashSet::new(),
            remote_requests: HashSet::new(),
            _subscription: subscription,
        };
        service.restore_login(cx);
        service.recover_interrupted_uploads(cx);
        service.check_stopped_uploads(cx);
        service
    }

    pub fn state(&self) -> &Entity<AppState> {
        &self.state
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn account(&self) -> Option<&Account> {
        match &self.connection {
            Connection::Connected(account) => Some(account),
            _ => None,
        }
    }

    pub fn is_connected(&self) -> bool {
        self.client.is_some() && self.account().is_some()
    }

    /// Whether uploads can use the account now, or what they need first.
    pub fn account_access(&self) -> AccountAccess {
        match &self.connection {
            _ if self.is_connected() => AccountAccess::Connected,
            Connection::NeedsSignIn(_) => AccountAccess::NeedsSignIn,
            _ => AccountAccess::NotConnected,
        }
    }

    /// The backend the settings describe, or why they do not describe one yet.
    pub fn config(&self) -> Result<&YoutubeConfig, &SharedString> {
        self.config.as_ref()
    }

    /// Whether `VIDEOCRM_GOOGLE_URL` / `VIDEOCRM_YOUTUBE_URL` override the settings.
    pub fn is_env_override(&self) -> bool {
        self.env_override
    }

    /// The channel picture (or account picture), once downloaded.
    pub fn avatar(&self) -> Option<&Path> {
        self.avatar.as_deref()
    }

    /// Client for the connected account. Refreshed tokens are saved to the credentials file.
    pub fn client(&self) -> Option<Arc<YoutubeClient>> {
        self.client.clone()
    }

    /// Opens the consent page in the browser and waits for the sign-in in the background.
    pub fn connect(&mut self, cx: &mut Context<Self>) {
        if matches!(self.connection, Connection::Connecting { .. } | Connection::Connected(_)) {
            return;
        }
        let config = match &self.config {
            Ok(config) => config.clone(),
            Err(problem) => {
                let problem = problem.clone();
                self.set_connection(Connection::Failed(problem), cx);
                return;
            }
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.sign_in = Some(cancel.clone());
        self.sign_in_attempt += 1;
        let attempt = self.sign_in_attempt;
        self.set_connection(Connection::Connecting { authorize_url: None }, cx);

        let tokens = TokenSink::new(self.credentials.clone(), false);
        let (sender, mut steps) = mpsc::unbounded();
        let spawned = thread::Builder::new()
            .name("youtube-sign-in".into())
            .spawn(move || sign_in(config, tokens, &cancel, &sender));
        if let Err(error) = spawned {
            log_line!("videocrm: couldn't start the sign-in thread: {error}");
            self.sign_in = None;
            let message = "Couldn’t start the sign-in. Quit VideoCRM and open it again, then connect.";
            self.set_connection(Connection::Failed(message.into()), cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            while let Some(step) = steps.next().await {
                if this.update(cx, |this, cx| this.on_sign_in_step(attempt, step, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Stops waiting for the browser sign-in.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.cancel_sign_in();
        if matches!(self.connection, Connection::Connecting { .. }) {
            self.set_connection(Connection::Disconnected, cx);
        }
    }

    /// Signs out: forgets the tokens and revokes them with Google, best effort.
    /// Uploads in progress stop and can be resumed after connecting again.
    pub fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.cancel_sign_in();
        for upload in self.active.values() {
            upload.cancel();
        }
        let cleared = match self.tokens.take() {
            Some(tokens) => tokens.sign_out(),
            None => self.credentials.clear(),
        };
        if let Err(error) = cleared {
            report_problem(&self.state, credentials_problem("Couldn’t remove the saved YouTube sign-in", &error), cx);
        }
        if let Some(client) = self.client.take() {
            // The local tokens are gone either way; revoking also ends the grant at Google.
            cx.background_spawn(async move { client.revoke() }).detach();
        }
        self.avatar = None;
        self.set_connection(Connection::Disconnected, cx);
    }

    fn cancel_sign_in(&mut self) {
        if let Some(cancel) = self.sign_in.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    fn set_connection(&mut self, connection: Connection, cx: &mut Context<Self>) {
        self.connection = connection;
        cx.emit(YoutubeEvent::ConnectionChanged);
        cx.notify();
    }

    fn on_sign_in_step(&mut self, attempt: u64, step: SignInStep, cx: &mut Context<Self>) {
        // A cancelled or superseded sign-in reports nothing.
        if attempt != self.sign_in_attempt || self.sign_in.is_none() {
            return;
        }
        match step {
            SignInStep::Opened(url) => {
                cx.open_url(&url);
                self.set_connection(Connection::Connecting { authorize_url: Some(url.into()) }, cx);
            }
            SignInStep::Finished(result) => {
                self.sign_in = None;
                let signed_in = (*result).and_then(|signed_in| {
                    let login = SavedLogin {
                        credentials: signed_in.client.credentials(),
                        account: Some(signed_in.account.clone()),
                    };
                    match signed_in.tokens.sign_in(&login) {
                        Ok(()) => Ok(signed_in),
                        Err(error) => {
                            log_line!("videocrm: couldn't save the YouTube sign-in: {error}");
                            Err(SignInError::Failed(
                                "Couldn’t save the YouTube sign-in: VideoCRM can’t write credentials.json in its \
                                 data folder. Check that the folder is writable, then connect again."
                                    .to_owned(),
                            ))
                        }
                    }
                });
                match signed_in {
                    Ok(SignedIn { client, tokens, account }) => {
                        self.client = Some(client);
                        self.tokens = Some(tokens);
                        self.set_connection(Connection::Connected(account), cx);
                        self.refresh_avatar(cx);
                    }
                    Err(SignInError::Cancelled) => self.set_connection(Connection::Disconnected, cx),
                    Err(SignInError::Failed(message)) => self.set_connection(Connection::Failed(message.into()), cx),
                }
            }
        }
    }

    /// Asks for a new sign-in after YouTube rejected the stored tokens. The account stays
    /// on screen, so the creator knows whom to sign in as.
    pub(super) fn require_sign_in(&mut self, cx: &mut Context<Self>) {
        self.client = None;
        self.tokens = None;
        let connection = match self.connection.clone() {
            Connection::Connected(account) | Connection::NeedsSignIn(account) => Connection::NeedsSignIn(account),
            _ => Connection::Failed("YouTube needs you to connect the account again.".into()),
        };
        self.set_connection(connection, cx);
    }

    fn restore_login(&mut self, cx: &mut Context<Self>) {
        let Ok(config) = &self.config else { return };
        match self.credentials.load() {
            Ok(Some(SavedLogin { credentials, account: Some(account) }))
                if account.backend == config.backend_label() =>
            {
                let tokens = TokenSink::new(self.credentials.clone(), true);
                self.client = Some(new_client(config.clone(), credentials, tokens.clone()));
                self.tokens = Some(tokens);
                self.connection = Connection::Connected(account);
                self.refresh_avatar(cx);
                self.refresh_account(cx);
            }
            // Tokens of another backend, or no account: not usable here.
            Ok(_) => {}
            Err(error) => {
                log_line!("videocrm: couldn't read the saved YouTube sign-in: {error}");
                self.connection =
                    Connection::Failed("Couldn’t read the saved YouTube sign-in. Connect again to sign in.".into());
            }
        }
    }

    fn on_settings_changed(&mut self, cx: &mut Context<Self>) {
        let (config, env_override) = backend_config(&self.state.read(cx).settings().youtube);
        if config == self.config {
            return;
        }
        self.config = config;
        self.env_override = env_override;
        // Tokens belong to the backend that issued them.
        let signed_in = self.client.is_some() || !matches!(self.connection, Connection::Disconnected);
        if signed_in {
            self.disconnect(cx);
        } else {
            cx.emit(YoutubeEvent::ConnectionChanged);
            cx.notify();
        }
    }

    /// Reads the account and channel again (name, picture, subscriber and video counts)
    /// and saves them. Offline, the saved account stays; rejected tokens ask for a new
    /// sign-in right away instead of at the next upload.
    pub(super) fn refresh_account(&mut self, cx: &mut Context<Self>) {
        let (Some(client), Some(tokens), Ok(config)) = (self.client.clone(), self.tokens.clone(), self.config.clone())
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let reader = client.clone();
            let account = cx.background_spawn(async move { read_account(&reader, &config) }).await;
            this.update(cx, |this, cx| {
                // A sign-out or another sign-in since then makes this answer stale.
                if !this.client.as_ref().is_some_and(|current| Arc::ptr_eq(current, &client)) {
                    return;
                }
                match account {
                    Ok(account) => {
                        let login = SavedLogin { credentials: client.credentials(), account: Some(account.clone()) };
                        if let Err(error) = tokens.update(&login) {
                            report_problem(
                                &this.state,
                                credentials_problem("Couldn’t save the YouTube account", &error),
                                cx,
                            );
                        }
                        if this.account() != Some(&account) {
                            this.set_connection(Connection::Connected(account), cx);
                            this.refresh_avatar(cx);
                        }
                    }
                    Err(error) if error.needs_sign_in() => this.require_sign_in(cx),
                    Err(_) => {}
                }
            })
            .ok();
        })
        .detach();
    }

    /// Downloads the channel picture into the data directory, once per picture URL.
    fn refresh_avatar(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.account() else { return };
        let Some(url) = account.channel_thumbnail.clone().or_else(|| account.picture.clone()) else {
            self.avatar = None;
            return;
        };
        let path = self.data_dir.join(AVATAR_DIR).join(avatar_file_name(&url));
        cx.spawn(async move |this, cx| {
            let fetched = cx.background_spawn(async move { download_avatar(&url, &path).map(|()| path) }).await;
            // Without a picture the account shows its initials.
            if let Ok(path) = fetched {
                this.update(cx, |this, cx| {
                    if this.account().is_some() {
                        this.avatar = Some(path);
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// What YouTube last said about an uploaded video (visibility, schedule, processing).
    pub fn youtube_video(&self, youtube_id: &str) -> Option<&VideoInfo> {
        self.remote.get(youtube_id)
    }

    /// Whether the video a finished upload put on YouTube is no longer there.
    pub fn is_off_youtube(&self, job: &UploadJob, cx: &App) -> bool {
        let (UploadStatus::Processing { video_id: youtube_id } | UploadStatus::Done { video_id: youtube_id }) =
            &job.status
        else {
            return false;
        };
        // Once YouTube said so, the video's link is cleared, also for later runs.
        self.gone.contains(youtube_id)
            || (self.latest_job(job.video_id).is_some_and(|latest| latest.id == job.id)
                && self.state.read(cx).video(job.video_id).is_some_and(|video| video.youtube.is_none()))
    }

    /// Asks YouTube about an uploaded video in the background (one quota unit), so the
    /// Publish tab shows what YouTube has rather than what was asked for.
    pub fn refresh_youtube_video(&mut self, youtube_id: &str, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        if !self.remote_requests.insert(youtube_id.to_owned()) {
            return;
        }
        let youtube_id = youtube_id.to_owned();
        cx.spawn(async move |this, cx| {
            let lookup = youtube_id.clone();
            let info = cx.background_spawn(async move { client.get_video(&lookup) }).await;
            this.update(cx, |this, cx| {
                this.remote_requests.remove(&youtube_id);
                match info {
                    Ok(Some(info)) => {
                        this.remote.insert(youtube_id, info);
                        cx.notify();
                    }
                    Ok(None) => this.on_video_gone(&youtube_id, cx),
                    // Offline, the stored link is what the tab shows.
                    Err(_) => {}
                }
            })
            .ok();
        })
        .detach();
    }

    /// YouTube has no video `youtube_id`. Only its owner sees a private video, so
    /// that counts only when the connected channel is the one it was uploaded to.
    /// The videos linking to it stop pointing at a dead page.
    fn on_video_gone(&mut self, youtube_id: &str, cx: &mut Context<Self>) {
        let Some(channel) = self.account().and_then(|account| account.channel_id.clone()) else { return };
        let linked: Vec<Video> = self
            .state
            .read(cx)
            .rows()
            .iter()
            .map(|row| &row.video)
            .filter(|video| {
                video.youtube.as_ref().is_some_and(|link| link.video_id == youtube_id && link.channel_id == channel)
            })
            .cloned()
            .collect();
        if linked.is_empty() {
            return;
        }
        self.gone.insert(youtube_id.to_owned());
        self.remote.remove(youtube_id);
        self.state.update(cx, |state, cx| {
            for mut video in linked {
                video.youtube = None;
                state.update_video(video, cx);
            }
        });
        cx.emit(YoutubeEvent::UploadsChanged);
        cx.notify();
    }
}

/// Sign-in worker: a check that the sign-in can work, consent in the browser, then the
/// account it belongs to.
fn sign_in(config: YoutubeConfig, tokens: TokenSink, cancel: &AtomicBool, steps: &mpsc::UnboundedSender<SignInStep>) {
    let result = (|| {
        // Opening the browser on a sign-in that can't work only leaves the creator
        // waiting: stopped emulators, no connection, or a client Google doesn't know.
        if let Some((google, youtube)) = emulator_urls(&config)
            && let EmulatorCheck::Unreachable(problem) = probe_emulators(&google, &youtube)
        {
            return Err(SignInError::Failed(problem.to_string()));
        }
        check_client(&config).map_err(|error| SignInError::new(error, &config))?;
        if cancel.load(Ordering::Relaxed) {
            return Err(SignInError::Cancelled);
        }
        sign_in_with_browser(config.clone(), tokens, cancel, steps).map_err(|error| SignInError::new(error, &config))
    })();
    let _ = steps.unbounded_send(SignInStep::Finished(Box::new(result)));
}

fn sign_in_with_browser(
    config: YoutubeConfig,
    tokens: TokenSink,
    cancel: &AtomicBool,
    steps: &mpsc::UnboundedSender<SignInStep>,
) -> Result<SignedIn, YoutubeError> {
    let pending = begin_authorization(&config)?;
    // The receiver is gone only when the app is shutting down.
    let _ = steps.unbounded_send(SignInStep::Opened(pending.authorize_url().to_owned()));
    let credentials = pending.wait(SIGN_IN_TIMEOUT, cancel)?;
    if !credentials.has_scope(UPLOAD_SCOPE) {
        // Google lets people untick permissions on the consent page.
        return Err(YoutubeError::Api {
            status: 403,
            reason: "insufficientPermissions".to_owned(),
            message: String::new(),
        });
    }
    let client = new_client(config.clone(), credentials, tokens.clone());
    let account = read_account(&client, &config)?;
    Ok(SignedIn { client, tokens, account })
}

/// Why a sign-in failed, for the creator.
fn sign_in_message(error: &YoutubeError, config: &YoutubeConfig) -> String {
    let minutes = SIGN_IN_TIMEOUT.as_secs() / 60;
    match error {
        error if error.is_invalid_client() => "Google doesn’t recognise this client ID and secret. Copy both \
            again from Google Cloud › Clients, from a client of type Desktop app."
            .to_owned(),
        error if error.is_network() && config.is_google() => {
            "Can’t reach Google. Check your internet connection and try again.".to_owned()
        }
        error if error.is_network() => format!("Nothing answers at {}. Is the server running?", config.auth_url),
        YoutubeError::Timeout => {
            format!("The sign-in wasn’t finished in the browser within {minutes} minutes. Connect again to start over.")
        }
        YoutubeError::AccessDenied => {
            "The sign-in was cancelled in the browser. Connect again and allow access to upload.".to_owned()
        }
        error => describe_error(error),
    }
}

/// The emulator base URLs of an emulator `config` (see [`YoutubeConfig::emulator`]).
fn emulator_urls(config: &YoutubeConfig) -> Option<(String, String)> {
    if config.is_google() {
        return None;
    }
    let google = config.auth_url.strip_suffix("/o/oauth2/v2/auth")?;
    let youtube = config.api_base.strip_suffix("/youtube/v3")?;
    Some((google.to_owned(), youtube.to_owned()))
}

/// `Running`, or which emulator did not answer.
pub(super) fn probe_emulators(google_url: &str, youtube_url: &str) -> EmulatorCheck {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(PROBE_TIMEOUT)).build().into();
    let answers = |url: String| agent.get(&url).call().is_ok();
    let google = answers(format!("{google_url}/.well-known/openid-configuration"));
    let youtube = answers(format!("{youtube_url}/_emulator/uploads"));
    match (google, youtube) {
        (true, true) => EmulatorCheck::Running,
        (false, true) => {
            EmulatorCheck::Unreachable(format!("The Google emulator isn’t answering at {google_url}.").into())
        }
        (true, false) => {
            EmulatorCheck::Unreachable(format!("The YouTube emulator isn’t answering at {youtube_url}.").into())
        }
        (false, false) => {
            EmulatorCheck::Unreachable("Neither emulator is answering. Start them with scripts/emulators.sh.".into())
        }
    }
}

/// The Google account and its YouTube channel, if it has one.
fn read_account(client: &YoutubeClient, config: &YoutubeConfig) -> Result<Account, YoutubeError> {
    let user = client.userinfo()?;
    let channel = match client.my_channel() {
        Ok(channel) => Some(channel),
        Err(YoutubeError::NoChannel) => None,
        Err(error) => return Err(error),
    };
    Ok(Account::new(&user, channel.as_ref(), config))
}

fn new_client(config: YoutubeConfig, credentials: Credentials, tokens: TokenSink) -> Arc<YoutubeClient> {
    Arc::new(YoutubeClient::new(config, credentials, move |credentials| tokens.save_credentials(credentials)))
}

/// The YouTube backend for `backend`, or why it is incomplete. The environment
/// overrides (`VIDEOCRM_GOOGLE_URL`, `VIDEOCRM_YOUTUBE_URL`) win over settings; the
/// flag says whether they did.
pub fn backend_config(backend: &YoutubeBackend) -> (Result<YoutubeConfig, SharedString>, bool) {
    if let Some(config) = YoutubeConfig::emulator_from_env() {
        return (Ok(config), true);
    }
    let config = match backend {
        YoutubeBackend::Google { client_id, client_secret } => {
            let (client_id, client_secret) = (client_id.trim(), client_secret.trim());
            if client_id.is_empty() || client_secret.is_empty() {
                Err("Add the client ID and client secret of your Google OAuth client.".into())
            } else if !client_id.ends_with(CLIENT_ID_SUFFIX) || client_id.contains(char::is_whitespace) {
                Err(format!("This isn’t a Google OAuth client ID. Client IDs end with {CLIENT_ID_SUFFIX}.").into())
            } else {
                Ok(YoutubeConfig::google(client_id, Some(client_secret.to_owned())))
            }
        }
        YoutubeBackend::Emulator { google_url, youtube_url } => {
            let url = |value: &str, default: &str| {
                if value.trim().is_empty() { default.to_owned() } else { value.trim().to_owned() }
            };
            Ok(YoutubeConfig::emulator(
                &url(google_url, settings::DEFAULT_EMULATOR_GOOGLE_URL),
                &url(youtube_url, settings::DEFAULT_EMULATOR_YOUTUBE_URL),
            ))
        }
    };
    (config, false)
}

fn avatar_file_name(url: &str) -> String {
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn download_avatar(url: &str, path: &Path) -> io::Result<()> {
    if path.is_file() {
        return Ok(());
    }
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(AVATAR_TIMEOUT)).build().into();
    let bytes =
        agent.get(url).call().and_then(|mut response| response.body_mut().read_to_vec()).map_err(io::Error::other)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, bytes)
}

/// Shows a notification in the active window; nothing happens without a window.
pub(super) fn push_notification(cx: &mut App, notification: impl FnOnce() -> Notification) {
    let window = cx.active_window().or_else(|| cx.windows().into_iter().next());
    if let Some(window) = window {
        let notification = notification();
        // The window may have closed since; then there is nobody to tell.
        let _ = window.update(cx, |_, window, cx| window.push_notification(notification, cx));
    }
}

/// A YouTube error as the creator should read it. The technical detail goes to stderr.
pub(super) fn describe_error(error: &YoutubeError) -> String {
    log_line!("videocrm: youtube: {error}");
    let message = match error {
        error if error.is_network() => "Can’t reach YouTube. Check your internet connection and try again.",
        // Quota, upload limit, missing permission, rate limits, outages.
        error if let Some(message) = error.api_message() => message,
        error if error.needs_sign_in() => {
            "YouTube no longer accepts the sign-in. Reconnect the account, then try again."
        }
        YoutubeError::ThumbnailsNotAllowed => {
            "YouTube allows custom thumbnails only on verified channels. Verify yours at youtube.com/verify, \
             then set the thumbnail in YouTube Studio."
        }
        YoutubeError::Api { status: 400..=499, .. } => {
            "YouTube refused the request. Check the video’s details and file, then try again."
        }
        YoutubeError::Api { .. } => "YouTube couldn’t handle the request. Try again in a few minutes.",
        YoutubeError::SessionExpired => {
            "YouTube can’t continue this upload. Start over to send the video from the beginning."
        }
        YoutubeError::Io { .. } => "Couldn’t read the file. Check that it’s still there and readable, then try again.",
        YoutubeError::Protocol(_) | YoutubeError::Json(_) => {
            "YouTube answered in a way VideoCRM didn’t expect. Try again in a few minutes."
        }
        YoutubeError::NoChannel => {
            "This Google account has no YouTube channel yet. Create one on youtube.com, then connect again."
        }
        YoutubeError::OAuth { .. } => "Google didn’t accept the sign-in. Connect again to retry.",
        YoutubeError::AccessDenied => "The sign-in was cancelled in the browser. Connect again and allow access.",
        YoutubeError::Timeout => "The sign-in took too long. Connect again to start over.",
        YoutubeError::Cancelled { .. } => "The upload was cancelled.",
        // VideoCRM's own checks, written as sentences already.
        YoutubeError::InvalidInput(text) => return sentence(text),
        error => return sentence(&error.to_string()),
    };
    message.to_owned()
}

/// A failed write to the credentials file, as the creator should read it.
fn credentials_problem(title: &str, error: &YoutubeError) -> Problem {
    log_line!("videocrm: {title}: {error}");
    Problem::new(
        title.to_owned(),
        "VideoCRM can’t write credentials.json in its data folder. Check that the folder is writable, then try again.",
    )
}

/// Shows `problem` the way the shell shows every app problem.
pub(super) fn report_problem(state: &Entity<AppState>, problem: Problem, cx: &mut App) {
    state.update(cx, |state, cx| state.report_problem(problem, cx));
}

/// Library errors read like `network error: ...`; the UI shows sentences.
pub(super) fn sentence(text: &str) -> String {
    let text = text.trim();
    let mut chars = text.chars();
    let mut sentence: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => return String::new(),
    };
    if !sentence.ends_with(['.', '!', '?']) {
        sentence.push('.');
    }
    sentence
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_backend_needs_both_client_fields() {
        let google =
            |id: &str, secret: &str| YoutubeBackend::Google { client_id: id.into(), client_secret: secret.into() };
        // Only meaningful without the environment override, which the test run does not set.
        if YoutubeConfig::emulator_from_env().is_some() {
            return;
        }
        assert!(backend_config(&google("", "secret")).0.is_err());
        assert!(backend_config(&google("id", "  ")).0.is_err());
        // A typo shows before the browser opens: the secret pasted as the ID, a cut-off ID.
        let problem = backend_config(&google("GOCSPX-abc", "GOCSPX-abc")).0.unwrap_err();
        assert_eq!(
            problem.as_ref(),
            "This isn’t a Google OAuth client ID. Client IDs end with .apps.googleusercontent.com."
        );
        assert!(backend_config(&google("1234-abc.apps.googleusercontent", "GOCSPX-x")).0.is_err());
        let config = backend_config(&google(" id.apps.googleusercontent.com ", "GOCSPX-x")).0.unwrap();
        assert!(config.is_google());
        assert_eq!(config.client_id, "id.apps.googleusercontent.com");

        let emulator = YoutubeBackend::Emulator { google_url: " ".into(), youtube_url: "http://127.0.0.1:9/".into() };
        let (config, overridden) = backend_config(&emulator);
        let config = config.unwrap();
        assert!(!overridden);
        assert_eq!(config.auth_url, "http://127.0.0.1:4002/o/oauth2/v2/auth");
        assert_eq!(config.api_base, "http://127.0.0.1:9/youtube/v3");
    }

    #[test]
    fn refreshed_tokens_are_saved_only_while_signed_in() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("credentials.json"));
        let credentials = |token: &str| Credentials {
            access_token: token.into(),
            refresh_token: Some("refresh".into()),
            expires_at: chrono::Utc::now(),
            scope: UPLOAD_SCOPE.into(),
            id_token: None,
        };
        let tokens = TokenSink::new(store.clone(), false);
        tokens.save_credentials(&credentials("before the sign-in is accepted"));
        assert_eq!(store.load().unwrap(), None);

        tokens.sign_in(&SavedLogin { credentials: credentials("first"), account: None }).unwrap();
        tokens.save_credentials(&credentials("refreshed"));
        assert_eq!(store.load().unwrap().unwrap().credentials.access_token, "refreshed");

        tokens.sign_out().unwrap();
        tokens.save_credentials(&credentials("after the sign-out"));
        assert!(tokens.update(&SavedLogin { credentials: credentials("late"), account: None }).is_ok());
        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn sign_in_failures_say_what_to_fix() {
        let google = YoutubeConfig::google("1-a.apps.googleusercontent.com", Some("GOCSPX-x".into()));
        let unknown_client = YoutubeError::OAuth { error: "invalid_client".into(), description: None };
        assert!(sign_in_message(&unknown_client, &google).starts_with("Google doesn’t recognise this client ID"));
        let offline = YoutubeError::Http(ureq::Error::ConnectionFailed);
        assert_eq!(
            sign_in_message(&offline, &google),
            "Can’t reach Google. Check your internet connection and try again."
        );
        assert!(sign_in_message(&YoutubeError::Timeout, &google).contains("within 5 minutes"));

        let emulator = YoutubeConfig::emulator("http://127.0.0.1:9", "http://127.0.0.1:10");
        assert_eq!(emulator_urls(&emulator), Some(("http://127.0.0.1:9".into(), "http://127.0.0.1:10".into())));
        assert_eq!(emulator_urls(&google), None);
    }

    #[test]
    fn probing_reports_which_emulator_is_down() {
        // Nothing listens on port 9 (discard) of the loopback interface in a test run.
        let check = probe_emulators("http://127.0.0.1:9", "http://127.0.0.1:9");
        assert_eq!(
            check,
            EmulatorCheck::Unreachable("Neither emulator is answering. Start them with scripts/emulators.sh.".into())
        );
    }

    #[test]
    fn outages_read_as_plain_language() {
        let offline = YoutubeError::Http(ureq::Error::ConnectionFailed);
        assert_eq!(describe_error(&offline), "Can’t reach YouTube. Check your internet connection and try again.");
        let outage = YoutubeError::Api { status: 503, reason: "backendError".into(), message: "Backend Error".into() };
        assert_eq!(describe_error(&outage), "YouTube is having problems right now. Try again in a few minutes.");
        assert!(describe_error(&YoutubeError::ThumbnailsNotAllowed).contains("youtube.com/verify"));
    }

    #[test]
    fn raw_library_text_stays_in_the_log() {
        let refused = YoutubeError::Api {
            status: 400,
            reason: "badContent".into(),
            message: "Content-Range total 400001000 does not match X-Upload-Content-Length 400000000.".into(),
        };
        assert_eq!(
            describe_error(&refused),
            "YouTube refused the request. Check the video’s details and file, then try again."
        );
        let quota = YoutubeError::Api { status: 403, reason: "quotaExceeded".into(), message: String::new() };
        assert!(describe_error(&quota).starts_with("The YouTube API quota for today is used up."));
        let unreadable = YoutubeError::Io {
            context: "opening /videos/final.mp4".into(),
            source: io::Error::from(io::ErrorKind::PermissionDenied),
        };
        assert_eq!(
            describe_error(&unreadable),
            "Couldn’t read the file. Check that it’s still there and readable, then try again."
        );
        let odd = YoutubeError::Protocol("the upload server holds 10 of 10 bytes but did not finish the upload".into());
        assert!(!describe_error(&odd).contains("bytes"));
    }

    #[test]
    fn errors_become_sentences() {
        assert_eq!(sentence("network error: connection refused"), "Network error: connection refused.");
        assert_eq!(sentence("Already fine."), "Already fine.");
        assert_eq!(sentence("  "), "");
        assert_ne!(avatar_file_name("https://a/1.png"), avatar_file_name("https://a/2.png"));
    }
}
