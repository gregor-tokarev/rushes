//! [`AiSettings`]: the AI editing section of the Settings page.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Div, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, Rems, Render, SharedString, Styled as _, Subscription, Task,
    TestSupportExt as _, Window, div,
};
use rushes_core::input::{PathInputError, display_user_path, expand_user_path};
use rushes_core::settings::{
    AgentHarness, DEFAULT_AGENT_COMMAND, DEFAULT_CODEX_COMMAND, EditHarness, PipelineSettings, data_dir,
    read_elevenlabs_key, write_elevenlabs_key,
};
use rushes_pipeline::default_skill_dir;

use super::brief::{COLOR_GRADES, color_grade_label};
use super::doctor::DoctorChecklist;
use super::service::PipelineService;
use crate::state::{AppEvent, AppState};
use crate::ui::menu_item;
use crate::ui::path_prompt::{self, PathKind, PathRequest};
use crate::ui::settings::{SettingsField, SettingsSection};

/// How long typing pauses before text fields are saved.
const SAVE_DELAY: Duration = Duration::from_millis(600);

/// Width of a short text field beside its label. The label takes up to three fifths
/// of the row, which leaves about this much beside it at the smallest window.
const TRAILING_FIELD_WIDTH: Rems = Rems(10.);

/// The Claude Code `--model` aliases the model menu names, with their labels.
const CLAUDE_MODELS: [(&str, &str); 3] = [("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku")];

const DEFAULT_MODEL_LABEL: &str = "Agent default";
const CUSTOM_MODEL_LABEL: &str = "Custom…";

/// What the model menu shows as chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ModelChoice {
    Default,
    /// A model the menu names, by its `--model` value.
    Named(SharedString),
    /// A model name typed in the field under the menu.
    Custom,
}

