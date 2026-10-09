//! [`AiSettings`]: the AI editing section of the Settings page.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::form::{Field, Form};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Div, Entity, Focusable as _, FontWeight, IntoElement, ParentElement as _,
    PathPromptOptions, Render, Styled as _, Subscription, Task, Window, div,
};
use videocrm_core::settings::{DEFAULT_AGENT_COMMAND, PipelineSettings};

use super::brief::{COLOR_GRADES, color_grade_label};
use super::doctor::DoctorChecklist;
use crate::state::{AppEvent, AppState};

/// How long typing pauses before text fields are saved.
const SAVE_DELAY: Duration = Duration::from_millis(600);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelChoice {
    Default,
    Opus,
    Sonnet,
    Haiku,
    Custom,
}

impl ModelChoice {
    const ALL: [ModelChoice; 5] =
        [ModelChoice::Default, ModelChoice::Opus, ModelChoice::Sonnet, ModelChoice::Haiku, ModelChoice::Custom];

    fn of(model: Option<&str>) -> Self {
        match model.map(str::trim) {
            None | Some("") => ModelChoice::Default,
            Some("opus") => ModelChoice::Opus,
            Some("sonnet") => ModelChoice::Sonnet,
            Some("haiku") => ModelChoice::Haiku,
            Some(_) => ModelChoice::Custom,
        }
    }

    fn label(self) -> &'static str {
        match self {
            ModelChoice::Default => "Agent default",
            ModelChoice::Opus => "Opus",
            ModelChoice::Sonnet => "Sonnet",
            ModelChoice::Haiku => "Haiku",
            ModelChoice::Custom => "Custom…",
        }
    }

    /// The `--model` alias, for the named choices.
    fn alias(self) -> Option<&'static str> {
        match self {
            ModelChoice::Opus => Some("opus"),
            ModelChoice::Sonnet => Some("sonnet"),
            ModelChoice::Haiku => Some("haiku"),
            ModelChoice::Default | ModelChoice::Custom => None,
        }
    }
}

