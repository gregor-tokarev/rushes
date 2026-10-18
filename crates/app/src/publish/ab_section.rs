//! The A/B test section of a Publish tab, for a video on YouTube: setting up a
//! test, how the versions do while it runs, and how it ended.

use std::path::PathBuf;

use chrono::{DateTime, Local, Utc};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Div, Entity, FontWeight, InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _, TestSupportExt as _,
    Window, div, img, rems,
};
use rushes_core::ab_test::{AbTest, StopReason, TestOutcome, TestSubject, VersionResult, version_letter};
use rushes_core::{Video, VideoId};

use super::ab_dialogs::{open_end_dialog, open_setup_dialog};
use super::ab_test::AbTests;
use super::rules::group_thousands;
use super::service::{TestAccess, YoutubeService};
use super::ui::{help_text, section_heading, sign_in_progress, status_line};
use crate::state::AppState;
use crate::ui::state_tooltip;

/// The section, while the video is on YouTube and the app runs tests.
#[derive(IntoElement)]
pub(super) struct TestSection {
    video_id: VideoId,
    /// Thumbnails the video has, for versions of a test: the one on YouTube first.
    images: Vec<PathBuf>,
}

impl TestSection {
    pub(super) fn new(video_id: VideoId, images: Vec<PathBuf>) -> Self {
        Self { video_id, images }
    }

    /// Whether the section has anything to show for `video`.
    pub(super) fn applies(video: &Video, cx: &App) -> bool {
        video.youtube.is_some() && AbTests::try_global(cx).is_some()
    }
}

impl RenderOnce for TestSection {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let (Some(runner), Some(service)) = (AbTests::try_global(cx), YoutubeService::try_global(cx)) else {
            return div().into_any_element();
        };
        let Some(video) = AppState::global(cx).read(cx).video(self.video_id).cloned() else {
            return div().into_any_element();
        };
        let Some(link) = video.youtube.clone() else { return div().into_any_element() };
        let now = runner.read(cx).now();
        let test = link.ab_test.clone();
        let running = test.as_ref().is_some_and(|test| !test.is_over());
        let access = service.read(cx).test_access(&link.channel_id);
        let switching = runner.read(cx).switching(self.video_id);

        let action = if let Some(test) = test.clone().filter(|test| !test.is_over()) {
            let reason = switching
                .map(|version| SharedString::from(format!("Wait until {} is on YouTube.", version_letter(version))));
            let video_id = self.video_id;
            let button =
                Button::new("ab-end").label("End test…").small().outline().disabled(reason.is_some()).on_click(
                    move |_, window, cx| {
                        let verdict = test.verdict(now);
                        let leader = verdict.leader.or(test.live_version()).unwrap_or(0);
                        open_end_dialog(video_id, version_labels(&test), leader, window, cx);
                    },
                );
            state_tooltip("ab-end-reason", reason, button)
        } else {
            let availability = runner.read(cx).availability(&video, cx);
            let reason = match (availability, access) {
                (Err(reason), _) => Some(SharedString::from(reason)),
                (Ok(()), TestAccess::Allowed) => None,
                (Ok(()), _) => Some("Allow access first.".into()),
            };
            let video_id = self.video_id;
            // The images of the last test can go into the next one.
            let mut images = self.images.clone();
            for image in test.iter().flat_map(|test| &test.versions).filter_map(|version| version.thumbnail.clone()) {
                if !images.contains(&image) {
                    images.push(image);
                }
            }
            let title = title_on_youtube(&video, test.as_ref(), &service, cx);
            let button = Button::new("ab-set-up")
                .label(if test.is_some() { "New test…" } else { "Set up test…" })
                .small()
                .outline()
                .disabled(reason.is_some())
                .on_click(move |_, window, cx| {
                    open_setup_dialog(video_id, title.clone(), images.clone(), window, cx);
                });
            state_tooltip("ab-set-up-reason", reason, button)
        };

        let heading = section_heading("A/B test").action(action).description(
            "Rushes shows each version on YouTube for a day at a time and keeps the one viewers watch most.",
        );
        let permission = (!running || access != TestAccess::Allowed)
            .then(|| render_access(&service, &link.channel_id, access, cx))
            .flatten();
        let body = test.map(|test| render_test(self.video_id, &test, &runner, now, cx));
        v_flex()
            .id("ab-test")
            .gap_4()
            .child(heading)
            .children(permission)
            .children(body)
            // Tests check what the section says.
            .test_support()
            .into_any_element()
    }
}