/// AI editing section of the Settings page: how the agent is launched, the creator's
/// standing instructions, default edit preferences and the requirement checklist.
///
/// Toggles save at once; text fields save shortly after typing stops and on blur.
pub struct AiSettings {
    state: Entity<AppState>,
    agent_command: Entity<InputState>,
    codex_command: Entity<InputState>,
    custom_model: Entity<InputState>,
    skill_dir: Entity<InputState>,
    /// Saved to its own owner-only file, not with the settings.
    elevenlabs_key: Entity<InputState>,
    extra_instructions: Entity<TextareaState>,
    target_length: Entity<InputState>,
    agent_harness: AgentHarness,
    /// Custom was chosen in the model menu, so the field shows even for a model the
    /// menu names.
    custom_model_picked: bool,
    /// Why the typed skill folder was not saved.
    skill_dir_error: Option<SharedString>,
    doctor: Entity<DoctorChecklist>,
    save_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl AiSettings {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = AppState::global(cx);
        let settings = state.read(cx).settings().pipeline.clone();
        let input = |value: String, placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value))
        };
        let agent_command = input(display_command(&settings.agent_command), DEFAULT_AGENT_COMMAND, window, cx);
        let codex_command = input(display_command(&settings.codex_command), DEFAULT_CODEX_COMMAND, window, cx);
        let custom_model = input(selected_model(&settings).unwrap_or_default().to_owned(), "Model name", window, cx);
        let skill_dir = input(
            settings.skill_dir.as_deref().map(display_user_path).unwrap_or_default(),
            "~/.claude/skills/video-use",
            window,
            cx,
        );
        let target_length = input(settings.default_preferences.target_length.clone(), "Natural length", window, cx);
        let elevenlabs_key = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Paste your key")
                .default_value(read_elevenlabs_key(&data_dir()).unwrap_or_default())
        });
        let extra_instructions = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 10)
                .placeholder("For example: keep my intro jingle, never cut mid-sentence.")
                .default_value(settings.extra_instructions.clone())
        });
        let doctor = cx.new(|cx| DoctorChecklist::new(window, cx));

        let mut subscriptions = vec![
            cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| {
                if *event == AppEvent::SettingsChanged {
                    this.sync_from_settings(window, cx);
                }
            }),
            // The skill check result shows next to the folder field.
            cx.observe(&PipelineService::global(cx), |_, _, cx| cx.notify()),
            cx.subscribe(&skill_dir, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) && this.skill_dir_error.is_some() {
                    this.skill_dir_error = None;
                    cx.notify();
                }
            }),
        ];
        // Typing a model name keeps the field open, even while the name matches a
        // model the menu lists or is empty.
        subscriptions.push(cx.subscribe(&custom_model, |this, _, event: &InputEvent, _| {
            if matches!(event, InputEvent::Change) {
                this.custom_model_picked = true;
            }
        }));
        for input in [&agent_command, &codex_command, &custom_model, &skill_dir, &target_length, &elevenlabs_key] {
            subscriptions.push(cx.subscribe(input, |this, _, event: &InputEvent, cx| this.on_text_event(event, cx)));
        }
        subscriptions
            .push(cx.subscribe(&extra_instructions, |this, _, event: &InputEvent, cx| this.on_text_event(event, cx)));
        // Leaving the settings before the pause ends must not lose what was typed.
        subscriptions.push(cx.on_release(|this, cx| {
            if this.save_task.is_some() {
                this.write_text(cx);
            }
        }));

        Self {
            state,
            agent_command,
            codex_command,
            custom_model,
            skill_dir,
            elevenlabs_key,
            extra_instructions,
            target_length,
            agent_harness: settings.agent_harness,
            custom_model_picked: false,
            skill_dir_error: None,
            doctor,
            save_task: None,
            _subscriptions: subscriptions,
        }
    }

    fn on_text_event(&mut self, event: &InputEvent, cx: &mut Context<Self>) {
        match event {
            InputEvent::Change => {
                self.save_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SAVE_DELAY).await;
                    _ = this.update(cx, |this, cx| this.save_text(cx));
                }));
            }
            InputEvent::Blur => self.save_text(cx),
            InputEvent::PressEnter { .. } | InputEvent::Focus => {}
        }
    }

    /// Saves every text field now.
    fn save_text(&mut self, cx: &mut Context<Self>) {
        self.write_text(cx);
        cx.notify();
    }

    fn write_text(&mut self, cx: &mut App) {
        self.save_task = None;
        let agent_command = command_input(&self.agent_command.read(cx).value(), DEFAULT_AGENT_COMMAND);
        let codex_command = command_input(&self.codex_command.read(cx).value(), DEFAULT_CODEX_COMMAND);
        let skill_dir = skill_dir_input(&self.skill_dir.read(cx).value());
        self.skill_dir_error = skill_dir.as_ref().err().map(|error| error.clone().into());
        let extra_instructions = self.extra_instructions.read(cx).value().to_string();
        let target_length = self.target_length.read(cx).value().trim().to_string();
        let custom_model = (self.model_choice(cx) == ModelChoice::Custom).then(|| self.custom_model_value(cx));
        let key = self.elevenlabs_key.read(cx).value().trim().to_string();
        if read_elevenlabs_key(&data_dir()).unwrap_or_default() != key {
            match write_elevenlabs_key(&data_dir(), &key) {
                Ok(()) => PipelineService::global(cx).update(cx, |service, cx| service.refresh_doctor(cx)),
                Err(error) => {
                    let message = SharedString::from(format!("Couldn’t save the ElevenLabs key: {error}"));
                    self.state.update(cx, |_, cx| cx.emit(AppEvent::Error(message)));
                }
            }
        }
        self.state.update(cx, |state, cx| {
            state.update_settings(cx, |settings| {
                let pipeline = &mut settings.pipeline;
                pipeline.agent_command = agent_command;
                pipeline.codex_command = codex_command;
                // A path that can't be used is not saved; the field says why.
                if let Ok(skill_dir) = skill_dir {
                    pipeline.skill_dir = skill_dir;
                }
                pipeline.extra_instructions = extra_instructions;
                pipeline.default_preferences.target_length = target_length;
                if let Some(model) = custom_model {
                    match self.agent_harness {
                        AgentHarness::Claude => pipeline.model = model,
                        AgentHarness::Codex => pipeline.codex_model = model,
                    }
                }
            })
        });
    }

    /// The model typed in the custom field; `None` when it is empty.
    fn custom_model_value(&self, cx: &App) -> Option<String> {
        Some(self.custom_model.read(cx).value().trim().to_string()).filter(|model| !model.is_empty())
    }

    /// The models the menu names for the selected harness, as `(--model value, label)`:
    /// Claude Code's aliases, or the models the Codex CLI lists. `None` while Codex
    /// is listing them.
    fn named_models(&self, cx: &App) -> Option<Vec<(SharedString, SharedString)>> {
        match self.agent_harness {
            AgentHarness::Claude => {
                Some(CLAUDE_MODELS.iter().map(|&(slug, label)| (slug.into(), label.into())).collect())
            }
            AgentHarness::Codex => PipelineService::global(cx).read(cx).codex_models(cx).map(|models| {
                models.iter().map(|model| (model.slug.clone().into(), model.display_name.clone().into())).collect()
            }),
        }
    }

    fn model_choice(&self, cx: &App) -> ModelChoice {
        if self.custom_model_picked {
            return ModelChoice::Custom;
        }
        let settings = &self.state.read(cx).settings().pipeline;
        let Some(model) = selected_model(settings).map(str::trim).filter(|model| !model.is_empty()) else {
            return ModelChoice::Default;
        };
        match self.named_models(cx) {
            // Still listing: a saved model is most likely one Codex lists.
            None => ModelChoice::Named(model.to_owned().into()),
            Some(named) if named.iter().any(|(slug, _)| slug == model) => ModelChoice::Named(model.to_owned().into()),
            Some(_) => ModelChoice::Custom,
        }
    }

    fn update(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut PipelineSettings)) {
        self.state.update(cx, |state, cx| state.update_settings(cx, |settings| change(&mut settings.pipeline)));
        cx.notify();
    }

    fn set_model(&mut self, choice: ModelChoice, window: &mut Window, cx: &mut Context<Self>) {
        let value = match &choice {
            ModelChoice::Default => None,
            ModelChoice::Named(slug) => Some(slug.to_string()),
            // A custom model stays; a named one leaves an empty field to type in.
            ModelChoice::Custom if self.model_choice(cx) == ModelChoice::Custom => self.custom_model_value(cx),
            ModelChoice::Custom => None,
        };
        self.custom_model_picked = choice == ModelChoice::Custom;
        let text = value.clone().unwrap_or_default();
        self.custom_model.update(cx, |input, cx| input.set_value(text, window, cx));
        if self.custom_model_picked {
            self.custom_model.update(cx, |input, cx| input.focus(window, cx));
        }
        let harness = self.agent_harness;
        self.update(cx, |pipeline| match harness {
            AgentHarness::Claude => pipeline.model = value,
            AgentHarness::Codex => pipeline.codex_model = value,
        });
    }

    fn set_agent_harness(&mut self, harness: AgentHarness, window: &mut Window, cx: &mut Context<Self>) {
        self.save_text(cx);
        self.update(cx, |pipeline| pipeline.agent_harness = harness);
        self.sync_from_settings(window, cx);
    }

    fn choose_skill_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose the video-use folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(picked) = picked.await else { return };
            _ = this.update_in(cx, |this, window, cx| this.on_skill_dir_picked(picked, window, cx));
        })
        .detach();
    }

    /// Saves the folder the chooser returned, or asks for its path when the
    /// chooser could not open (no desktop portal, for example).
    fn on_skill_dir_picked(
        &mut self,
        picked: gpui_kit::Result<Option<Vec<PathBuf>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match picked.map(|paths| paths.and_then(|paths| paths.into_iter().next())) {
            Ok(Some(dir)) => self.use_skill_dir(dir, cx),
            Ok(None) => {}
            Err(error) => {
                log_line!("rushes: folder chooser failed: {error}");
                let request = PathRequest::new(
                    PathKind::Folder,
                    "Type the path of the video-use folder instead.",
                    "~/.claude/skills/video-use",
                )
                .value(self.skill_dir.read(cx).value().to_string());
                let settings = cx.weak_entity();
                path_prompt::open(request, window, cx, move |dir, _, _| {
                    let settings = settings.clone();
                    path_prompt::accept_now(move |_, cx| {
                        _ = settings.update(cx, |settings, cx| settings.use_skill_dir(dir, cx));
                    })
                });
            }
        }
    }

    fn use_skill_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        self.skill_dir_error = None;
        self.update(cx, |pipeline| pipeline.skill_dir = Some(dir));
    }

    /// Shows settings changed elsewhere (onboarding, the folder picker) in the fields
    /// that are not being edited.
    fn sync_from_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pipeline = self.state.read(cx).settings().pipeline.clone();
        if self.agent_harness != pipeline.agent_harness {
            self.agent_harness = pipeline.agent_harness;
            self.custom_model_picked = false;
            let value = selected_model(&pipeline).unwrap_or_default();
            self.custom_model.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        let skill_dir = pipeline.skill_dir.as_deref().map(display_user_path).unwrap_or_default();
        let fields = [
            (&self.agent_command, display_command(&pipeline.agent_command)),
            (&self.codex_command, display_command(&pipeline.codex_command)),
            (&self.skill_dir, skill_dir),
            (&self.target_length, pipeline.default_preferences.target_length.clone()),
        ];
        for (input, value) in fields {
            let editing = input.focus_handle(cx).is_focused(window);
            if !editing && input.read(cx).value().trim() != value.trim() {
                input.update(cx, |input, cx| input.set_value(value, window, cx));
            }
        }
        let editing = self.extra_instructions.focus_handle(cx).is_focused(window);
        if !editing && self.extra_instructions.read(cx).value() != pipeline.extra_instructions.as_str() {
            self.extra_instructions.update(cx, |input, cx| input.set_value(pipeline.extra_instructions, window, cx));
        }
        cx.notify();
    }

    /// Under the skill folder field: why it was not saved, or that the folder has
    /// no `SKILL.md`.
    fn render_skill_dir_status(&self, cx: &App) -> Option<impl IntoElement + use<>> {
        let theme = cx.theme();
        let (id, message, color): (_, SharedString, _) = if let Some(error) = &self.skill_dir_error {
            ("ai-settings-skill-dir-error", error.clone(), theme.danger)
        } else {
            let service = PipelineService::global(cx);
            let service = service.read(cx);
            let missing = !service.is_checking()
                && service.doctor().is_some_and(|doctor| doctor.skill.as_ref().is_some_and(|check| !check.ok));
            let folder = self.state.read(cx).settings().pipeline.skill_dir.clone().or_else(default_skill_dir)?;
            if !missing {
                return None;
            }
            (
                "ai-settings-skill-dir-missing",
                format!("No SKILL.md in {}.", display_user_path(&folder)).into(),
                theme.danger,
            )
        };
        Some(div().id(id).test_support().text_sm().text_color(color).child(message))
    }

    /// The model menu, with the model name under it once Custom is chosen: side by
    /// side they would not fit beside the label in a narrow window.
    fn render_model(&self, cx: &mut Context<Self>) -> Div {
        let settings = cx.weak_entity();
        let current = self.model_choice(cx);
        let custom = current == ModelChoice::Custom;
        let mut named = self.named_models(cx).unwrap_or_default();
        if let ModelChoice::Named(slug) = &current
            && !named.iter().any(|(named, _)| named == slug)
        {
            // Codex is still listing its models.
            named.push((slug.clone(), slug.clone()));
        }
        let mut choices = vec![(ModelChoice::Default, SharedString::from(DEFAULT_MODEL_LABEL))];
        choices.extend(named.into_iter().map(|(slug, label)| (ModelChoice::Named(slug), label)));
        choices.push((ModelChoice::Custom, CUSTOM_MODEL_LABEL.into()));
        let label = choices
            .iter()
            .find(|(choice, _)| *choice == current)
            .map_or_else(|| SharedString::from(CUSTOM_MODEL_LABEL), |(_, label)| label.clone());
        v_flex()
            .gap_2()
            .when(custom, |this| this.w(TRAILING_FIELD_WIDTH))
            .child(
                Button::new("ai-settings-model")
                    .outline()
                    .label(SharedString::from(label.trim_end_matches('…').to_owned()))
                    .dropdown_caret(true)
                    .dropdown_menu(move |mut menu, _, _| {
                        for (choice, label) in &choices {
                            let settings = settings.clone();
                            let checked = *choice == current;
                            let choice = choice.clone();
                            menu =
                                menu.item(menu_item(label.clone()).checked(checked).on_click(move |_, window, cx| {
                                    let choice = choice.clone();
                                    _ = settings.update(cx, |this, cx| this.set_model(choice, window, cx));
                                }));
                        }
                        menu
                    }),
            )
            .when(custom, |this| this.child(Input::new(&self.custom_model).id("ai-settings-custom-model")))
            .when_some(self.render_model_error(cx), |this, error| this.child(error))
    }

    /// Under the model menu: why Codex could not list its models.
    fn render_model_error(&self, cx: &App) -> Option<impl IntoElement + use<>> {
        if self.agent_harness != AgentHarness::Codex {
            return None;
        }
        let service = PipelineService::global(cx);
        let error = service.read(cx).codex_models_error(cx)?;
        let reason = error
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("no reason given")
            .trim_end_matches('.');
        let message = format!("Couldn’t list Codex’s models: {reason}. Re-check under Requirements to try again.");
        Some(
            div()
                .id("ai-settings-model-error")
                .test_support()
                .w(TRAILING_FIELD_WIDTH)
                .text_sm()
                .text_color(cx.theme().danger)
                .child(message),
        )
    }

    fn render_agent_harness(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let settings = cx.weak_entity();
        let current = self.agent_harness;
        Button::new("ai-settings-agent-harness").outline().label(current.label()).dropdown_caret(true).dropdown_menu(
            move |mut menu, _, _| {
                for harness in [AgentHarness::Codex, AgentHarness::Claude] {
                    let settings = settings.clone();
                    menu = menu.item(menu_item(harness.label()).checked(harness == current).on_click(
                        move |_, window, cx| {
                            _ = settings.update(cx, |this, cx| this.set_agent_harness(harness, window, cx));
                        },
                    ));
                }
                menu
            },
        )
    }

    fn render_harness(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let settings = cx.weak_entity();
        let current = self.state.read(cx).settings().pipeline.harness;
        Button::new("ai-settings-harness").outline().label(harness_label(current)).dropdown_caret(true).dropdown_menu(
            move |mut menu, _, _| {
                for harness in [EditHarness::Rushes, EditHarness::VideoUse] {
                    let settings = settings.clone();
                    menu = menu.item(menu_item(harness_label(harness)).checked(harness == current).on_click(
                        move |_, _, cx| {
                            _ = settings.update(cx, |this, cx| this.update(cx, |pipeline| pipeline.harness = harness));
                        },
                    ));
                }
                menu
            },
        )
    }

    fn render_skill_dir(&self, cx: &mut Context<Self>) -> Div {
        v_flex()
            .gap_1p5()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.skill_dir).id("ai-settings-skill-dir")))
                    .child(
                        Button::new("ai-settings-choose-skill")
                            .outline()
                            .label("Choose…")
                            .on_click(cx.listener(|this, _, window, cx| this.choose_skill_dir(window, cx))),
                    ),
            )
            .children(self.render_skill_dir_status(cx))
    }

    /// A switch for one boolean of the pipeline settings, saved at once.
    fn render_switch(
        &self,
        id: &'static str,
        value: impl Fn(&PipelineSettings) -> bool,
        set: fn(&mut PipelineSettings, bool),
        cx: &mut Context<Self>,
    ) -> Switch {
        let checked = value(&self.state.read(cx).settings().pipeline);
        Switch::new(id).checked(checked).on_change(cx.listener(move |this, checked: &bool, _, cx| {
            let checked = *checked;
            this.update(cx, |pipeline| set(pipeline, checked))
        }))
    }

    fn render_grade(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let settings = cx.weak_entity();
        let grade = self.state.read(cx).settings().pipeline.default_preferences.color_grade.clone();
        Button::new("ai-settings-grade")
            .outline()
            .label(color_grade_label(&grade).to_owned())
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for (value, label) in COLOR_GRADES {
                    let settings = settings.clone();
                    menu = menu.item(menu_item(label).checked(grade == value).on_click(move |_, _, cx| {
                        _ = settings.update(cx, |this, cx| {
                            this.update(cx, |pipeline| pipeline.default_preferences.color_grade = value.to_owned())
                        });
                    }));
                }
                menu
            })
    }

    /// The settings, grouped as the Settings page shows them.
    pub fn sections() -> Vec<SettingsSection<Self>> {
        vec![
            SettingsSection::new()
                .title("Agent")
                .field(
                    SettingsField::new("Agent harness", |this: &mut Self, _, cx| this.render_agent_harness(cx))
                        .description("Choose Codex or Claude Code for new AI edits and YouTube details. Existing edits keep their original harness."),
                )
                .field(
                    SettingsField::new("Claude Code command", |this: &mut Self, _, _| {
                        Input::new(&this.agent_command).id("ai-settings-agent-command")
                    })
                    .description("The Claude Code CLI: a command on PATH or the full path to it.")
                    .stacked(),
                )
                .field(
                    SettingsField::new("Codex command", |this: &mut Self, _, _| {
                        Input::new(&this.codex_command).id("ai-settings-codex-command")
                    })
                    .description("The Codex CLI, signed in. Also used to draw thumbnails, whichever harness you choose.")
                    .stacked(),
                )
                .field(
                    SettingsField::new("Model", |this: &mut Self, _, cx| this.render_model(cx))
                        .description("Agent default uses the selected harness’s model. Each harness keeps its own choice."),
                )
                .field(
                    SettingsField::new("Editing tools", |this: &mut Self, _, cx| this.render_harness(cx)).description(
                        "What the agent edits with. Rushes’ built-in tools need only ffmpeg and an ElevenLabs key; \
                         the video-use skill is installed separately. Applies to new edits: an edit keeps the tools \
                         it started with.",
                    ),
                )
                .field(
                    SettingsField::new("ElevenLabs API key", |this: &mut Self, _, _| {
                        Input::new(&this.elevenlabs_key).mask_toggle().id("ai-settings-elevenlabs-key")
                    })
                    .description(
                        "Transcribes the footage. Kept on this computer in a file only you can read. Empty uses \
                         the key in the environment or in video-use’s .env.",
                    )
                    .stacked(),
                )
                .field(
                    SettingsField::new("video-use skill folder", |this: &mut Self, _, cx| this.render_skill_dir(cx))
                        .description(
                            "For the video-use skill only: the folder with its SKILL.md. Empty looks in \
                             ~/.claude/skills/video-use.",
                        )
                        .stacked(),
                ),
            SettingsSection::new()
                .title("Edits")
                .field(
                    SettingsField::new("Standing instructions", |this: &mut Self, _, _| {
                        Textarea::new(&this.extra_instructions)
                    })
                    .description("Added to every edit brief: house style, intro and outro rules, branding.")
                    .stacked(),
                )
                .field(
                    SettingsField::new("Skip strategy review", |this: &mut Self, _, cx| {
                        this.render_switch(
                            "ai-settings-auto-approve",
                            |pipeline| pipeline.auto_approve,
                            |pipeline, on| pipeline.auto_approve = on,
                            cx,
                        )
                    })
                    .description(
                        "Plan and render in one pass. Off: the agent proposes a strategy and waits for your approval.",
                    ),
                ),
            SettingsSection::new()
                .title("New edits")
                .description("Where every new edit brief starts.")
                .field(SettingsField::new("Remove fillers", |this: &mut Self, _, cx| {
                    this.render_switch(
                        "ai-settings-remove-fillers",
                        |pipeline| pipeline.default_preferences.remove_fillers,
                        |pipeline, on| pipeline.default_preferences.remove_fillers = on,
                        cx,
                    )
                }))
                .field(SettingsField::new("Burn subtitles", |this: &mut Self, _, cx| {
                    this.render_switch(
                        "ai-settings-subtitles",
                        |pipeline| pipeline.default_preferences.subtitles,
                        |pipeline, on| pipeline.default_preferences.subtitles = on,
                        cx,
                    )
                }))
                .field(SettingsField::new("Colour grade", |this: &mut Self, _, cx| this.render_grade(cx)))
                .field(
                    SettingsField::new("Target length", |this: &mut Self, _, _| {
                        div()
                            .w(TRAILING_FIELD_WIDTH)
                            .child(Input::new(&this.target_length).id("ai-settings-target-length"))
                    })
                    .description("For example 60 seconds; empty keeps the natural length."),
                ),
            SettingsSection::new()
                .title("Requirements")
                .description(
                    "AI edits run the selected harness on this computer with Rushes’ editing tools or video-use. The footage, the key and the tools stay local.",
                )
                // The checklist's rows, which the search can't see.
                .keywords(["claude", "codex", "video-use", "editing tools", "ffmpeg", "ffprobe", "elevenlabs", "python", "re-check"])
                .element(|this: &mut Self, _, _| this.doctor.clone()),
        ]
    }
}

