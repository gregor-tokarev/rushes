//! [`UpdateService`]: keeps Rushes up to date with its releases.
//!
//! A copy installed from a release asks GitHub for a newer one a minute after
//! it starts and every few hours after, while automatic updates are on. A
//! release it can install replaces it in the background, and the creator
//! restarts when it suits them; one it can't install (a read-only folder, the
//! disk image) is announced with its download page. Checking by hand works in
//! every copy.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::{App, AppContext as _, Context, Entity, Global, SharedString, Subscription, Task};
use rushes_update::{Asset, Delivery, Error as UpdateError, ManualReason, Progress, Update, Updater, Version};

use super::copy;
use crate::state::{AppEvent, AppState};

/// The first automatic check waits this long, so it doesn't slow the start.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(60);
/// Automatic checks after the first. GitHub answers 60 checks an hour per address.
const CHECK_INTERVAL: Duration = Duration::from_secs(4 * 60 * 60);

/// What the updater is doing, or found last.
#[derive(Clone, Debug, PartialEq)]
pub enum UpdateStatus {
    /// Nothing checked in this session yet.
    Idle,
    Checking,
    UpToDate {
        checked_at: DateTime<Utc>,
    },
    Downloading {
        version: Version,
        received: u64,
        total: u64,
    },
    /// Unpacking and putting the new version in place.
    Installing {
        version: Version,
    },
    /// A newer release this copy can't install itself.
    Available {
        version: Version,
        page_url: SharedString,
        reason: ManualReason,
    },
    Failed(UpdateFailure),
}

/// Why the last check or installation failed, as the creator reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateFailure {
    pub title: SharedString,
    pub message: SharedString,
    /// The release's page, when a release was found: it can be downloaded by hand.
    pub page_url: Option<SharedString>,
}

/// Whether the creator asked for the check; automatic checks announce what
/// they find with a notification, checks by hand show it where they were asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trigger {
    Automatic,
    Manual,
}

/// What a notification told the creator about a version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Notice {
    Ready,
    Available,
    Failed,
}

pub struct UpdateService {
    updater: Arc<Updater>,
    status: UpdateStatus,
    /// Installed by this session; the next start runs it.
    installed: Option<Version>,
    /// The notifications shown in this session, so later checks don't repeat them.
    announced: Vec<(Version, Notice)>,
    automatic: bool,
    /// Checks every few hours while automatic updates are on.
    schedule: Option<Task<()>>,
    /// The check or installation running now.
    work: Option<Task<()>>,
    /// Stops a download when the app quits.
    cancel: Arc<AtomicBool>,
    _subscriptions: Vec<Subscription>,
}

struct GlobalUpdateService(Entity<UpdateService>);

impl Global for GlobalUpdateService {}

impl UpdateService {
    /// Creates the service for the running copy and registers it globally.
    /// Call once at startup, after `AppState::set_global`.
    pub fn init(state: Entity<AppState>, cx: &mut App) {
        let current = Version::parse(env!("CARGO_PKG_VERSION")).expect("the workspace version is a version");
        Self::init_with(state, Updater::for_running_copy(current), cx);
    }

    /// [`UpdateService::init`] with `updater` (tests point it at a local server).
    pub fn init_with(state: Entity<AppState>, updater: Updater, cx: &mut App) -> Entity<UpdateService> {
        let service = cx.new(|cx| {
            let automatic = state.read(cx).settings().updates.automatic;
            let subscriptions = vec![
                cx.subscribe(&state, |this: &mut Self, state, event: &AppEvent, cx| {
                    if *event == AppEvent::SettingsChanged {
                        let automatic = state.read(cx).settings().updates.automatic;
                        this.set_automatic(automatic, cx);
                    }
                }),
                cx.on_app_quit(|this: &mut Self, _| {
                    this.cancel.store(true, Ordering::Relaxed);
                    async {}
                }),
            ];
            let mut service = UpdateService {
                updater: Arc::new(updater),
                status: UpdateStatus::Idle,
                installed: None,
                announced: Vec::new(),
                automatic: false,
                schedule: None,
                work: None,
                cancel: Arc::default(),
                _subscriptions: subscriptions,
            };
            service.set_automatic(automatic, cx);
            service
        });
        cx.set_global(GlobalUpdateService(service.clone()));
        service
    }

    pub fn global(cx: &App) -> Entity<UpdateService> {
        cx.global::<GlobalUpdateService>().0.clone()
    }

    /// The service, when the app has one (views tested on their own may not).
    pub fn try_global(cx: &App) -> Option<Entity<UpdateService>> {
        cx.try_global::<GlobalUpdateService>().map(|global| global.0.clone())
    }

    pub fn status(&self) -> &UpdateStatus {
        &self.status
    }

    /// The version running now.
    pub fn current(&self) -> &Version {
        self.updater.current()
    }

    /// The version this session installed, which the next start runs.
    pub fn installed(&self) -> Option<&Version> {
        self.installed.as_ref()
    }

    /// Whether this copy was installed from a release, so it can replace
    /// itself and checks on its own.
    pub fn updates_itself(&self) -> bool {
        self.updater.installation().is_some()
    }

    /// Whether a check or installation is running.
    pub fn is_busy(&self) -> bool {
        matches!(
            self.status,
            UpdateStatus::Checking | UpdateStatus::Downloading { .. } | UpdateStatus::Installing { .. }
        )
    }

    /// Checks for a newer release now, and installs it if this copy can.
    pub fn check_now(&mut self, cx: &mut Context<Self>) {
        self.check(Trigger::Manual, cx);
    }