/// The title the video has on YouTube, as far as Rushes knows: what the last test
/// left there, else what YouTube last said, else the title it was uploaded with.
fn title_on_youtube(video: &Video, test: Option<&AbTest>, service: &Entity<YoutubeService>, cx: &App) -> String {
    if let Some(test) = test {
        if let Some(TestOutcome::Stopped { reason: StopReason::TitleChanged { title }, .. }) = &test.outcome {
            return title.clone();
        }
        if test.subject.has_title()
            && let Some(title) = test.live_title()
        {
            return title.to_owned();
        }
    }
    let link = video.youtube.as_ref();
    link.and_then(|link| service.read(cx).youtube_video(&link.video_id))
        .map_or_else(|| video.upload_title().to_owned(), |info| info.title.clone())
}

/// What tests need from the account `key` and the way to get it; `None` when they
/// have it, or when the account card already asks for a sign-in.
fn render_access(service: &Entity<YoutubeService>, key: &str, access: TestAccess, cx: &App) -> Option<AnyElement> {
    let youtube = service.read(cx);
    let theme = cx.theme();
    let name = youtube.find_account(key).map_or_else(|| "the account".to_owned(), |account| account.name());
    let element =
        match access {
            TestAccess::Allowed | TestAccess::NeedsSignIn | TestAccess::NotConnected => return None,
            TestAccess::SigningIn if youtube.is_allowing_tests() => {
                sign_in_progress(service, youtube.sign_in_url().cloned(), cx).into_any_element()
            }
            TestAccess::SigningIn => return None,
            TestAccess::NeedsPermission => {
                let busy = youtube.is_signing_in();
                let (service, key) = (service.clone(), key.to_owned());
                v_flex()
                    .gap_3()
                    .child(status_line(
                        IconName::KeyRound,
                        theme.muted_foreground,
                        "Allow A/B tests",
                        Some(
                            format!(
                                "Google asks you to let Rushes change the titles of {name}’s videos and read their \
                             analytics. Uploads work either way."
                            )
                            .into(),
                        ),
                        cx,
                    ))
                    .child(
                        h_flex().child(
                            Button::new("ab-allow").label("Allow access…").small().outline().disabled(busy).on_click(
                                move |_, _, cx| service.update(cx, |service, cx| service.allow_tests(&key, cx)),
                            ),
                        ),
                    )
                    .into_any_element()
            }
        };
    Some(div().id("ab-access").child(element).test_support().into_any_element())
}

