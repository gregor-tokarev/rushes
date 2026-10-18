//! The dialogs of A/B tests: setting one up (what it compares, the versions, how
//! long) and ending one (which version stays on YouTube).

use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::{ActiveTheme as _, Side, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, ExternalPaths, FontWeight, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, StyledImage as _, TestSupportExt as _,
    Window, div, img, rems,
};
use rushes_core::VideoId;
use rushes_core::ab_test::{
    DEFAULT_TEST_DAYS, MAX_VERSIONS, MIN_VERSIONS, TEST_LENGTHS, TestSubject, TestVersion, version_letter,
};

use super::ab_test::{AbTests, version_problems};
use super::rules::TITLE_MAX_CHARS;
use super::ui::{FieldRow, file_name, help_text};
use crate::ui::dialog::form_footer;
use crate::ui::{menu_item, notification, scrolling_menu};

/// Asks what to test on `video_id` and starts the test. `title` is the video's title
/// on YouTube, which version A starts with; `images` are the thumbnails the video has
/// (the one on YouTube first), to pick versions from.
pub(super) fn open_setup_dialog(
    video_id: VideoId,
    title: String,
    images: Vec<PathBuf>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(runner) = AbTests::try_global(cx) else { return };
    let form = cx.new(|cx| SetupForm::new(title, images, window, cx));
    // A is the video as it is; B is what the creator came to write.
    let second = form.read(cx).versions[1].title.clone();
    window.open_dialog(cx, move |dialog, window, _| {
        let (form, runner) = (form.clone(), runner.clone());
        dialog
            .title("Set up an A/B test")
            .width(window.rem_size() * 38.)
            .child(form.clone())
            .footer(form_footer("ab-setup-cancel", "ab-setup-start", "Start test"))
            .on_ok(move |_, _, cx| {
                let (subject, versions, days) = form.read(cx).test(cx);
                let problems =
                    version_problems(subject, &versions, |path| std::fs::metadata(path).ok().map(|m| m.len()));
                let started = if problems.is_empty() {
                    runner
                        .update(cx, |runner, cx| runner.start(video_id, subject, versions, days, cx))
                        .map_err(|p| vec![p])
                } else {
                    Err(problems)
                };
                match started {
                    Ok(()) => true,
                    // The form says why, under the versions, and stays open.
                    Err(problems) => {
                        form.update(cx, |form, cx| form.show_problems(problems, cx));
                        false
                    }
                }
            })
    });
    second.update(cx, |input, cx| input.focus(window, cx));
}

/// One version as the form edits it.
struct VersionDraft {
    title: Entity<InputState>,
    thumbnail: Option<PathBuf>,
}

/// The setup dialog's content.
struct SetupForm {
    subject: TestSubject,
    versions: Vec<VersionDraft>,
    days: u32,
    /// Thumbnails to choose versions from.
    images: Vec<PathBuf>,
    /// What stopped the last Start, shown until the form changes.
    problems: Vec<String>,
}

impl SetupForm {
    fn new(title: String, images: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut form = Self {
            subject: TestSubject::Title,
            versions: Vec::new(),
            days: DEFAULT_TEST_DAYS,
            images,
            problems: Vec::new(),
        };
        // A starts as the video is now; B is the new idea.
        let first = form.images.first().cloned();
        form.add_version(Some(title), first, window, cx);
        form.add_version(None, None, window, cx);
        form
    }