/// AI editing section of the Settings page: how the agent is launched, the creator's
/// standing instructions, default edit preferences and the requirement checklist.
///
/// Toggles save at once; text fields save shortly after typing stops and on blur.
pub struct AiSettings {
    state: Entity<AppState>,
    agent_command: Entity<InputState>,
    custom_model: Entity<InputState>,
    skill_dir: Entity<InputState>,
    extra_instructions: Entity<TextareaState>,
    target_length: Entity<InputState>,
    model: ModelChoice,
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
        let agent_command = input(settings.agent_command.clone(), DEFAULT_AGENT_COMMAND, window, cx);
        let model = ModelChoice::of(settings.model.as_deref());
        let custom_model = input(
            if model == ModelChoice::Custom { settings.model.clone().unwrap_or_default() } else { String::new() },
            "Model name, e.g. claude-opus-4-1",
            window,
            cx,
        );
        let skill_dir = input(
            settings.skill_dir.as_ref().map(|dir| dir.display().to_string()).unwrap_or_default(),
            "~/.claude/skills/video-use",
            window,
            cx,
        );
        let target_length = input(settings.default_preferences.target_length.clone(), "Natural length", window, cx);
        let extra_instructions = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 10)
                .placeholder("For example: keep my intro jingle, end on the subscribe card, never cut mid-sentence.")
                .default_value(settings.extra_instructions.clone())
        });
        let doctor = cx.new(|cx| DoctorChecklist::new(window, cx));

        let mut subscriptions = vec![cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| {
            if *event == AppEvent::SettingsChanged {
                this.sync_from_settings(window, cx);
            }
        })];
        for input in [&agent_command, &custom_model, &skill_dir, &target_length] {
            subscriptions.push(cx.subscribe(input, |this, _, event: &InputEvent, cx| this.on_text_event(event, cx)));
        }
        subscriptions
            .push(cx.subscribe(&extra_instructions, |this, _, event: &InputEvent, cx| this.on_text_event(event, cx)));

        Self {
            state,
            agent_command,
            custom_model,
            skill_dir,
            extra_instructions,
            target_length,
            model,
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
        self.save_task = None;
        let agent_command = self.agent_command.read(cx).value().trim().to_string();
        let skill_dir = self.skill_dir.read(cx).value().trim().to_string();
        let extra_instructions = self.extra_instructions.read(cx).value().to_string();
        let target_length = self.target_length.read(cx).value().trim().to_string();
        let model = self.model_value(cx);
        self.update(cx, move |pipeline| {
            pipeline.agent_command =
                if agent_command.is_empty() { DEFAULT_AGENT_COMMAND.to_owned() } else { agent_command };
            pipeline.skill_dir = (!skill_dir.is_empty()).then(|| PathBuf::from(skill_dir));
            pipeline.extra_instructions = extra_instructions;
            pipeline.default_preferences.target_length = target_length;
            pipeline.model = model;
        });
    }

    fn model_value(&self, cx: &App) -> Option<String> {
        match self.model {
            ModelChoice::Custom => {
                Some(self.custom_model.read(cx).value().trim().to_string()).filter(|model| !model.is_empty())
            }
            choice => choice.alias().map(str::to_owned),
        }
    }

    fn update(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut PipelineSettings)) {
        self.state.update(cx, |state, cx| state.update_settings(cx, |settings| change(&mut settings.pipeline)));
        cx.notify();
    }

    fn set_model(&mut self, model: ModelChoice, window: &mut Window, cx: &mut Context<Self>) {
        self.model = model;
        if model == ModelChoice::Custom {
            self.custom_model.update(cx, |input, cx| input.focus(window, cx));
        }
        let value = self.model_value(cx);
        self.update(cx, |pipeline| pipeline.model = value);
    }

    fn choose_skill_dir(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose the video-use folder".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(dir) = paths.into_iter().next() else { return };
            _ = this.update(cx, |this, cx| this.update(cx, |pipeline| pipeline.skill_dir = Some(dir)));
        })
        .detach();
    }

    /// Shows settings changed elsewhere (onboarding, the folder picker) in the fields
    /// that are not being edited.
    fn sync_from_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pipeline = self.state.read(cx).settings().pipeline.clone();
        let skill_dir = pipeline.skill_dir.as_ref().map(|dir| dir.display().to_string()).unwrap_or_default();
        let fields = [
            (&self.agent_command, pipeline.agent_command.clone()),
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

    fn render_model(&self, cx: &mut Context<Self>) -> Div {
        let settings = cx.weak_entity();
        let current = self.model;
        h_flex()
            .gap_2()
            .child(
                Button::new("ai-settings-model")
                    .outline()
                    .label(current.label().trim_end_matches('…'))
                    .dropdown_caret(true)
                    .dropdown_menu(move |mut menu, _, _| {
                        for choice in ModelChoice::ALL {
                            let settings = settings.clone();
                            menu = menu.item(PopupMenuItem::new(choice.label()).checked(choice == current).on_click(
                                move |_, window, cx| {
                                    _ = settings.update(cx, |this, cx| this.set_model(choice, window, cx));
                                },
                            ));
                        }
                        menu
                    }),
            )
            .when(current == ModelChoice::Custom, |this| {
                this.child(div().flex_1().child(Input::new(&self.custom_model)))
            })
    }

    fn render_preferences(&self, cx: &mut Context<Self>) -> Div {
        let preferences = self.state.read(cx).settings().pipeline.default_preferences.clone();
        let settings = cx.weak_entity();
        let grade = preferences.color_grade.clone();
        let theme = cx.theme();
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_6()
                    .child(
                        Checkbox::new("ai-settings-remove-fillers")
                            .label("Remove fillers")
                            .checked(preferences.remove_fillers)
                            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                let checked = *checked;
                                this.update(cx, |pipeline| pipeline.default_preferences.remove_fillers = checked)
                            })),
                    )
                    .child(
                        Checkbox::new("ai-settings-subtitles")
                            .label("Burn subtitles")
                            .checked(preferences.subtitles)
                            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                let checked = *checked;
                                this.update(cx, |pipeline| pipeline.default_preferences.subtitles = checked)
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(div().w_32().text_sm().text_color(theme.muted_foreground).child("Colour grade"))
                    .child(
                        Button::new("ai-settings-grade")
                            .outline()
                            .small()
                            .label(color_grade_label(&grade).to_owned())
                            .dropdown_caret(true)
                            .dropdown_menu(move |mut menu, _, _| {
                                for (value, label) in COLOR_GRADES {
                                    let settings = settings.clone();
                                    menu = menu.item(PopupMenuItem::new(label).checked(grade == value).on_click(
                                        move |_, _, cx| {
                                            _ = settings.update(cx, |this, cx| {
                                                this.update(cx, |pipeline| {
                                                    pipeline.default_preferences.color_grade = value.to_owned()
                                                })
                                            });
                                        },
                                    ));
                                }
                                menu
                            }),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(div().w_32().text_sm().text_color(theme.muted_foreground).child("Target length"))
                    .child(div().w_48().child(Input::new(&self.target_length).small())),
            )
    }
}

