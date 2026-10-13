//! What the updater says: the settings status, notifications and failures.

use std::path::Path;

use gpui_kit::SharedString;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::notification::Notification;
use rushes_core::input::display_user_path;
use rushes_update::{Error as UpdateError, ManualReason, Version};

use super::service::{UpdateFailure, UpdateStatus};
use crate::ui::format_when;

/// Title and description of the update status in Settings.
pub(super) fn status(
    status: &UpdateStatus,
    current: &Version,
    installed: Option<&Version>,
    updates_itself: bool,
    automatic: bool,
) -> (String, String) {
    match (status, installed) {
        (UpdateStatus::Checking, _) => ("Checking for updates…".into(), format!("You have version {current}.")),
        (UpdateStatus::Downloading { version, received, total }, _) => (
            format!("Downloading version {version}…"),
            format!("{:.1} of {}", *received as f64 / 1e6, megabytes(*total)),
        ),
        (UpdateStatus::Installing { version }, _) => {
            (format!("Installing version {version}…"), "It starts the next time Rushes opens.".into())
        }
        (_, Some(installed)) => (
            format!("Version {installed} is ready"),
            format!("Restart Rushes to use it. Until then you keep using version {current}."),
        ),
        // The About header already names the current version.
        (UpdateStatus::Idle, None) => {
            let (title, description) = if !updates_itself {
                ("Manual updates", "This copy wasn’t installed from a release, so it doesn’t update itself.")
            } else if automatic {
                ("Automatic updates", "Rushes checks for new versions every few hours.")
            } else {
                ("Automatic updates", "Automatic updates are off.")
            };
            (title.into(), description.into())
        }
        (UpdateStatus::UpToDate { checked_at }, None) => (
            "Rushes is up to date".into(),
            format!("Version {current} · Checked {}", format_when(*checked_at, chrono::Local::now())),
        ),
        (UpdateStatus::Available { version, reason, .. }, None) => {
            (format!("Version {version} is available"), manual_reason(reason))
        }
        (UpdateStatus::Failed(failure), None) => (failure.title.to_string(), failure.message.to_string()),
    }
}

/// Why the creator downloads the new version themselves.
pub(super) fn manual_reason(reason: &ManualReason) -> String {
    match reason {
        ManualReason::NotInstalled => {
            "This copy wasn’t installed from a release, so it can’t update itself. Download the new version to \
             install it."
                .into()
        }
        ManualReason::ReadOnly(folder) if runs_from_disk_image(folder) => {
            "Rushes is running from its disk image, which can’t change. Drag it to Applications and open it \
             from there to get updates."
                .into()
        }
        ManualReason::ReadOnly(folder) => format!(
            "Rushes can’t change {}, so it can’t update itself there. Download the new version to install it.",
            display_user_path(folder)
        ),
        ManualReason::NoFile => "The release has no download for this computer yet.".into(),
    }
}

/// A Mac copy started from the mounted disk image, or from wherever macOS
/// moves a downloaded app it hasn't seen opened from its own folder.
fn runs_from_disk_image(folder: &Path) -> bool {
    folder.starts_with("/Volumes") || folder.components().any(|part| part.as_os_str() == "AppTranslocation")
}

pub(super) fn check_failed(error: &UpdateError) -> UpdateFailure {
    let message = if error.is_rate_limited() {
        "GitHub limits how often one address can check. Try again in an hour."
    } else if error.is_network() {
        "Rushes couldn’t reach GitHub. Check your internet connection and try again."
    } else {
        "GitHub’s answer didn’t make sense to Rushes. Try again later."
    };
    UpdateFailure { title: "Couldn’t check for updates".into(), message: message.into(), page_url: None }
}

pub(super) fn install_failed(version: &Version, error: &UpdateError, page_url: SharedString) -> UpdateFailure {
    let message = match error {
        _ if error.is_permission_denied() => {
            "Rushes can’t change the folder it’s in. Download the new version to install it yourself."
        }
        UpdateError::Http(_) | UpdateError::Status { .. } => {
            "The download stopped. Check your internet connection and try again."
        }
        UpdateError::Corrupt(_) => "The download was damaged. Try again.",
        _ => "Something went wrong putting it in place. Try again, or download it to install it yourself.",
    };
    UpdateFailure {
        title: format!("Couldn’t install version {version}").into(),
        message: message.into(),
        page_url: Some(page_url),
    }
}