    fn add_version(
        &mut self,
        title: Option<String>,
        thumbnail: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let letter = version_letter(self.versions.len());
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(format!("Title of version {letter}"))
                .default_value(title.unwrap_or_default())
        });
        self.versions.push(VersionDraft { title: input, thumbnail });
        self.problems.clear();
        cx.notify();
    }

    fn remove_version(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.versions.len() > MIN_VERSIONS && ix < self.versions.len() {
            self.versions.remove(ix);
            self.problems.clear();
            cx.notify();
        }
    }

    fn set_thumbnail(&mut self, ix: usize, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(version) = self.versions.get_mut(ix) {
            version.thumbnail = Some(path);
            self.problems.clear();
            cx.notify();
        }
    }

    /// Asks the system for an image for version `ix`.
    fn choose_file(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            match paths.await {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.update(cx, |this, cx| this.set_thumbnail(ix, path, cx)).ok();
                    }
                }
                Ok(Ok(None)) | Err(_) => {}
                // Linux needs the desktop portal for file dialogs; dropping still works.
                Ok(Err(error)) => {
                    log_line!("rushes: file chooser failed: {error}");
                    this.update_in(cx, |_, window, cx| {
                        let problem = "Couldn’t open the file chooser. Drop the image on the version instead.";
                        window.push_notification(notification::warning(problem, cx), cx);
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn show_problems(&mut self, problems: Vec<String>, cx: &mut Context<Self>) {
        self.problems = problems;
        cx.notify();
    }

    /// The test as the form describes it.
    fn test(&self, cx: &App) -> (TestSubject, Vec<TestVersion>, u32) {
        let versions = self
            .versions
            .iter()
            .map(|draft| TestVersion {
                title: self.subject.has_title().then(|| draft.title.read(cx).value().trim().to_owned()),
                thumbnail: draft.thumbnail.clone().filter(|_| self.subject.has_thumbnail()),
            })
            .collect();
        (self.subject, versions, self.days)
    }

    fn render_version(&self, ix: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let draft = &self.versions[ix];
        let theme = cx.theme();
        let letter = version_letter(ix);
        let images = self.images.clone();
        let chosen = draft.thumbnail.clone();
        let form = cx.weak_entity();
        let name = chosen.as_deref().map_or_else(|| "No image yet".to_owned(), file_name);
        let thumbnail = self.subject.has_thumbnail().then(|| {
            let preview = match &chosen {
                // A rounded parent does not clip its children, so the image rounds itself.
                Some(path) => img(path.clone())
                    .w(rems(7.))
                    .h(rems(7. * 9. / 16.))
                    .rounded(theme.radius)
                    .object_fit(ObjectFit::Cover)
                    .into_any_element(),
                None => div()
                    .w(rems(7.))
                    .h(rems(7. * 9. / 16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Choose image")
                    .into_any_element(),
            };
            let (drop_border, drop_fill) = (theme.primary, theme.drop_target);
            div()
                .id(("ab-version-drop", ix))
                .rounded(theme.radius)
                .drag_over::<ExternalPaths>(move |style, _, _, _| style.border_color(drop_border).bg(drop_fill))
                .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                    if let Some(path) = paths.paths().first() {
                        this.set_thumbnail(ix, path.clone(), cx);
                    }
                }))
                .child(
                    Button::new(("ab-version-thumbnail", ix))
                        .outline()
                        .p_1()
                        .h_auto()
                        .accessibility_label(format!("Thumbnail of version {letter}"))
                        .child(preview)
                        .dropdown_menu(move |menu, window, _| {
                            let mut menu = scrolling_menu(menu, window).check_side(Side::Right);
                            for path in &images {
                                let (form, path) = (form.clone(), path.clone());
                                menu = menu.item(
                                    menu_item(file_name(&path))
                                        .icon(IconName::Image)
                                        .checked(chosen.as_ref() == Some(&path))
                                        .on_click(move |_, _, cx| {
                                            form.update(cx, |form, cx| form.set_thumbnail(ix, path.clone(), cx)).ok();
                                        }),
                                );
                            }
                            if !images.is_empty() {
                                menu = menu.separator();
                            }
                            let form = form.clone();
                            menu.item(menu_item("Choose file…").icon(IconName::FolderOpen).on_click(
                                move |_, window, cx| {
                                    form.update(cx, |form, cx| form.choose_file(ix, window, cx)).ok();
                                },
                            ))
                        }),
                )
                // Tests drop images here.
                .test_support()
        });
        let removable = ix >= MIN_VERSIONS;
        h_flex()
            .id(("ab-version", ix))
            .gap_3()
            .items_center()
            .child(
                div()
                    .flex_shrink_0()
                    .size_7()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(letter.to_string()),
            )
            .when(self.subject.has_title(), |this| {
                this.child(
                    div().flex_1().min_w_0().child(
                        Input::new(&draft.title)
                            .id(("ab-version-title", ix))
                            .aria_label(format!("Title of version {letter}")),
                    ),
                )
            })
            .children(thumbnail)
            .when(!self.subject.has_title(), |this| {
                this.child(div().flex_1().min_w_0().text_sm().text_color(theme.muted_foreground).truncate().child(name))
            })
            .child(div().flex_shrink_0().w_7().when(removable, |this| {
                this.child(
                    Button::new(("ab-remove-version", ix))
                        .icon(IconName::X)
                        .ghost()
                        .small()
                        .accessibility_label(format!("Remove version {letter}"))
                        .on_click(cx.listener(move |this, _, _, cx| this.remove_version(ix, cx))),
                )
            }))
    }
}

impl Render for SetupForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let subjects: Vec<SharedString> = TestSubject::ALL.iter().map(|subject| subject.label().into()).collect();
        let lengths: Vec<SharedString> = TEST_LENGTHS
            .iter()
            .map(|days| if days % 7 == 0 { weeks(days / 7) } else { format!("{days} days") }.into())
            .collect();
        let subject_ix = TestSubject::ALL.iter().position(|subject| *subject == self.subject).unwrap_or(0);
        let length_ix = TEST_LENGTHS.iter().position(|days| *days == self.days);
        let rows: Vec<_> = (0..self.versions.len()).map(|ix| self.render_version(ix, cx)).collect();
        let can_add = self.versions.len() < MAX_VERSIONS;
        let problems = self.problems.clone();
        let theme = cx.theme();
        v_flex()
            .id("ab-setup")
            .gap_5()
            .child(
                help_text(
                    "Rushes puts one version on YouTube at a time, each for a day, switching at midnight Pacific \
                     time. When the test ends it keeps the version that earned the most watch time per \
                     impression, or A when none did clearly better.",
                    cx,
                )
                .text_sm(),
            )
            .child(
                FieldRow::new("Test").control(
                    RadioGroup::horizontal("ab-setup-subject")
                        .children(subjects)
                        .selected_index(Some(subject_ix))
                        .on_change(cx.listener(|this, ix: &usize, _, cx| {
                            this.subject = TestSubject::ALL[*ix];
                            this.problems.clear();
                            cx.notify();
                        })),
                ),
            )
            .child(
                FieldRow::new("Versions")
                    .help(match self.subject {
                        TestSubject::Title => {
                            format!("Up to {TITLE_MAX_CHARS} characters each. A starts with the title the video has.")
                        }
                        TestSubject::Thumbnail => "JPG or PNG images up to 2 MB each: the video’s thumbnails, or \
                             one dropped on a version. A starts with the one on YouTube."
                            .to_owned(),
                        TestSubject::Both => format!(
                            "Titles up to {TITLE_MAX_CHARS} characters, JPG or PNG images up to 2 MB. A starts as \
                             the video is on YouTube."
                        ),
                    })
                    .control(v_flex().gap_2().children(rows).when(can_add, |this| {
                        this.child(
                            h_flex().child(
                                Button::new("ab-add-version")
                                    .label("Add version")
                                    .icon(IconName::Plus)
                                    .small()
                                    .ghost()
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.add_version(None, None, window, cx)),
                                    ),
                            ),
                        )
                    })),
            )
            .child(FieldRow::new("Length").control(
                RadioGroup::horizontal("ab-setup-length").children(lengths).selected_index(length_ix).on_change(
                    cx.listener(|this, ix: &usize, _, cx| {
                        this.days = TEST_LENGTHS[*ix];
                        cx.notify();
                    }),
                ),
            ))
            .when(!problems.is_empty(), |this| {
                this.child(
                    v_flex()
                        .id("ab-setup-problems")
                        .gap_1()
                        .text_sm()
                        .text_color(theme.danger)
                        .children(problems.into_iter().map(|problem| div().child(problem)))
                        // Tests check what stops the test.
                        .test_support(),
                )
            })
    }
}

