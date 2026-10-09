//! The dialog form that adds a property, renames one or edits its options.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Side, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Div, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, WeakEntity, Window, div,
};
use videocrm_core::{PropertyDef, PropertyId, PropertyKind, SelectOption, TagColor};

use super::colors::tag_colors;
use super::elements::Chip;
use super::icons::kind_icon;
use super::panel::{DatabasePanel, TEXT_ENTRY_CONTEXT};

/// What the property dialog edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PropertyEditorMode {
    /// A new property: name, type and options.
    Create,
    /// Only the name of an existing property.
    Rename,
    /// Name and options of an existing property; its type is fixed.
    Edit,
}

/// Property types offered for new properties, in menu order.
const KINDS: [fn() -> PropertyKind; 7] = [
    || PropertyKind::Text,
    || PropertyKind::Number,
    || PropertyKind::Select {
        options: Vec::new(),
    },
    || PropertyKind::MultiSelect {
        options: Vec::new(),
    },
    || PropertyKind::Date,
    || PropertyKind::Checkbox,
    || PropertyKind::Url,
];

struct OptionDraft {
    id: String,
    color: TagColor,
    name: Entity<InputState>,
}

pub(crate) struct PropertyEditor {
    mode: PropertyEditorMode,
    existing: Option<PropertyDef>,
    /// Position given to a new property.
    position: i32,
    name: Entity<InputState>,
    /// The type, with its options kept in `options` while editing.
    kind: PropertyKind,
    options: Vec<OptionDraft>,
    new_option: Entity<InputState>,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl PropertyEditor {
    pub fn new(
        existing: Option<PropertyDef>,
        mode: PropertyEditorMode,
        position: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let current_name = existing
            .as_ref()
            .map(|def| def.name.clone())
            .unwrap_or_default();
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Property name")
                .default_value(current_name)
        });
        let new_option = cx.new(|cx| InputState::new(window, cx).placeholder("Add an option…"));
        let kind = existing
            .as_ref()
            .map_or(PropertyKind::Text, |def| def.kind.clone());
        let options = kind
            .options()
            .iter()
            .map(|option| OptionDraft {
                id: option.id.clone(),
                color: option.color,
                name: cx.new(|cx| InputState::new(window, cx).default_value(option.name.clone())),
            })
            .collect();
        let subscriptions = vec![cx.subscribe_in(
            &new_option,
            window,
            |this, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let name = input.read(cx).value().trim().to_string();
                    if !name.is_empty() {
                        input.update(cx, |input, cx| input.set_value("", window, cx));
                        this.add_option(name, window, cx);
                    }
                }
            },
        )];

        Self {
            mode,
            existing,
            position,
            name,
            kind,
            options,
            new_option,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    /// Opens `editor` in a dialog; committing hands the property to `panel`.
    pub fn open(
        editor: Entity<Self>,
        panel: WeakEntity<DatabasePanel>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let mode = editor.read(cx).mode;
        let (title, commit) = match mode {
            PropertyEditorMode::Create => ("New property", "Create"),
            PropertyEditorMode::Rename => ("Rename property", "Rename"),
            PropertyEditorMode::Edit => ("Edit property", "Save"),
        };
        let name = editor.read(cx).name.clone();
        window.open_dialog(cx, move |dialog, window, _| {
            let editor = editor.clone();
            let panel = panel.clone();
            dialog
                .title(title)
                .width(window.rem_size() * 28.)
                .child(editor.clone())
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new()
                                .child(Button::new("property-cancel").outline().label("Cancel")),
                        )
                        .child(
                            DialogAction::new()
                                .child(Button::new("property-commit").primary().label(commit)),
                        ),
                )
                .on_ok(move |_, _, cx| match editor.read(cx).build(cx) {
                    Ok(def) => {
                        _ = panel.update(cx, |panel, cx| panel.save_property(def, cx));
                        true
                    }
                    Err(error) => {
                        editor.update(cx, |editor, cx| {
                            editor.error = Some(error);
                            cx.notify();
                        });
                        false
                    }
                })
        });
        // Opening the dialog focuses the dialog itself; the name comes first.
        name.update(cx, |input, cx| input.focus(window, cx));
    }

    /// The property the form describes, or what to fix first.
    fn build(&self, cx: &App) -> Result<PropertyDef, SharedString> {
        let name = self.name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return Err("Give the property a name.".into());
        }
        let mut options: Vec<SelectOption> = self
            .options
            .iter()
            .filter_map(|draft| {
                let name = draft.name.read(cx).value().trim().to_string();
                (!name.is_empty()).then(|| SelectOption {
                    id: draft.id.clone(),
                    name,
                    color: draft.color,
                })
            })
            .collect();
        // An option typed but not yet added with Enter is still meant.
        let pending = self.new_option.read(cx).value().trim().to_string();
        if !pending.is_empty()
            && !options
                .iter()
                .any(|option| option.name.eq_ignore_ascii_case(&pending))
        {
            options.push(SelectOption {
                id: PropertyId::new().to_string(),
                name: pending,
                color: option_color(self.options.len()),
            });
        }
        let kind = match &self.kind {
            PropertyKind::Select { .. } => PropertyKind::Select { options },
            PropertyKind::MultiSelect { .. } => PropertyKind::MultiSelect { options },
            kind => kind.clone(),
        };
        Ok(match &self.existing {
            Some(existing) => PropertyDef {
                name,
                kind,
                ..existing.clone()
            },
            None => PropertyDef {
                id: PropertyId::new(),
                name,
                kind,
                position: self.position,
            },
        })
    }

    fn add_option(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let exists = self.options.iter().any(|draft| {
            draft
                .name
                .read(cx)
                .value()
                .trim()
                .eq_ignore_ascii_case(&name)
        });
        if exists {
            return;
        }
        self.options.push(OptionDraft {
            id: PropertyId::new().to_string(),
            color: option_color(self.options.len()),
            name: cx.new(|cx| InputState::new(window, cx).default_value(name)),
        });
        cx.notify();
    }

    fn render_kind_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = cx.weak_entity();
        let current = self.kind.label();
        // Icon and label form one child so the caret's `justify_between`
        // keeps them at the leading edge.
        Button::new("property-kind")
            .outline()
            .w_full()
            .accessibility_label(self.kind.label())
            .child(
                h_flex()
                    .gap_2()
                    .child(Icon::new(kind_icon(&self.kind)).small())
                    .child(self.kind.label()),
            )
            .dropdown_caret(true)
            .disabled(self.existing.is_some())
            .dropdown_menu(move |menu, _, _| {
                // Each item has an icon, so the check goes on the trailing side.
                KINDS
                    .iter()
                    .fold(menu.check_side(Side::Right), |menu, kind| {
                        let kind = kind();
                        let editor = editor.clone();
                        let chosen = kind.clone();
                        menu.item(
                            PopupMenuItem::new(kind.label())
                                .icon(kind_icon(&kind))
                                .checked(kind.label() == current)
                                .on_click(move |_, _, cx| {
                                    let kind = chosen.clone();
                                    _ = editor.update(cx, |editor, cx| {
                                        editor.kind = kind;
                                        cx.notify();
                                    });
                                }),
                        )
                    })
            })
    }

    fn render_options(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = cx.weak_entity();
        v_flex()
            .gap_1p5()
            .child(field_label("Options", cx))
            .children(self.options.iter().map(|draft| {
                let dot = tag_colors(draft.color, cx).dot;
                let (id, current) = (draft.id.clone(), draft.color);
                let color_editor = editor.clone();
                let remove_editor = editor.clone();
                let remove_id = id.clone();
                h_flex()
                    .gap_1()
                    .child(
                        Button::new(SharedString::from(format!("option-color-{id}")))
                            .ghost()
                            .small()
                            .tooltip("Color")
                            .child(div().size_3().rounded_full().bg(dot))
                            .dropdown_menu(move |menu, _, _| {
                                TagColor::ALL.into_iter().fold(menu, |menu, color| {
                                    let editor = color_editor.clone();
                                    let id = id.clone();
                                    menu.item(
                                        PopupMenuItem::element(move |_, _| {
                                            Chip::new(color.label(), color)
                                        })
                                        .checked(color == current)
                                        .on_click(
                                            move |_, _, cx| {
                                                _ = editor.update(cx, |editor, cx| {
                                                    if let Some(draft) = editor
                                                        .options
                                                        .iter_mut()
                                                        .find(|d| d.id == id)
                                                    {
                                                        draft.color = color;
                                                        cx.notify();
                                                    }
                                                });
                                            },
                                        ),
                                    )
                                })
                            }),
                    )
                    .child(div().flex_1().child(Input::new(&draft.name)))
                    .child(
                        Button::new(SharedString::from(format!("option-remove-{remove_id}")))
                            .ghost()
                            .small()
                            .icon(IconName::X)
                            .tooltip("Remove option")
                            .on_click(move |_, _, cx| {
                                _ = remove_editor.update(cx, |editor, cx| {
                                    editor.options.retain(|draft| draft.id != remove_id);
                                    cx.notify();
                                });
                            }),
                    )
            }))
            .child(
                div().key_context(TEXT_ENTRY_CONTEXT).child(
                    Input::new(&self.new_option).id("new-option").prefix(
                        Icon::new(IconName::Plus)
                            .xsmall()
                            .text_color(cx.theme().muted_foreground),
                    ),
                ),
            )
    }
}

impl Render for PropertyEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_options = matches!(
            self.kind,
            PropertyKind::Select { .. } | PropertyKind::MultiSelect { .. }
        );
        v_flex()
            .gap_4()
            .child(
                v_flex()
                    .gap_1p5()
                    .child(field_label("Name", cx))
                    .child(Input::new(&self.name).id("property-name")),
            )
            .when(self.mode != PropertyEditorMode::Rename, |this| {
                this.child(
                    v_flex()
                        .gap_1p5()
                        .child(field_label("Type", cx))
                        .child(self.render_kind_picker(cx)),
                )
            })
            .when(
                has_options && self.mode != PropertyEditorMode::Rename,
                |this| this.child(self.render_options(cx)),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
    }
}

/// The color of the option added after `count` others: the palette in turn,
/// so neighbouring options differ, with gray left for the default.
fn option_color(count: usize) -> TagColor {
    TagColor::ALL[1 + count % (TagColor::ALL.len() - 1)]
}

fn field_label(label: &'static str, cx: &App) -> Div {
    div()
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .text_color(cx.theme().foreground)
        .child(label)
}