/// A running test's state, results and problems, or how an ended one ended.
fn render_test(video_id: VideoId, test: &AbTest, runner: &Entity<AbTests>, now: DateTime<Utc>, cx: &App) -> Div {
    let theme = cx.theme();
    let runner = runner.read(cx);
    let at = test.outcome.as_ref().map_or(now, TestOutcome::at);
    let results = test.results(at);
    let verdict = test.verdict(at);
    let mut lines: Vec<AnyElement> = Vec::new();
    match &test.outcome {
        None => {
            lines.push(
                help_text(progress_text(test, now), cx).text_sm().id("ab-progress").test_support().into_any_element(),
            );
            if let Some(version) = runner.switching(video_id) {
                lines.push(
                    h_flex()
                        .gap_2()
                        .child(Spinner::new().small().color(theme.muted_foreground))
                        .child(div().text_sm().child(format!("Putting {} on YouTube…", version_letter(version))))
                        .into_any_element(),
                );
            }
            if let Some((version, problem)) = runner.switch_problem(video_id) {
                let due = format!("Couldn’t put {} on YouTube", version_letter(version));
                lines.push(
                    div()
                        .id("ab-switch-problem")
                        .child(status_line(
                            IconName::TriangleAlert,
                            theme.warning,
                            due,
                            Some(format!("{problem} Rushes tries again in a few minutes.").into()),
                            cx,
                        ))
                        .test_support()
                        .into_any_element(),
                );
            }
            if let Some(problem) = runner.numbers_problem(video_id) {
                lines.push(
                    div()
                        .id("ab-numbers-problem")
                        .child(status_line(
                            IconName::TriangleAlert,
                            theme.warning,
                            "Couldn’t read the numbers",
                            Some(problem.to_owned().into()),
                            cx,
                        ))
                        .test_support()
                        .into_any_element(),
                );
            }
        }
        Some(TestOutcome::Finished { kept, winner, chosen, at }) => {
            let letter = version_letter(*kept);
            let (title, detail) = match (winner, chosen) {
                (Some(winner), _) if winner == kept => (
                    format!("{letter} won · it’s on YouTube"),
                    format!("It earned {} more watch time per impression than the next best.", percent(verdict.lift)),
                ),
                (_, true) => {
                    (format!("Ended · {letter} stays on YouTube"), format!("You ended the test on {}.", local_day(*at)))
                }
                _ => (
                    format!("No clear winner · {letter} stays on YouTube"),
                    "No version did clearly better than the others.".to_owned(),
                ),
            };
            lines.push(
                div()
                    .id("ab-outcome")
                    .child(status_line(IconName::CircleCheck, theme.muted_foreground, title, Some(detail.into()), cx))
                    .test_support()
                    .into_any_element(),
            );
        }
        Some(TestOutcome::Stopped { reason, at }) => {
            let detail = match reason {
                StopReason::TitleChanged { title } => format!(
                    "On {} the title was changed on YouTube to “{title}”, so Rushes left it as it is.",
                    local_day(*at)
                ),
                StopReason::Cancelled => format!("You stopped it on {}.", local_day(*at)),
            };
            lines.push(
                div()
                    .id("ab-outcome")
                    .child(status_line(IconName::TriangleAlert, theme.warning, "Test stopped", Some(detail.into()), cx))
                    .test_support()
                    .into_any_element(),
            );
        }
    }
    let kept = match &test.outcome {
        Some(TestOutcome::Finished { kept, .. }) => Some(*kept),
        _ => None,
    };
    let live = if test.is_over() { None } else { test.live_version() };
    let table = render_results(test, &results, verdict.leader, live, kept, cx);
    let verdict_text = match (&test.outcome, verdict.leader) {
        (Some(_), _) => None,
        (None, None) => {
            Some("Not enough numbers to compare yet: each version needs two days that YouTube has reported.".to_owned())
        }
        (None, Some(leader)) if verdict.clear => Some(format!(
            "{} leads by {}, more than chance would explain.",
            version_letter(leader),
            percent(verdict.lift)
        )),
        (None, Some(leader)) => {
            Some(format!("{} leads by {} so far: not clear yet.", version_letter(leader), percent(verdict.lift)))
        }
    };
    v_flex()
        .gap_3()
        .children(lines)
        .child(table)
        .when_some(verdict_text, |this, text| this.child(div().id("ab-verdict").text_sm().child(text).test_support()))
        .when(!test.is_over(), |this| {
            this.child(help_text(
                "YouTube reports each day’s numbers about two days later. Rushes switches versions at midnight \
                 Pacific time while it’s open; a day it switches late is shared by the hours each version was up.",
                cx,
            ))
        })
}

/// "Testing titles · day 3 of 14 · ends Mon, 2 Nov".
fn progress_text(test: &AbTest, now: DateTime<Utc>) -> String {
    let what = match test.subject {
        TestSubject::Title => "Testing titles",
        TestSubject::Thumbnail => "Testing thumbnails",
        TestSubject::Both => "Testing titles and thumbnails",
    };
    if now < test.starts_at {
        return format!(
            "{what} · starts when the video goes public on {}; A is on YouTube until then",
            test.starts_at.with_timezone(&Local).format("%a, %-d %b at %H:%M")
        );
    }
    if now >= test.ends_at() {
        return format!("{what} · the last day is over; Rushes decides once YouTube reports it");
    }
    let day = ((now - test.starts_at).num_hours() / 24 + 1).min(i64::from(test.days));
    format!("{what} · day {day} of {} · ends {}", test.days, local_day(test.ends_at()))
}

fn local_day(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local).format("%a, %-d %b").to_string()
}

