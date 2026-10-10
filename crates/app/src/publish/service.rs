//! [`YoutubeService`]: the app-wide YouTube connection. Settings decide the backend,
//! browser sign-ins add YouTube accounts, and one credentials file per account keeps
//! it across restarts. A video uploads to the account it chose, or to the default
//! account. Uploads live in `upload.rs`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, AppContext as _, Context, Entity, EventEmitter, Global, SharedString, Subscription};
use videocrm_core::settings::{self, YoutubeBackend};
use videocrm_core::{PublishChannel, UploadId, UploadJob, UploadStatus, Video, VideoId, YoutubeLink};
use videocrm_youtube::{
    Account, CredentialStore, Credentials, Error as YoutubeError, SavedLogin, VideoInfo, YoutubeClient, YoutubeConfig,
    begin_authorization, check_client,
};

use super::rules::AccountAccess;
use super::ui::display_name;
use super::upload::{ActiveUpload, ProcessingOutcome, Retired};
use crate::state::{AppEvent, AppState, Problem, Severity};

/// One credentials file per account, in this folder of the data directory.
const ACCOUNTS_DIR: &str = "youtube-accounts";
/// Where the one sign-in lived before VideoCRM kept several; it moves to [`ACCOUNTS_DIR`].
const LEGACY_CREDENTIALS_FILE: &str = "credentials.json";
const AVATAR_DIR: &str = "avatars";
/// A browser sign-in left open longer than this is abandoned.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// How long "Check" and the check before an emulator sign-in wait for an answer.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// Every Google OAuth client ID ends like this.
const CLIENT_ID_SUFFIX: &str = ".apps.googleusercontent.com";
const UPLOAD_SCOPE: &str = "https://www.googleapis.com/auth/youtube.upload";
const AVATAR_TIMEOUT: Duration = Duration::from_secs(15);
/// The Publish tab asks YouTube about its video again when it is shown, at
/// most this often: each answer costs a unit of the daily quota.
const REMOTE_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Settings key of the videos YouTube said it no longer has, by YouTube video id.
const GONE_KEY: &str = "youtube.gone_videos";
/// Settings key of the account videos upload to unless they chose another, by [`Account::key`].
const DEFAULT_ACCOUNT_KEY: &str = "youtube.default_account";

/// Where a YouTube account stands, as a view that shows one account sees it.
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
    /// An account was added, removed, made the default, or changed state, or a
    /// sign-in started or ended.
    ConnectionChanged,
    /// An upload job was created or changed status.
    UploadsChanged,
}

/// A YouTube account VideoCRM signed in to.
pub struct YoutubeAccount {
    account: Account,
    /// `None` once YouTube rejected the saved sign-in: the account needs a new one.
    client: Option<Arc<YoutubeClient>>,
    /// The account's credentials file, while the sign-in lasts.
    tokens: TokenSink,
    /// The channel picture (or account picture), once downloaded.
    avatar: Option<PathBuf>,
}

impl YoutubeAccount {
    pub fn account(&self) -> &Account {
        &self.account
    }

    /// See [`Account::key`].
    pub fn key(&self) -> &str {
        self.account.key()
    }

    /// Whether uploads can use the account now; otherwise it needs a new sign-in.
    pub fn is_connected(&self) -> bool {
        self.client.is_some()
    }

    pub fn avatar(&self) -> Option<&Path> {
        self.avatar.as_deref()
    }

    /// The channel title, falling back to the account name and email.
    pub fn name(&self) -> String {
        display_name(&self.account)
    }

    /// Whether videos can upload to it: an account without a channel has nowhere to put them.
    pub fn has_channel(&self) -> bool {
        self.account.channel_id.is_some()
    }

    /// The account as a video's choice of where to upload.
    pub fn publish_channel(&self) -> PublishChannel {
        PublishChannel { id: self.key().to_owned(), name: display_name(&self.account) }
    }
}