impl Render for AiSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let auto_approve = self.state.read(cx).settings().pipeline.auto_approve;
        let model = self.render_model(cx);
        let preferences = self.render_preferences(cx);
        let theme = cx.theme();
        let heading = |title: &'static str| div().text_base().font_weight(FontWeight::SEMIBOLD).child(title);

        v_flex()
            .w_full()
            .gap_10()
            .child(
                v_flex().gap_4().child(heading("Agent")).child(
                    Form::new()
                        .child(
                            Field::new()
                                .label("Agent command")
                                .description("The Claude Code CLI: a command on PATH or the full path to it.")
                                .child(Input::new(&self.agent_command).id("ai-settings-agent-command")),
                        )
                        .child(Field::new().label("Model").child(model))
                        .child(
                            Field::new()
                                .label("video-use skill folder")
                                .description(
                                    "The folder with video-use's SKILL.md. Empty looks in ~/.claude/skills/video-use.",
                                )
                                .child(
                                    h_flex().gap_2().child(div().flex_1().child(Input::new(&self.skill_dir).id("ai-settings-skill-dir"))).child(
                                        Button::new("ai-settings-choose-skill")
                                            .outline()
                                            .icon(IconName::FolderOpen)
                                            .label("Choose…")
                                            .on_click(cx.listener(|this, _, _, cx| this.choose_skill_dir(cx))),
                                    ),
                                ),
                        ),
                ),
            )
            .child(
                v_flex().gap_4().child(heading("Edits")).child(
                    Form::new()
                        .child(
                            Field::new()
                                .label("Standing instructions")
                                .description("Added to every edit brief: house style, intro and outro rules, branding.")
                                .child(Textarea::new(&self.extra_instructions)),
                        )
                        .child(
                            Field::new().label("Strategy review").description(
                                "Off: the agent proposes a strategy and waits for your approval before it renders.",
                            ).child(
                                Switch::new("ai-settings-auto-approve")
                                    .label("Skip by default")
                                    .checked(auto_approve)
                                    .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                        let checked = *checked;
                                        this.update(cx, |pipeline| pipeline.auto_approve = checked)
                                    })),
                            ),
                        )
                        .child(
                            Field::new()
                                .label("Default edit preferences")
                                .description("Where every new edit brief starts.")
                                .child(preferences),
                        ),
                ),
            )
            .child(
                v_flex()
                    .gap_4()
                    .child(
                        v_flex().gap_1().child(heading("Requirements")).child(
                            div().text_sm().text_color(theme.muted_foreground).child(
                                "AI edits run video-use inside Claude Code on this computer. The key and the tools stay local.",
                            ),
                        ),
                    )
                    .child(self.doctor.clone()),
            )
    }
}