fn selected_model(settings: &PipelineSettings) -> Option<&str> {
    match settings.agent_harness {
        AgentHarness::Claude => settings.model.as_deref(),
        AgentHarness::Codex => settings.codex_model.as_deref(),
    }
}

/// A command typed in a field: a command name as typed, a path starting with `~`
/// expanded; empty is `default`.
fn command_input(text: &str, default: &str) -> String {
    let text = text.trim();
    match text {
        "" => default.to_owned(),
        path if path.starts_with('~') => {
            expand_user_path(path).map_or_else(|_| path.to_owned(), |path| path.display().to_string())
        }
        command => command.to_owned(),
    }
}

/// The agent command as the field shows it: a path under the home folder with `~`.
fn display_command(command: &str) -> String {
    let path = Path::new(command);
    if path.is_absolute() { display_user_path(path) } else { command.to_owned() }
}

/// The skill folder typed in the field: `None` (empty) is the default folder; `~` is
/// expanded; a relative path is refused, with the reason to show.
fn harness_label(harness: EditHarness) -> &'static str {
    match harness {
        EditHarness::Rushes => "Rushes (built in)",
        EditHarness::VideoUse => "video-use skill",
    }
}

fn skill_dir_input(text: &str) -> Result<Option<PathBuf>, String> {
    match expand_user_path(text) {
        Ok(path) => Ok(Some(path)),
        Err(PathInputError::Empty) => Ok(None),
        Err(PathInputError::Relative) => Err("Use a full path, like ~/.claude/skills/video-use.".to_owned()),
        Err(error) => Err(error.to_string()),
    }
}