/// App-wide YouTube connection: settings -> clients, OAuth sign-ins, credentials, uploads.
pub struct YoutubeService {
    pub(super) state: Entity<AppState>,
    data_dir: PathBuf,
    config: Result<YoutubeConfig, SharedString>,
    /// The config comes from `VIDEOCRM_GOOGLE_URL` / `VIDEOCRM_YOUTUBE_URL`, not settings.
    env_override: bool,
    /// The signed-in accounts, by name.
    accounts: Vec<YoutubeAccount>,
    /// Key of the account videos upload to unless they chose another. Names one of
    /// `accounts` whenever there are any.
    default_account: Option<String>,
    /// The browser sign-in in progress.
    sign_in: Option<SignIn>,
    sign_in_attempt: u64,
    /// Why the last sign-in failed, until the next one starts.
    sign_in_problem: Option<SharedString>,
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
    /// removed by YouTube), by YouTube video id. Kept across starts: once its
    /// link is dropped, only this says an earlier upload of a video is gone.
    gone: HashSet<String>,
    remote_requests: HashSet<String>,
    /// When YouTube was last asked about an uploaded video, by YouTube video id.
    remote_asked: HashMap<String, Instant>,
    _subscription: Subscription,
}

struct GlobalYoutubeService(Entity<YoutubeService>);

impl Global for GlobalYoutubeService {}

impl EventEmitter<YoutubeEvent> for YoutubeService {}

/// A browser sign-in in progress.
struct SignIn {
    cancel: Arc<AtomicBool>,
    /// The consent page, once it exists.
    authorize_url: Option<SharedString>,
    /// The account signing in again, by key. It keeps asking for a new sign-in
    /// until one succeeds.
    reconnecting: Option<String>,
    /// The video whose Publish tab added the account: it uploads to the account.
    for_video: Option<VideoId>,
}

enum SignInStep {
    Opened(String),
    Finished(Box<Result<SignedIn, SignInError>>),
}

/// Why a sign-in ended without an account.
enum SignInError {
    Cancelled,
    /// Cancelled on the browser's consent page; what to tell the creator.
    Denied(String),
    /// What to tell the creator.
    Failed(String),
}