fn weeks(count: u32) -> String {
    if count == 1 { "1 week".to_owned() } else { format!("{count} weeks") }
}

/// Asks which version stays on YouTube, then ends `video_id`'s test. `leader` is
/// preselected: the version the numbers favour, else the one on YouTube.
pub(super) fn open_end_dialog(
    video_id: VideoId,
    labels: Vec<String>,
    leader: usize,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(runner) = AbTests::try_global(cx) else { return };
    let form = cx.new(|_| EndForm { labels, keep: leader });
    window.open_dialog(cx, move |dialog, window, _| {
        let (form, runner) = (form.clone(), runner.clone());
        dialog
            .title("End the A/B test?")
            .width(window.rem_size() * 30.)
            .child(form.clone())
            .footer(form_footer("ab-end-cancel", "ab-end-commit", "End test"))
            .on_ok(move |_, _, cx| {
                let keep = form.read(cx).keep;
                runner.update(cx, |runner, cx| runner.end(video_id, keep, cx));
                true
            })
    });
}

/// The end dialog's content.
struct EndForm {
    /// "A · How I bake bread", one per version.
    labels: Vec<String>,
    keep: usize,
}

impl Render for EndForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let labels: Vec<SharedString> = self.labels.iter().cloned().map(Into::into).collect();
        v_flex()
            .gap_4()
            .child(help_text("The versions stop taking turns. The one you keep stays on YouTube.", cx).text_sm())
            .child(FieldRow::new("Keep").control(
                RadioGroup::vertical("ab-end-keep").children(labels).selected_index(Some(self.keep)).on_change(
                    cx.listener(|this, ix: &usize, _, cx| {
                        this.keep = *ix;
                        cx.notify();
                    }),
                ),
            ))
    }
}