impl Render for AiSettings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sections: Vec<AnyElement> =
            Self::sections().iter().map(|section| section.render(self, window, cx)).collect();
        v_flex().w_full().gap_8().children(sections)
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{Bounds, ElementId, TestAppContext, WindowBounds, WindowOptions, px, size};
    use rushes_core::Store;

    use super::*;

    #[gpui_kit::test]
    fn without_a_folder_chooser_the_skill_folder_is_typed(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| cx.set_reduce_motion(true));
        let state = cx.new(|_| AppState::new(Store::open_in_memory().unwrap()).unwrap());
        cx.update(|cx| {
            AppState::set_global(state.clone(), cx);
            PipelineService::init(state.clone(), cx);
        });
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(Default::default(), size(px(900.), px(1600.))))),
            ..WindowOptions::default()
        };
        let (window, settings) = cx
            .update(|cx| gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| AiSettings::new(window, cx))))
            .unwrap();
        let skill = tempfile::tempdir().unwrap();
        let step = |cx: &mut TestAppContext, interact: &dyn Fn(&mut Window, &mut App)| {
            cx.update_window(window, |_, window, cx| interact(window, cx)).unwrap();
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        };

        step(cx, &|window, cx| {
            let failed = Err(std::io::Error::other("ZBus Error: no bus").into());
            settings.update(cx, |settings, cx| settings.on_skill_dir_picked(failed, window, cx));
        });
        assert!(cx.update_window(window, |_, window, cx| window.has_active_dialog(cx)).unwrap());
        let missing = skill.path().join("missing").display().to_string();
        step(cx, &|window, cx| window.input(&missing, cx));
        step(cx, &|window, cx| window.press("enter", cx));
        let error = ElementId::Name(path_prompt::ERROR_ID.into());
        assert!(cx.update_window(window, |_, window, _| window.try_find(error.clone()).is_some()).unwrap());

        let typed = skill.path().display().to_string();
        step(cx, &|window, cx| window.press("secondary-a", cx));
        step(cx, &|window, cx| window.input(&typed, cx));
        step(cx, &|window, cx| window.press("enter", cx));
        assert!(!cx.update_window(window, |_, window, cx| window.has_active_dialog(cx)).unwrap());
        let saved = cx.read(|cx| state.read(cx).settings().pipeline.skill_dir.clone());
        assert_eq!(saved.as_deref(), Some(skill.path()));
    }

    #[test]
    fn typed_paths_expand_the_home_folder() {
        let home = std::env::home_dir().expect("home folder");
        assert_eq!(skill_dir_input(" ~/video_editing/video-use "), Ok(Some(home.join("video_editing/video-use"))));
        assert_eq!(skill_dir_input("/opt/video-use"), Ok(Some(PathBuf::from("/opt/video-use"))));
        assert_eq!(skill_dir_input("  "), Ok(None));
        assert_eq!(skill_dir_input("video-use"), Err("Use a full path, like ~/.claude/skills/video-use.".to_owned()));

        assert_eq!(
            command_input("~/.local/bin/claude", DEFAULT_AGENT_COMMAND),
            home.join(".local/bin/claude").display().to_string()
        );
        assert_eq!(command_input(" claude ", DEFAULT_AGENT_COMMAND), "claude");
        assert_eq!(command_input("", DEFAULT_AGENT_COMMAND), DEFAULT_AGENT_COMMAND);
        assert_eq!(command_input("", DEFAULT_CODEX_COMMAND), "codex");
        assert_eq!(display_command(&home.join(".local/bin/claude").display().to_string()), "~/.local/bin/claude");
        assert_eq!(display_command("claude"), "claude");
    }
}
