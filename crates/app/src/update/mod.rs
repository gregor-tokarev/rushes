//! Updates: [`UpdateService`] keeps VideoCRM up to date with its releases;
//! Settings › About shows where it is ([`settings_group`]), and the title bar
//! offers a restart once a new version is installed ([`restart_button`]).

mod copy;
mod service;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::link::Link;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, Size, WindowExt as _, h_flex};
use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _,
    TestSupportExt as _, Window, div,
};

use crate::ai_edit::PipelineService;
use crate::publish::YoutubeService;
use crate::state::AppState;
use crate::ui::settings::setting_group;

pub use service::{UpdateFailure, UpdateService, UpdateStatus};

/// Restarts into the installed version, first asking when an upload or an
/// AI edit is running, which the restart stops.
pub fn restart(window: &mut Window, cx: &mut App) {
    let Some(service) = UpdateService::try_global(cx) else { return };
    let uploads = YoutubeService::try_global(cx).map_or(0, |youtube| youtube.read(cx).active_upload_count());
    let edits = PipelineService::try_global(cx).map_or(0, |pipelines| pipelines.read(cx).live_run_count());
    let Some(running) = running_work(uploads, edits) else {
        service.update(cx, |service, cx| service.restart(cx));
        return;
    };
    window.open_alert_dialog(cx, move |alert, _, _| {
        let service = service.clone();
        alert.title("Restart VideoCRM now?").description(running.clone()).confirm().ok_text("Restart").on_ok(
            move |_, _, cx| {
                service.update(cx, |service, cx| service.restart(cx));
                true
            },
        )
    })
}

/// What a restart would stop: `An upload to YouTube and an AI edit are
/// running. Restarting stops them.`
fn running_work(uploads: usize, edits: usize) -> Option<String> {
    let count = |count: usize, one: &str, many: &str| match count {
        0 => None,
        1 => Some(one.to_owned()),
        count => Some(format!("{count} {many}")),
    };
    let parts: Vec<String> =
        [count(uploads, "an upload to YouTube", "uploads to YouTube"), count(edits, "an AI edit", "AI edits")]
            .into_iter()
            .flatten()
            .collect();
    if parts.is_empty() {
        return None;
    }
    let running = parts.join(" and ");
    let (verb, them) = if uploads + edits == 1 { ("is", "it") } else { ("are", "them") };
    let mut sentence = running.chars();
    let first = sentence.next().map(|first| first.to_uppercase().collect::<String>()).unwrap_or_default();
    Some(format!("{first}{} {verb} running. Restarting stops {them}.", sentence.as_str()))
}

/// The title bar's way to the installed version, once there is one.
pub fn restart_button(cx: &App) -> Option<Button> {
    let version = UpdateService::try_global(cx)?.read(cx).installed()?.clone();
    Some(
        Button::new("restart-to-update")
            .outline()
            .small()
            .icon(IconName::RotateCw)
            .label("Restart to update")
            .tooltip(format!("Version {version} is installed"))
            .on_click(|_, window, cx| restart(window, cx)),
    )
}

/// The Updates group of Settings › About: where updating stands with its next
/// step, and whether new versions install by themselves. The page re-renders
/// when the service notifies.
pub fn settings_group(cx: &App) -> Option<SettingGroup> {
    let service = UpdateService::try_global(cx)?;
    let read = service.read(cx);
    let automatic = AppState::global(cx).read(cx).settings().updates.automatic;
    let (title, description) =
        copy::status(read.status(), read.current(), read.installed(), read.updates_itself(), automatic);
    let status =
        SettingItem::new(title, SettingField::render(move |options, _, cx| status_control(options.size(), cx)))
            .description(description)
            .keywords(["update", "upgrade", "version", "release"]);
    let mut group = setting_group().title("Updates").item(status);
    if read.updates_itself() {
        group = group.item(
            SettingItem::new(
                "Update automatically",
                SettingField::switch(
                    |cx: &App| AppState::global(cx).read(cx).settings().updates.automatic,
                    |on: bool, cx: &mut App| {
                        AppState::global(cx).update(cx, |state, cx| {
                            state.update_settings(cx, |settings| settings.updates.automatic = on)
                        })
                    },
                ),
            )
            .description("Download new versions in the background. They start the next time VideoCRM opens.")
            .keywords(["update", "upgrade", "automatic"]),
        );
    }
    Some(group)
}

/// The next step for the status: check, follow the download, restart, or
/// download by hand.
fn status_control(size: Size, cx: &mut App) -> impl IntoElement + use<> {
    let service = UpdateService::global(cx);
    let read = service.read(cx);
    let check = |label: &'static str, busy: bool| {
        let service = service.clone();
        Button::new("check-for-updates")
            .with_size(size)
            .label(label)
            .loading(busy)
            .disabled(busy)
            .on_click(move |_, _, cx| service.update(cx, |service, cx| service.check_now(cx)))
            .into_any_element()
    };
    let controls = match (read.status(), read.installed()) {
        (UpdateStatus::Checking, _) => vec![check("Check for updates", true)],
        (UpdateStatus::Downloading { received, total, .. }, _) => {
            let percent = (*received as f32 / (*total).max(1) as f32) * 100.;
            vec![progress_bar(Progress::new("update-progress").value(percent))]
        }
        (UpdateStatus::Installing { .. }, _) => vec![progress_bar(Progress::new("update-progress").loading(true))],
        (_, Some(_)) => vec![
            Button::new("restart-to-update-settings")
                .primary()
                .with_size(size)
                .label("Restart")
                .on_click(|_, window, cx| restart(window, cx))
                .into_any_element(),
        ],
        (UpdateStatus::Available { page_url, .. }, None) => vec![download_link(page_url.clone())],
        (UpdateStatus::Failed(failure), None) => {
            let mut controls = vec![check("Try again", false)];
            controls.extend(failure.page_url.clone().map(download_link));
            controls
        }
        (UpdateStatus::Idle | UpdateStatus::UpToDate { .. }, None) => vec![check("Check for updates", false)],
    };
    h_flex().gap_3().children(controls)
}

fn progress_bar(progress: Progress) -> AnyElement {
    div().w_32().child(progress.accessibility_label("Update progress")).into_any_element()
}

/// The release page, where the new version is downloaded by hand.
fn download_link(page_url: SharedString) -> AnyElement {
    div()
        .id("update-download")
        .child(
            Link::new("update-download-link")
                .href(page_url)
                .child(h_flex().gap_1().child("Download").child(Icon::new(IconName::ExternalLink).xsmall())),
        )
        // Links are not observable on their own; tests find the link through this frame.
        .test_support()
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::running_work;

    #[test]
    fn a_restart_names_the_work_it_stops() {
        assert_eq!(running_work(0, 0), None);
        assert_eq!(running_work(1, 0).as_deref(), Some("An upload to YouTube is running. Restarting stops it."));
        assert_eq!(running_work(0, 1).as_deref(), Some("An AI edit is running. Restarting stops it."));
        assert_eq!(
            running_work(1, 1).as_deref(),
            Some("An upload to YouTube and an AI edit are running. Restarting stops them.")
        );
        assert_eq!(
            running_work(2, 3).as_deref(),
            Some("2 uploads to YouTube and 3 AI edits are running. Restarting stops them.")
        );
    }
}