    /// Starts the installed version: once this process exits, a helper opens
    /// Rushes again, then the app quits (saving pending edits as Quit does).
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        match self.updater.relaunch_after_exit() {
            Ok(()) => cx.quit(),
            Err(error) => {
                log_line!("rushes: cannot restart: {error}");
                let version = self.installed.clone();
                crate::ui::push_notification(cx, move || copy::restart_failed(version.as_ref()));
            }
        }
    }

    fn set_automatic(&mut self, automatic: bool, cx: &mut Context<Self>) {
        // A copy built from source has nothing to install; it checks only when asked.
        let automatic = automatic && self.updates_itself();
        if automatic == self.automatic {
            return;
        }
        self.automatic = automatic;
        self.schedule = automatic.then(|| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(FIRST_CHECK_DELAY).await;
                loop {
                    if this.update(cx, |this, cx| this.check(Trigger::Automatic, cx)).is_err() {
                        return;
                    }
                    cx.background_executor().timer(CHECK_INTERVAL).await;
                }
            })
        });
        cx.notify();
    }

    fn check(&mut self, trigger: Trigger, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        self.set_status(UpdateStatus::Checking, cx);
        let updater = self.updater.clone();
        // A version installed since counts as running: only a newer one is news.
        let newer_than = self.installed.clone().unwrap_or_else(|| updater.current().clone());
        let checked = cx.background_spawn(async move { updater.check(&newer_than) });
        self.work = Some(cx.spawn(async move |this, cx| {
            let result = checked.await;
            this.update(cx, |this, cx| this.on_checked(result, trigger, cx)).ok();
        }));
    }

    fn on_checked(&mut self, result: Result<Option<Update>, UpdateError>, trigger: Trigger, cx: &mut Context<Self>) {
        self.work = None;
        let update = match result {
            Ok(Some(update)) => update,
            Ok(None) => return self.set_status(UpdateStatus::UpToDate { checked_at: Utc::now() }, cx),
            Err(error) => {
                log_line!("rushes: update check: {error}");
                return self.set_status(UpdateStatus::Failed(copy::check_failed(&error)), cx);
            }
        };
        let version = update.release.version.clone();
        let page_url = SharedString::from(update.release.page_url.clone());
        match update.delivery {
            Delivery::Install(asset) => self.install(version, page_url, asset, trigger, cx),
            Delivery::Manual(reason) => {
                if trigger == Trigger::Automatic && self.first_notice(&version, Notice::Available) {
                    let (version, page_url, reason) = (version.clone(), page_url.clone(), reason.clone());
                    crate::ui::push_notification(cx, move || copy::available_notification(&version, page_url, &reason));
                }
                self.set_status(UpdateStatus::Available { version, page_url, reason }, cx);
            }
        }
    }

    fn install(
        &mut self,
        version: Version,
        page_url: SharedString,
        asset: Asset,
        trigger: Trigger,
        cx: &mut Context<Self>,
    ) {
        self.set_status(UpdateStatus::Downloading { version: version.clone(), received: 0, total: asset.size }, cx);
        let (sender, mut progress) = mpsc::unbounded();
        let (updater, cancel) = (self.updater.clone(), self.cancel.clone());
        let installed = cx.background_spawn(async move {
            // One message per percent is plenty for the UI.
            let mut last_percent = None;
            updater.install(
                &asset,
                &mut |step| {
                    if let Progress::Downloading { received, total } = step {
                        let percent = received * 100 / total.max(1);
                        if last_percent.replace(percent) == Some(percent) {
                            return;
                        }
                    }
                    _ = sender.unbounded_send(step);
                },
                &cancel,
            )
        });
        self.work = Some(cx.spawn(async move |this, cx| {
            while let Some(step) = progress.next().await {
                let version = version.clone();
                let status = match step {
                    Progress::Downloading { received, total } => UpdateStatus::Downloading { version, received, total },
                    Progress::Installing => UpdateStatus::Installing { version },
                };
                if this.update(cx, |this, cx| this.set_status(status, cx)).is_err() {
                    return;
                }
            }
            let result = installed.await;
            this.update(cx, |this, cx| this.on_installed(version, page_url, result, trigger, cx)).ok();
        }));
    }

    fn on_installed(
        &mut self,
        version: Version,
        page_url: SharedString,
        result: Result<(), UpdateError>,
        trigger: Trigger,
        cx: &mut Context<Self>,
    ) {
        self.work = None;
        match result {
            Ok(()) => {
                log_line!("rushes: installed version {version}; it starts with the next launch");
                self.installed = Some(version.clone());
                if self.first_notice(&version, Notice::Ready) {
                    crate::ui::push_notification(cx, move || copy::ready_notification(&version));
                }
                self.set_status(UpdateStatus::UpToDate { checked_at: Utc::now() }, cx);
            }
            Err(UpdateError::Cancelled) => self.set_status(UpdateStatus::Idle, cx),
            Err(error) => {
                log_line!("rushes: installing version {version}: {error}");
                let failure = copy::install_failed(&version, &error, page_url);
                if trigger == Trigger::Automatic && self.first_notice(&version, Notice::Failed) {
                    let failure = failure.clone();
                    crate::ui::push_notification(cx, move || copy::failure_notification(&failure));
                }
                self.set_status(UpdateStatus::Failed(failure), cx);
            }
        }
    }

    /// Records that the creator is told `notice` about `version`; false if
    /// they were already.
    fn first_notice(&mut self, version: &Version, notice: Notice) -> bool {
        let entry = (version.clone(), notice);
        if self.announced.contains(&entry) {
            return false;
        }
        self.announced.push(entry);
        true
    }

    fn set_status(&mut self, status: UpdateStatus, cx: &mut Context<Self>) {
        self.status = status;
        cx.notify();
    }
}