fn percent(fraction: f64) -> String {
    if fraction.is_finite() { format!("{:.0}%", fraction * 100.0) } else { "far".to_owned() }
}

/// "A · How I bake bread", or the image's file name for a thumbnail test.
fn version_labels(test: &AbTest) -> Vec<String> {
    test.versions
        .iter()
        .enumerate()
        .map(|(ix, version)| {
            let what = version
                .title
                .clone()
                .or_else(|| version.thumbnail.as_deref().map(super::ui::file_name))
                .unwrap_or_default();
            format!("{} · {what}", version_letter(ix))
        })
        .collect()
}

/// Widths of the number columns.
const DAYS_WIDTH: f32 = 3.5;
const IMPRESSIONS_WIDTH: f32 = 6.;
const RATE_WIDTH: f32 = 5.;
const SCORE_WIDTH: f32 = 7.;
/// The narrowest the table gets before it scrolls sideways.
const TABLE_MIN_WIDTH: f32 = 34.;

/// The versions side by side: what each is and how it did.
fn render_results(
    test: &AbTest,
    results: &[VersionResult],
    leader: Option<usize>,
    live: Option<usize>,
    kept: Option<usize>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let number = |width: f32| div().flex_shrink_0().w(rems(width)).text_right();
    // Labels that wrap keep the one bottom line of the others.
    let header = h_flex()
        .items_end()
        .gap_4()
        .py_1p5()
        .border_b_1()
        .border_color(theme.border)
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(div().flex_1().min_w_0().child("Version"))
        .child(number(DAYS_WIDTH).child("Days"))
        .child(number(IMPRESSIONS_WIDTH).child("Impressions"))
        .child(number(RATE_WIDTH).child("Click rate"))
        .child(number(SCORE_WIDTH).child("Watch time per impression"));
    let rows = test.versions.iter().zip(results).enumerate().map(|(ix, (version, result))| {
        let tag = if live == Some(ix) {
            Some("On YouTube")
        } else if kept == Some(ix) {
            Some("Kept")
        } else {
            None
        };
        let leading = leader == Some(ix);
        let identity = h_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .child(
                div()
                    .flex_shrink_0()
                    .size_6()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(version_letter(ix).to_string()),
            )
            .when_some(version.thumbnail.clone().filter(|_| test.subject.has_thumbnail()), |this, image| {
                this.child(
                    img(image)
                        .flex_shrink_0()
                        .w(rems(4.5))
                        .h(rems(4.5 * 9. / 16.))
                        .rounded(theme.radius)
                        .object_fit(ObjectFit::Cover),
                )
            })
            .child(
                v_flex()
                    .min_w_0()
                    .gap_1()
                    .items_start()
                    .child(div().text_sm().line_clamp(2).child(match (&version.title, &version.thumbnail) {
                        (Some(title), _) if test.subject.has_title() => title.clone(),
                        (_, Some(image)) => super::ui::file_name(image),
                        _ => String::new(),
                    }))
                    .when_some(tag, |this, tag| this.child(Tag::secondary().small().child(tag))),
            );
        let value = |text: String| div().child(text);
        h_flex()
            .id(("ab-result", ix))
            .gap_4()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .text_sm()
            .child(identity)
            .child(number(DAYS_WIDTH).child(value(format!("{:.1}", result.days_shown))))
            .child(number(IMPRESSIONS_WIDTH).child(value(if result.days_measured > 0.0 {
                group_thousands(result.impressions.round() as usize)
            } else {
                "—".to_owned()
            })))
            .child(number(RATE_WIDTH).child(value(
                result.click_rate().map_or_else(|| "—".to_owned(), |rate| format!("{:.1}%", rate * 100.0)),
            )))
            .child(
                number(SCORE_WIDTH)
                    .when(leading, |this| this.font_weight(FontWeight::SEMIBOLD))
                    .child(value(result.score().map_or_else(|| "—".to_owned(), |score| format!("{score:.1} s")))),
            )
            // Tests read the numbers.
            .test_support()
    });
    div()
        .id("ab-results")
        .w_full()
        .overflow_x_scroll()
        .child(v_flex().min_w(rems(TABLE_MIN_WIDTH)).child(header).children(rows))
        .into_any_element()
}