impl SignInError {
    fn new(error: YoutubeError, config: &YoutubeConfig) -> Self {
        match error {
            YoutubeError::Cancelled { .. } => SignInError::Cancelled,
            YoutubeError::AccessDenied => SignInError::Denied(sign_in_message(&error, config)),
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

/// Saves a client's refreshed tokens to its credentials file while its sign-in lasts.
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

    /// [`YoutubeService::init`] with the credentials files and cached avatars in `data_dir`.
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

    /// The folder of the credentials files in `data_dir`, one per signed-in account.
    pub fn accounts_dir(data_dir: &Path) -> PathBuf {
        data_dir.join(ACCOUNTS_DIR)
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
            data_dir,
            config,
            env_override,
            accounts: Vec::new(),
            default_account: None,
            sign_in: None,
            sign_in_attempt: 0,
            sign_in_problem: None,
            jobs: Vec::new(),
            active: HashMap::new(),
            outcomes: HashMap::new(),
            thumbnail_problems: HashMap::new(),
            retired: HashMap::new(),
            checking_files: false,
            remote: HashMap::new(),
            gone: HashSet::new(),
            remote_requests: HashSet::new(),
            remote_asked: HashMap::new(),
            _subscription: subscription,
        };
        let legacy = service.restore_logins(cx);
        service.recover_interrupted_uploads(cx);
        if let Some(key) = legacy {
            service.adopt_legacy_uploads(&key, cx);
        }
        service.load_gone(cx);
        service.check_stopped_uploads(cx);
        service
    }

    pub fn state(&self) -> &Entity<AppState> {
        &self.state
    }

    /// Every signed-in account, by name.
    pub fn accounts(&self) -> &[YoutubeAccount] {
        &self.accounts
    }

    /// The signed-in account with this [`Account::key`].
    pub fn find_account(&self, key: &str) -> Option<&YoutubeAccount> {
        self.accounts.iter().find(|account| account.key() == key)
    }

    fn find_account_mut(&mut self, key: &str) -> Option<&mut YoutubeAccount> {
        self.accounts.iter_mut().find(|account| account.key() == key)
    }

    /// The account videos upload to unless they chose another.
    pub fn default_account(&self) -> Option<&YoutubeAccount> {
        self.default_account.as_deref().and_then(|key| self.find_account(key))
    }

    /// The account `key` names, or the default account for `None`.
    fn account_or_default(&self, key: Option<&str>) -> Option<&YoutubeAccount> {
        match key {
            Some(key) => self.find_account(key),
            None => self.default_account(),
        }
    }

    /// Key of the account `video` uploads to: the one it chose, else the default account.
    pub fn channel_for(&self, video: &Video) -> Option<String> {
        match &video.publish.channel {
            Some(channel) => Some(channel.id.clone()),
            None => self.default_account.clone(),
        }
    }

    /// Where the default account stands; see [`Self::connection_for`].
    pub fn connection(&self) -> Connection {
        self.connection_for(None)
    }

    /// Where the account `key` (the default account for `None`) stands: signed in,
    /// asking for a new sign-in, or signing in again. Without such an account, the
    /// sign-in that adds one, the last failed sign-in, or nothing.
    pub fn connection_for(&self, key: Option<&str>) -> Connection {
        let connecting = |sign_in: &SignIn| Connection::Connecting { authorize_url: sign_in.authorize_url.clone() };
        match (self.account_or_default(key), &self.sign_in) {
            (Some(account), Some(sign_in)) if sign_in.reconnecting.as_deref() == Some(account.key()) => {
                connecting(sign_in)
            }
            (Some(account), _) if account.is_connected() => Connection::Connected(account.account.clone()),
            (Some(account), _) => Connection::NeedsSignIn(account.account.clone()),
            (None, Some(sign_in)) if sign_in.reconnecting.is_none() => connecting(sign_in),
            (None, _) => self.sign_in_problem.clone().map_or(Connection::Disconnected, Connection::Failed),
        }
    }

    /// The default account, while uploads can use it.
    pub fn account(&self) -> Option<&Account> {
        self.default_account().filter(|account| account.is_connected()).map(YoutubeAccount::account)
    }

    /// Whether uploads can use the default account now.
    pub fn is_connected(&self) -> bool {
        self.account().is_some()
    }

    /// Whether uploads can use the default account now, or what they need first.
    pub fn account_access(&self) -> AccountAccess {
        self.account_access_for(None)
    }

    /// Whether uploads can use the account `key` (the default account for `None`)
    /// now, or what they need first.
    pub fn account_access_for(&self, key: Option<&str>) -> AccountAccess {
        let signing_in = |reconnecting: Option<&str>| {
            self.sign_in.as_ref().is_some_and(|sign_in| sign_in.reconnecting.as_deref() == reconnecting)
        };
        match self.account_or_default(key) {
            Some(account) if account.is_connected() => AccountAccess::Connected,
            Some(account) if signing_in(Some(account.key())) => AccountAccess::SigningIn,
            Some(_) => AccountAccess::NeedsSignIn,
            None if signing_in(None) => AccountAccess::SigningIn,
            // The account the video chose isn't signed in anymore.
            None if key.is_some() => AccountAccess::Gone,
            None => AccountAccess::NotConnected,
        }
    }

    /// Whether a browser sign-in is under way.
    pub fn is_signing_in(&self) -> bool {
        self.sign_in.is_some()
    }

    /// The consent page of the sign-in under way, once it exists.
    pub fn sign_in_url(&self) -> Option<&SharedString> {
        self.sign_in.as_ref().and_then(|sign_in| sign_in.authorize_url.as_ref())
    }

    /// The account the sign-in under way signs in again, by key.
    pub fn reconnecting(&self) -> Option<&str> {
        self.sign_in.as_ref().and_then(|sign_in| sign_in.reconnecting.as_deref())
    }

    /// Why the last sign-in failed, until the next one starts.
    pub fn sign_in_problem(&self) -> Option<&SharedString> {
        self.sign_in_problem.as_ref()
    }

    /// The backend the settings describe, or why they do not describe one yet.
    pub fn config(&self) -> Result<&YoutubeConfig, &SharedString> {
        self.config.as_ref()
    }

    /// Whether `VIDEOCRM_GOOGLE_URL` / `VIDEOCRM_YOUTUBE_URL` override the settings.
    pub fn is_env_override(&self) -> bool {
        self.env_override
    }

    /// The default account's channel picture (or account picture), once downloaded.
    pub fn avatar(&self) -> Option<&Path> {
        self.default_account().and_then(YoutubeAccount::avatar)
    }

    /// Client for the default account. Refreshed tokens are saved to its credentials file.
    pub fn client(&self) -> Option<Arc<YoutubeClient>> {
        self.default_account().and_then(|account| account.client.clone())
    }

    /// Client for the account `key`, while uploads can use it.
    pub(super) fn client_for(&self, key: &str) -> Option<Arc<YoutubeClient>> {
        self.find_account(key).and_then(|account| account.client.clone())
    }

    /// Adds an account: opens the consent page in the browser and waits for the
    /// sign-in in the background. Signing in to an account that is already there
    /// signs it in again.
    pub fn connect(&mut self, cx: &mut Context<Self>) {
        self.begin_sign_in(None, None, cx);
    }

    /// [`Self::connect`] for `video_id`: the video uploads to the account signed in.
    pub fn connect_for(&mut self, video_id: VideoId, cx: &mut Context<Self>) {
        self.begin_sign_in(None, Some(video_id), cx);
    }

    /// Signs the account `key` in again, after YouTube stopped accepting its sign-in.
    pub fn reconnect(&mut self, key: &str, cx: &mut Context<Self>) {
        self.begin_sign_in(Some(key.to_owned()), None, cx);
    }

    fn begin_sign_in(&mut self, reconnecting: Option<String>, for_video: Option<VideoId>, cx: &mut Context<Self>) {
        // One browser sign-in at a time: the consent page names no account.
        if self.sign_in.is_some() {
            return;
        }
        let config = match &self.config {
            Ok(config) => config.clone(),
            Err(problem) => {
                self.sign_in_problem = Some(problem.clone());
                self.connection_changed(cx);
                return;
            }
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.sign_in_attempt += 1;
        let attempt = self.sign_in_attempt;
        self.sign_in = Some(SignIn { cancel: cancel.clone(), authorize_url: None, reconnecting, for_video });
        self.sign_in_problem = None;
        self.connection_changed(cx);

        let tokens = TokenSink::new(CredentialStore::new(self.new_account_file(attempt)), false);
        let (sender, mut steps) = mpsc::unbounded();
        let spawned = thread::Builder::new()
            .name("youtube-sign-in".into())
            .spawn(move || sign_in(config, tokens, &cancel, &sender));
        if let Err(error) = spawned {
            log_line!("videocrm: couldn't start the sign-in thread: {error}");
            self.sign_in = None;
            let message = "Couldn’t start the sign-in. Quit VideoCRM and open it again, then connect.";
            self.sign_in_problem = Some(message.into());
            self.connection_changed(cx);
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

    /// A credentials file no account uses yet.
    fn new_account_file(&self, attempt: u64) -> PathBuf {
        let millis = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis());
        Self::accounts_dir(&self.data_dir).join(format!("{millis}-{attempt}.json"))
    }

    /// Stops waiting for the browser sign-in. An account that was signing in
    /// again still needs its new sign-in.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.cancel_sign_in() {
            self.connection_changed(cx);
        }
    }

    /// Signs the account `key` out: forgets its tokens and revokes them with Google,
    /// best effort. Its uploads in progress stop and can be resumed after connecting
    /// it again. Videos that chose it keep their choice.
    pub fn disconnect(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(ix) = self.accounts.iter().position(|account| account.key() == key) else { return };
        // A sign-in finishing later must not bring the account back.
        if self.reconnecting() == Some(key) {
            self.cancel_sign_in();
        }
        let account = self.accounts.remove(ix);
        for (id, upload) in &self.active {
            if self.job(*id).is_some_and(|job| job.channel.as_deref() == Some(key)) {
                upload.cancel();
            }
        }
        if let Err(error) = account.tokens.sign_out() {
            report_problem(&self.state, credentials_problem("Couldn’t remove the saved YouTube sign-in", &error), cx);
        }
        if let Some(client) = account.client {
            // The local tokens are gone either way; revoking also ends the grant at Google.
            cx.background_spawn(async move { client.revoke() }).detach();
        }
        if self.default_account.as_deref() == Some(key) {
            // Another account takes over, by choice rather than by name: a later
            // account must not take the videos without a choice by sorting first.
            let next = self.accounts.first().map(|account| account.key().to_owned());
            self.save_default(next, cx);
        }
        self.connection_changed(cx);
    }

    /// Signs every account out, as [`Self::disconnect`] does.
    pub fn disconnect_all(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<String> = self.accounts.iter().map(|account| account.key().to_owned()).collect();
        for key in keys {
            self.disconnect(&key, cx);
        }
    }

    /// Makes the account `key` the one videos upload to unless they chose another.
    pub fn set_default(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.find_account(key).is_some() && self.default_account.as_deref() != Some(key) {
            self.save_default(Some(key.to_owned()), cx);
            self.connection_changed(cx);
        }
    }

    fn save_default(&mut self, key: Option<String>, cx: &mut Context<Self>) {
        // A refusal is reported by the state; this run still knows.
        self.state.update(cx, |state, cx| state.save_setting(DEFAULT_ACCOUNT_KEY, &key, cx));
        self.default_account = key;
    }

    /// The stored default account, or the first account when that one isn't signed in.
    fn load_default(&mut self, cx: &mut Context<Self>) {
        let stored = match self.state.read(cx).read_setting::<Option<String>>(DEFAULT_ACCOUNT_KEY) {
            Ok(stored) => stored.flatten(),
            Err(error) => {
                log_line!("videocrm: couldn't read the default YouTube account: {error}");
                None
            }
        };
        match stored {
            Some(key) if self.find_account(&key).is_some() => self.default_account = Some(key),
            _ if self.accounts.is_empty() => self.default_account = None,
            _ => {
                let first = self.accounts.first().map(|account| account.key().to_owned());
                self.save_default(first, cx);
            }
        }
    }

    /// Stops the sign-in under way; `false` when there was none.
    fn cancel_sign_in(&mut self) -> bool {
        let Some(sign_in) = self.sign_in.take() else { return false };
        sign_in.cancel.store(true, Ordering::Relaxed);
        true
    }

    fn connection_changed(&mut self, cx: &mut Context<Self>) {
        // Another account may see the videos differently.
        self.remote_asked.clear();
        cx.emit(YoutubeEvent::ConnectionChanged);
        cx.notify();
    }

    fn on_sign_in_step(&mut self, attempt: u64, step: SignInStep, cx: &mut Context<Self>) {
        // A cancelled or superseded sign-in reports nothing.
        if attempt != self.sign_in_attempt {
            return;
        }
        let Some(sign_in) = self.sign_in.as_mut() else { return };
        match step {
            SignInStep::Opened(url) => {
                cx.open_url(&url);
                sign_in.authorize_url = Some(url.into());
                self.connection_changed(cx);
            }
            SignInStep::Finished(result) => {
                let Some(SignIn { reconnecting, for_video, .. }) = self.sign_in.take() else { return };
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
                                "Couldn’t save the YouTube sign-in: VideoCRM can’t write to its data folder. Check \
                                 that the folder is writable, then connect again."
                                    .to_owned(),
                            ))
                        }
                    }
                });
                match signed_in {
                    Ok(signed_in) => {
                        let key = signed_in.account.key().to_owned();
                        self.add_account(signed_in, reconnecting.as_deref(), cx);
                        if let Some(video_id) = for_video {
                            self.choose_channel(video_id, &key, cx);
                        }
                    }
                    Err(SignInError::Cancelled) => self.connection_changed(cx),
                    // Cancelled in the browser while signing in again: as with Cancel
                    // in the app, the account still asks for its new sign-in.
                    Err(SignInError::Denied(message)) => {
                        match reconnecting.as_deref().and_then(|key| self.find_account(key)) {
                            Some(account) => {
                                let notice = Problem::new(
                                    "Sign-in cancelled",
                                    format!(
                                        "The sign-in was cancelled in the browser. Reconnect to keep uploading as {}.",
                                        display_name(&account.account)
                                    ),
                                )
                                .with_severity(Severity::Info);
                                report_problem(&self.state, notice, cx);
                            }
                            None => self.sign_in_problem = Some(message.into()),
                        }
                        self.connection_changed(cx);
                    }
                    Err(SignInError::Failed(message)) => {
                        self.sign_in_problem = Some(message.into());
                        self.connection_changed(cx);
                    }
                }
            }
        }
    }

    /// Keeps a finished sign-in. One of an account that is already there replaces
    /// its earlier sign-in.
    fn add_account(&mut self, signed_in: SignedIn, reconnecting: Option<&str>, cx: &mut Context<Self>) {
        let SignedIn { client, tokens, account } = signed_in;
        let key = account.key().to_owned();
        // The same account, also when it got a channel since its earlier sign-in.
        let same = |known: &YoutubeAccount| {
            known.key() == key
                || (known.account.channel_id.is_none()
                    && account.channel_id.is_some()
                    && known.account.email == account.email)
        };
        let mut earlier_key = None;
        if let Some(ix) = self.accounts.iter().position(same) {
            // The new sign-in has its own credentials file; the earlier one goes. Its
            // grant stays: it is the same account and app.
            let earlier = self.accounts.remove(ix);
            if let Err(error) = earlier.tokens.sign_out() {
                log_line!("videocrm: couldn't remove the earlier YouTube sign-in: {error}");
            }
            earlier_key = Some(earlier.key().to_owned());
        }
        // Signing in again picks the account on the consent page: another one
        // may come back, and the account signing in again still waits for its turn.
        if let Some(waiting) = reconnecting.filter(|waiting| *waiting != key).and_then(|key| self.find_account(key)) {
            let notice = Problem::new(
                format!("Signed in as {}", display_name(&account)),
                format!(
                    "{} still needs a new sign-in. Reconnect it and pick it in the browser to keep uploading as it.",
                    display_name(&waiting.account)
                ),
            )
            .with_severity(Severity::Info);
            report_problem(&self.state, notice, cx);
        }
        self.accounts.push(YoutubeAccount { account, client: Some(client), tokens, avatar: None });
        self.sort_accounts();
        if let Some(earlier) = earlier_key {
            self.rekey(&earlier, &key, cx);
        }
        if self.default_account().is_none() {
            self.save_default(Some(key.clone()), cx);
        }
        self.connection_changed(cx);
        self.refresh_avatar(&key, cx);
    }

    fn sort_accounts(&mut self) {
        self.accounts
            .sort_by_cached_key(|account| (display_name(&account.account).to_lowercase(), account.key().to_owned()));
    }

    /// Asks for a new sign-in of the account `key` after YouTube rejected its tokens.
    /// The account stays on screen, so the creator knows whom to sign in as.
    pub(super) fn require_sign_in(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(account) = self.find_account_mut(key) else { return };
        if account.client.take().is_some() {
            self.connection_changed(cx);
        }
    }

    /// Restores the saved accounts without network access, then reads them again
    /// in the background. Returns the key of the account a release that kept one
    /// account had signed in, when its sign-in moved to the accounts folder now.
    fn restore_logins(&mut self, cx: &mut Context<Self>) -> Option<String> {
        let Ok(config) = self.config.clone() else { return None };
        let legacy = self.move_legacy_login();
        let dir = Self::accounts_dir(&self.data_dir);
        let mut files: Vec<PathBuf> = match fs::read_dir(&dir) {
            Ok(entries) => entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                // Skips the temporary files of a write that never finished.
                .filter(|path| path.extension().is_some_and(|extension| extension == "json"))
                .collect(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                log_line!("videocrm: couldn't read {}: {error}", dir.display());
                self.sign_in_problem =
                    Some("Couldn’t read the saved YouTube sign-ins. Connect your accounts again.".into());
                Vec::new()
            }
        };
        files.sort();
        for file in files {
            let store = CredentialStore::new(file);
            match store.load() {
                // Tokens of another backend are not usable here.
                Ok(Some(SavedLogin { credentials, account: Some(account) }))
                    if account.backend == config.backend_label() && self.find_account(account.key()).is_none() =>
                {
                    let tokens = TokenSink::new(store, true);
                    let client = new_client(config.clone(), credentials, tokens.clone());
                    self.accounts.push(YoutubeAccount { account, client: Some(client), tokens, avatar: None });
                }
                Ok(_) => {}
                Err(error) => {
                    log_line!("videocrm: couldn't read the saved YouTube sign-in: {error}");
                    self.sign_in_problem =
                        Some("Couldn’t read a saved YouTube sign-in. Connect the account again.".into());
                }
            }
        }
        self.sort_accounts();
        self.load_default(cx);
        let keys: Vec<String> = self.accounts.iter().map(|account| account.key().to_owned()).collect();
        for key in keys {
            self.refresh_avatar(&key, cx);
            self.refresh_account(&key, cx);
        }
        legacy
    }

    /// Moves the sign-in of a release that kept one account to the accounts folder;
    /// the key of its account once moved.
    fn move_legacy_login(&self) -> Option<String> {
        let legacy = self.data_dir.join(LEGACY_CREDENTIALS_FILE);
        if !legacy.is_file() {
            return None;
        }
        let key = match CredentialStore::new(&legacy).load() {
            Ok(login) => login.and_then(|login| login.account).map(|account| account.key().to_owned()),
            Err(error) => {
                log_line!("videocrm: couldn't read {}: {error}", legacy.display());
                None
            }
        };
        let target = self.new_account_file(0);
        let moved = target.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| fs::rename(&legacy, &target));
        match moved {
            Ok(()) => key,
            Err(error) => {
                log_line!("videocrm: couldn't move {} to {}: {error}", legacy.display(), target.display());
                None
            }
        }
    }

    /// Stopped uploads stored before uploads recorded their account went to the one
    /// account that release kept: they resume with it, whatever the default becomes.
    fn adopt_legacy_uploads(&mut self, key: &str, cx: &mut Context<Self>) {
        let stopped: Vec<UploadId> =
            self.jobs.iter().filter(|job| job.channel.is_none() && self.is_resumable(job)).map(|job| job.id).collect();
        for id in stopped {
            self.update_job(id, cx, |job| job.channel = Some(key.to_owned()));
        }
    }

    /// The account known as `before` is known as `after` now (it got a channel):
    /// the default, the videos that chose it and its uploads follow.
    fn rekey(&mut self, before: &str, after: &str, cx: &mut Context<Self>) {
        let Some(channel) = self.find_account(after).map(YoutubeAccount::publish_channel) else { return };
        if before == after {
            return;
        }
        if self.default_account.as_deref() == Some(before) {
            self.save_default(Some(after.to_owned()), cx);
        }
        self.state.update(cx, |state, cx| {
            let chosen: Vec<Video> = state
                .rows()
                .iter()
                .map(|row| &row.video)
                .filter(|video| video.publish.channel.as_ref().is_some_and(|chosen| chosen.id == before))
                .cloned()
                .collect();
            for mut video in chosen {
                video.publish.channel = Some(channel.clone());
                state.update_video(video, cx);
            }
        });
        let jobs: Vec<UploadId> =
            self.jobs.iter().filter(|job| job.channel.as_deref() == Some(before)).map(|job| job.id).collect();
        for id in jobs {
            self.update_job(id, cx, |job| job.channel = Some(after.to_owned()));
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
        self.cancel_sign_in();
        self.sign_in_problem = None;
        self.disconnect_all(cx);
        self.connection_changed(cx);
    }

    /// Reads the account `key` and its channel again (name, picture, subscriber and
    /// video counts) and saves them. Offline, the saved account stays; rejected
    /// tokens ask for a new sign-in right away instead of at the next upload.
    pub(super) fn refresh_account(&mut self, key: &str, cx: &mut Context<Self>) {
        let (Some(account), Ok(config)) = (self.find_account(key), self.config.clone()) else { return };
        let (Some(client), tokens) = (account.client.clone(), account.tokens.clone()) else { return };
        cx.spawn(async move |this, cx| {
            let reader = client.clone();
            let account = cx.background_spawn(async move { read_account(&reader, &config) }).await;
            this.update(cx, |this, cx| {
                // A sign-out or another sign-in since then makes this answer stale.
                let Some(known) = this
                    .accounts
                    .iter_mut()
                    .find(|known| known.client.as_ref().is_some_and(|current| Arc::ptr_eq(current, &client)))
                else {
                    return;
                };
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
                        if known.account != account {
                            let (before, after) = (known.key().to_owned(), account.key().to_owned());
                            known.account = account;
                            this.sort_accounts();
                            // An account that got a channel since is known by the channel now.
                            this.rekey(&before, &after, cx);
                            this.connection_changed(cx);
                            this.refresh_avatar(&after, cx);
                        }
                    }
                    Err(error) if error.needs_sign_in() => {
                        known.client = None;
                        this.connection_changed(cx);
                    }
                    Err(_) => {}
                }
            })
            .ok();
        })
        .detach();
    }

    /// Downloads the channel picture of the account `key` into the data directory,
    /// once per picture URL.
    fn refresh_avatar(&mut self, key: &str, cx: &mut Context<Self>) {
        let avatars = self.data_dir.join(AVATAR_DIR);
        let Some(account) = self.find_account_mut(key) else { return };
        let Some(url) = account.account.channel_thumbnail.clone().or_else(|| account.account.picture.clone()) else {
            account.avatar = None;
            return;
        };
        let path = avatars.join(avatar_file_name(&url));
        let key = key.to_owned();
        cx.spawn(async move |this, cx| {
            let fetched = cx.background_spawn(async move { download_avatar(&url, &path).map(|()| path) }).await;
            // Without a picture the account shows its initials.
            if let Ok(path) = fetched {
                this.update(cx, |this, cx| {
                    if let Some(account) = this.find_account_mut(&key) {
                        account.avatar = Some(path);
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// Lets `video_id` upload to the account `key`, keeping that choice whatever
    /// the default account becomes.
    pub(super) fn choose_channel(&mut self, video_id: VideoId, key: &str, cx: &mut Context<Self>) {
        let Some(channel) = self.find_account(key).map(YoutubeAccount::publish_channel) else { return };
        self.state.update(cx, |state, cx| {
            let Some(mut video) = state.video(video_id).cloned() else { return };
            if video.publish.channel.as_ref() != Some(&channel) {
                video.publish.channel = Some(channel);
                state.update_video(video, cx);
            }
        });
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
    /// Publish tab shows what YouTube has rather than what was asked for. An answer
    /// younger than [`REMOTE_REFRESH_INTERVAL`] is kept instead. Only the channel it
    /// went to sees a private video, so that account asks, while it is signed in.
    pub fn refresh_youtube_video(&mut self, link: &YoutubeLink, cx: &mut Context<Self>) {
        let Some(client) = self.client_for(&link.channel_id) else { return };
        let youtube_id = link.video_id.clone();
        let now = cx.background_executor().now();
        let recent = self.remote_asked.get(&youtube_id).is_some_and(|asked| now < *asked + REMOTE_REFRESH_INTERVAL);
        if recent || !self.remote_requests.insert(youtube_id.clone()) {
            return;
        }
        self.remote_asked.insert(youtube_id.clone(), now);
        let channel = link.channel_id.clone();
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
                    Ok(None) => this.on_video_gone(&youtube_id, &channel, cx),
                    // Offline, the stored link is what the tab shows.
                    Err(_) => {}
                }
            })
            .ok();
        })
        .detach();
    }

    fn load_gone(&mut self, cx: &mut Context<Self>) {
        match self.state.read(cx).read_setting::<Vec<String>>(GONE_KEY) {
            Ok(gone) => self.gone = gone.unwrap_or_default().into_iter().collect(),
            Err(error) => log_line!("videocrm: couldn't read which uploads left YouTube: {error}"),
        }
    }

    /// Keeps which uploads YouTube no longer has, for the next start.
    fn save_gone(&mut self, cx: &mut Context<Self>) {
        let mut gone: Vec<&str> = self
            .jobs
            .iter()
            .filter_map(|job| match &job.status {
                UploadStatus::Processing { video_id } | UploadStatus::Done { video_id } => Some(video_id.as_str()),
                _ => None,
            })
            .filter(|youtube_id| self.gone.contains(*youtube_id))
            .collect();
        gone.sort_unstable();
        gone.dedup();
        // A refusal is reported by the state; this run still knows.
        self.state.update(cx, |state, cx| state.save_setting(GONE_KEY, &gone, cx));
    }

    /// The channel `channel` has no video `youtube_id`: the videos uploaded there
    /// stop pointing at a dead page.
    fn on_video_gone(&mut self, youtube_id: &str, channel: &str, cx: &mut Context<Self>) {
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
        self.save_gone(cx);
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
            "YouTube can’t continue this upload. Upload the video again to send it from the beginning."
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
        "VideoCRM can’t write to its data folder. Check that the folder is writable, then try again.",
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