pub(super) fn ready_notification(version: &Version) -> Notification {
    Notification::success("Restart Rushes to use it.")
        .title(format!("Version {version} is ready"))
        .id::<UpdateFailure>()
        .action(|_, _, cx| {
            Button::new("update-notification-restart").small().label("Restart").on_click(cx.listener(
                |notification, _, window, cx| {
                    notification.dismiss(window, cx);
                    super::restart(window, cx);
                },
            ))
        })
}

pub(super) fn available_notification(version: &Version, page_url: SharedString, reason: &ManualReason) -> Notification {
    Notification::info(manual_reason(reason))
        .title(format!("Version {version} is available"))
        .id::<UpdateFailure>()
        .action(move |_, _, cx| download_button(page_url.clone(), cx))
}

pub(super) fn failure_notification(failure: &UpdateFailure) -> Notification {
    let notification = Notification::error(failure.message.clone()).title(failure.title.clone()).id::<UpdateFailure>();
    match failure.page_url.clone() {
        Some(page_url) => notification.action(move |_, _, cx| download_button(page_url.clone(), cx)),
        None => notification,
    }
}

pub(super) fn restart_failed(version: Option<&Version>) -> Notification {
    let message = match version {
        Some(version) => format!("Quit Rushes and open it again to use version {version}."),
        None => "Quit Rushes and open it again.".into(),
    };
    Notification::error(message).title("Couldn’t restart Rushes").id::<UpdateFailure>()
}

/// Opens the release page, where the new version is downloaded by hand.
fn download_button(page_url: SharedString, cx: &mut gpui_kit::Context<Notification>) -> Button {
    Button::new("update-notification-download").small().label("Download…").on_click(cx.listener(
        move |notification, _, window, cx| {
            notification.dismiss(window, cx);
            cx.open_url(&page_url);
        },
    ))
}

/// `bytes` in megabytes with one decimal: `36.2 MB`.
fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).expect("version")
    }

    #[test]
    fn the_status_leads_with_a_version_ready_to_start() {
        let current = v("0.1.1");
        let (title, description) = status(&UpdateStatus::Idle, &current, Some(&v("0.1.2")), true, true);
        assert_eq!(title, "Version 0.1.2 is ready");
        assert_eq!(description, "Restart Rushes to use it. Until then you keep using version 0.1.1.");

        // Work in progress still shows over it.
        let downloading = UpdateStatus::Downloading { version: v("0.1.3"), received: 1_250_000, total: 36_171_936 };
        let (title, description) = status(&downloading, &current, Some(&v("0.1.2")), true, true);
        assert_eq!(title, "Downloading version 0.1.3…");
        assert_eq!(description, "1.2 of 36.2 MB");
    }

    #[test]
    fn an_idle_status_says_whether_updates_come_by_themselves() {
        let current = v("0.1.1");
        let idle = |updates_itself, automatic| status(&UpdateStatus::Idle, &current, None, updates_itself, automatic).1;
        assert_eq!(idle(true, true), "Rushes checks for new versions every few hours.");
        assert_eq!(idle(true, false), "Automatic updates are off.");
        assert_eq!(idle(false, true), "This copy wasn’t installed from a release, so it doesn’t update itself.");
    }

    #[test]
    fn a_mac_copy_on_its_disk_image_is_told_to_move_to_applications() {
        let from_image = manual_reason(&ManualReason::ReadOnly(PathBuf::from("/Volumes/Rushes")));
        assert!(from_image.starts_with("Rushes is running from its disk image"), "{from_image}");
        let translocated = PathBuf::from("/private/var/folders/x/T/AppTranslocation/1234/d");
        assert!(manual_reason(&ManualReason::ReadOnly(translocated)).contains("Drag it to Applications"));
        assert_eq!(
            manual_reason(&ManualReason::ReadOnly(PathBuf::from("/opt/rushes"))),
            "Rushes can’t change /opt/rushes, so it can’t update itself there. Download the new version to \
             install it."
        );
    }
}
