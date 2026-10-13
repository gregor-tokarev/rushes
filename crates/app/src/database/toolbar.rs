//! The toolbar above a view: view tabs, filter, sort, search, properties and
//! the New video command.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, ElementExt as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, Context, Div, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, div, px, rems,
};
use rushes_core::{
    BuiltinField, Column, DatabaseView, FilterSpec, PropertyDef, SortDirection, SortSpec, VideoFormat, VideoStatus,
    ViewId, ViewKind,
};

use super::colors::format_color;
use super::elements::{Chip, count_badge};
use super::icons::{column_icon, view_icon};
use super::panel::{DatabasePanel, SortPicker};
use super::property_editor::PropertyEditorMode;
use super::query::{active_filter_count, column_name, resolve_columns};
use super::table::column_key;
use crate::ui::PAGE_INSET;

impl DatabasePanel {
    pub(super) fn set_sort_picker(&mut self, picker: Option<SortPicker>, cx: &mut Context<Self>) {
        if self.sort_picker != picker {
            self.sort_picker = picker;
            cx.notify();
        }
    }

    /// Points a sort rule at `column`, or adds a rule for it, and returns the
    /// Sort popover to its rules.
    fn choose_sort_column(&mut self, picker: SortPicker, column: Column, cx: &mut Context<Self>) {
        self.sort_picker = None;
        self.update_view(
            |view| match picker {
                SortPicker::Rule(ix) => set_sort_column(&mut view.sorts, ix, column),
                SortPicker::New => {
                    if !view.sorts.iter().any(|sort| sort.column == column) {
                        view.sorts.push(SortSpec { column, direction: SortDirection::Ascending });
                    }
                }
            },
            cx,
        );
        cx.notify();
    }

    pub(super) fn render_toolbar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = self.active_view().cloned();
        let panel = cx.weak_entity();
        let actions = h_flex()
            .id("database-actions")
            // On the tabs' line, or right-aligned on a line of its own when
            // the toolbar is too narrow for both.
            .ml_auto()
            .gap_2()
            .when_some(view, |this, view| {
                this.child(filter_popover(&view.filter, &panel, cx))
                    .child(sort_popover(&view, self.properties(), self.sort_picker, &panel, cx))
                    .when(view.kind == ViewKind::Table, |this| {
                        this.child(properties_popover(&view, self.properties(), &panel, cx))
                    })
            })
            .child(
                Input::new(&self.search)
                    .id("database-search")
                    .small()
                    .w_48()
                    .prefix(Icon::new(IconName::Search).xsmall().text_color(cx.theme().muted_foreground))
                    .cleanable(true),
            )
            .child(
                Button::new("new-video")
                    .small()
                    .icon(IconName::Plus)
                    .label("New video…")
                    .on_click(cx.listener(|this, _, _, cx| this.create_video_in_view(cx))),
            );
        h_flex()
            .w_full()
            .flex_shrink_0()
            .flex_wrap()
            .gap_2()
            .px(PAGE_INSET)
            .pb_2()
            .child(self.render_view_tabs(window, cx))
            .child(actions)
    }

    fn render_view_tabs(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = cx.weak_entity();
        let views: Vec<(ViewId, String, ViewKind)> =
            self.views().iter().map(|view| (view.id, view.name.clone(), view.kind)).collect();
        let view_ids: Vec<ViewId> = views.iter().map(|(id, ..)| *id).collect();
        let can_delete = views.len() > 1;

        // The strip scrolls when the views do not fit; it then also offers a
        // menu of every view, so none is out of reach.
        let overflow = self.view_tabs_overflow.clone();
        let tabs_scroll = self.view_tabs_scroll.clone();
        let tabs = TabBar::new("database-views")
            .underline()
            .small()
            .track_scroll(&self.view_tabs_scroll)
            .menu(overflow.get())
            .when_some(self.active_index(), |this, ix| this.selected_index(ix))
            .children(views.into_iter().map(|(id, name, kind)| {
                Tab::new()
                    .prefix(Icon::new(view_icon(kind)).xsmall())
                    .label(name)
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, _| this.menu_view = Some(id)))
            }))
            .on_click(cx.listener(move |this, ix: &usize, window, cx| {
                if let Some(id) = view_ids.get(*ix) {
                    this.select_view(*id, window, cx);
                }
            }));

        let menu_panel = panel.clone();
        let overflow_panel = panel.clone();
        let active = self.active_view().map(|view| view.id);
        h_flex()
            .min_w_0()
            .gap_1()
            .child(
                div()
                    .id("database-view-tabs")
                    .min_w_0()
                    .child(tabs)
                    .on_prepaint(move |_, window, _| {
                        let overflowing = tabs_scroll.max_offset().x > px(0.5);
                        if overflowing != overflow.get() {
                            overflow.set(overflowing);
                            window.on_next_frame(move |_, cx| {
                                _ = overflow_panel.update(cx, |_, cx| cx.notify());
                            });
                        }
                    })
                    .context_menu(move |menu, _, cx| {
                        let target = menu_panel.upgrade().and_then(|panel| panel.read(cx).menu_view);
                        match target {
                            Some(id) => view_menu(menu, id, can_delete, &menu_panel),
                            None => menu,
                        }
                    }),
            )
            .when_some(active, |this, id| {
                let panel = panel.clone();
                this.child(
                    Button::new("view-options")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Ellipsis)
                        .tooltip("View options")
                        .dropdown_menu(move |menu, _, _| view_menu(menu, id, can_delete, &panel)),
                )
            })
            .child({
                let panel = panel.clone();
                Button::new("add-view").ghost().xsmall().icon(IconName::Plus).tooltip("Add view").dropdown_menu(
                    move |menu, _, _| {
                        let add = |kind: ViewKind| {
                            let panel = panel.clone();
                            move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                                _ = panel.update(cx, |panel, cx| panel.create_view(kind, window, cx));
                            }
                        };
                        menu.label("Add a view")
                            .item(
                                PopupMenuItem::new("Table")
                                    .icon(view_icon(ViewKind::Table))
                                    .on_click(add(ViewKind::Table)),
                            )
                            .item(
                                PopupMenuItem::new("Board")
                                    .icon(view_icon(ViewKind::Board))
                                    .on_click(add(ViewKind::Board)),
                            )
                    },
                )
            })
    }

    pub(super) fn open_rename_view(&mut self, id: ViewId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.views().iter().find(|view| view.id == id).map(|view| view.name.clone()) else {
            return;
        };
        let input = self.view_name.clone();
        input.update(cx, |input, cx| input.set_value(name, window, cx));
        let panel = cx.weak_entity();
        let dialog_input = input.clone();
        window.open_dialog(cx, move |dialog, window, _| {
            let panel = panel.clone();
            let input = dialog_input.clone();
            dialog
                .title("Rename view")
                .width(window.rem_size() * 24.)
                .child(Input::new(&input).id("view-name"))
                .footer(
                    gpui_kit::component::dialog::DialogFooter::new()
                        .child(
                            gpui_kit::component::dialog::DialogClose::new()
                                .child(Button::new("cancel-rename-view").outline().label("Cancel")),
                        )
                        .child(
                            gpui_kit::component::dialog::DialogAction::new()
                                .child(Button::new("save-view-name").primary().label("Rename")),
                        ),
                )
                .on_ok(move |_, _, cx| {
                    let name = input.read(cx).value().to_string();
                    _ = panel.update(cx, |panel, cx| panel.rename_view(id, &name, cx));
                    true
                })
        });
        input.update(cx, |input, cx| input.focus(window, cx));
    }

    pub(super) fn confirm_delete_view(&mut self, id: ViewId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.views().iter().find(|view| view.id == id).map(|view| view.name.clone()) else {
            return;
        };
        let panel = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let panel = panel.clone();
            alert
                .title(SharedString::from(format!("Delete view “{name}”?")))
                .description("Videos are not affected.")
                .confirm()
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    _ = panel.update(cx, |panel, cx| panel.delete_view(id, window, cx));
                    true
                })
        });
    }
}

fn view_menu(menu: PopupMenu, id: ViewId, can_delete: bool, panel: &WeakEntity<DatabasePanel>) -> PopupMenu {
    let rename = panel.clone();
    let delete = panel.clone();
    menu.item(PopupMenuItem::new("Rename…").icon(IconName::Pencil).on_click(move |_, window, cx| {
        _ = rename.update(cx, |panel, cx| panel.open_rename_view(id, window, cx));
    }))
    .separator()
    .item(PopupMenuItem::new("Delete view…").icon(IconName::Trash).disabled(!can_delete).on_click(
        move |_, window, cx| {
            _ = delete.update(cx, |panel, cx| panel.confirm_delete_view(id, window, cx));
        },
    ))
}

/// A ghost toolbar trigger that keeps its pressed look while its popover is open.
fn toolbar_button(id: &'static str, icon: IconName, label: &'static str) -> Button {
    Button::new(id).ghost().small().icon(icon).label(label)
}

fn section_label(label: &'static str, cx: &App) -> Div {
    div().text_xs().font_weight(FontWeight::MEDIUM).text_color(cx.theme().muted_foreground).child(label)
}

fn filter_popover(filter: &FilterSpec, panel: &WeakEntity<DatabasePanel>, cx: &App) -> AnyElement {
    let count = active_filter_count(filter);
    let trigger = toolbar_button("filter", IconName::ListFilter, "Filter")
        .when(count > 0, |this| this.child(count_badge(count, cx)));
    let filter = filter.clone();
    let panel = panel.clone();

    Popover::new("filter-popover")
        .anchor(Anchor::TopRight)
        .trigger(trigger)
        .content(move |_, _, cx| {
            let edit = |change: fn(&mut FilterSpec, usize), ix: usize| {
                let panel = panel.clone();
                move |_: &bool, _: &mut Window, cx: &mut App| {
                    _ = panel.update(cx, |panel, cx| panel.update_view(|view| change(&mut view.filter, ix), cx));
                }
            };
            v_flex()
                .id("filter-content")
                .w_64()
                .gap_3()
                .child(v_flex().gap_1p5().child(section_label("Status", cx)).children(
                    VideoStatus::ALL.into_iter().enumerate().map(|(ix, status)| {
                        Checkbox::new(SharedString::from(format!("filter-status-{}", status.label())))
                            .accessibility_label(status.label())
                            .checked(filter.statuses.contains(&status))
                            .on_click(edit(toggle_status, ix))
                            .child(h_flex().child(Chip::new(status.label(), status.color()).dot(true)))
                    }),
                ))
                .child(v_flex().gap_1p5().child(section_label("Format", cx)).children(
                    VideoFormat::ALL.into_iter().enumerate().map(|(ix, format)| {
                        Checkbox::new(SharedString::from(format!("filter-format-{}", format.label())))
                            .accessibility_label(format.label())
                            .checked(filter.formats.contains(&format))
                            .on_click(edit(toggle_format, ix))
                            .child(h_flex().child(Chip::new(format.label(), format_color(format))))
                    }),
                ))
                .child(
                    Switch::new("filter-hide-archived").label("Hide archived").checked(filter.hide_archived).on_click(
                        {
                            let panel = panel.clone();
                            move |hide: &bool, _, cx| {
                                let hide = *hide;
                                _ = panel.update(cx, |panel, cx| {
                                    panel.update_view(|view| view.filter.hide_archived = hide, cx)
                                });
                            }
                        },
                    ),
                )
                .when(active_filter_count(&filter) > 0, |this| {
                    let panel = panel.clone();
                    this.child(h_flex().child(
                        Button::new("filter-clear").ghost().small().icon(IconName::X).label("Clear filters").on_click(
                            move |_, window, cx| {
                                _ = panel.update(cx, |panel, cx| panel.clear_filters(window, cx));
                            },
                        ),
                    ))
                })
        })
        .into_any_element()
}

fn toggle_status(filter: &mut FilterSpec, ix: usize) {
    toggle(&mut filter.statuses, VideoStatus::ALL[ix]);
    filter.statuses.sort();
}

fn toggle_format(filter: &mut FilterSpec, ix: usize) {
    toggle(&mut filter.formats, VideoFormat::ALL[ix]);
}

fn toggle<T: PartialEq>(values: &mut Vec<T>, value: T) {
    if let Some(ix) = values.iter().position(|known| *known == value) {
        values.remove(ix);
    } else {
        values.push(value);
    }
}

type SortsEdit = Box<dyn Fn(&mut Vec<SortSpec>)>;

/// The Sort popover: one row per sort rule, and the list of columns a rule
/// can sort by. Choosing a column happens inside the popover, so the popover
/// stays open to set the direction or add another rule.
fn sort_popover(
    view: &DatabaseView,
    properties: &[PropertyDef],
    picker: Option<SortPicker>,
    panel: &WeakEntity<DatabasePanel>,
    cx: &App,
) -> AnyElement {
    let sorts = view.sorts.clone();
    let columns: Vec<Column> = resolve_columns(view, properties).into_iter().map(|layout| layout.column).collect();
    let properties: Vec<PropertyDef> = properties.to_vec();
    let panel = panel.clone();
    let trigger = toolbar_button("sort", IconName::ArrowUpDown, "Sort")
        .when(!sorts.is_empty(), |this| this.child(count_badge(sorts.len(), cx)));
    let close_picker = panel.clone();

    Popover::new("sort-popover")
        .anchor(Anchor::TopRight)
        .trigger(trigger)
        .on_open_change(move |open, _, cx| {
            if !open {
                _ = close_picker.update(cx, |panel, cx| panel.set_sort_picker(None, cx));
            }
        })
        .content(move |_, _, cx| match picker {
            Some(picker) => sort_column_list(picker, &sorts, &columns, &properties, &panel, cx),
            None => sort_rules(&sorts, &columns, &properties, &panel, cx),
        })
        .into_any_element()
}

fn sort_rules(
    sorts: &[SortSpec],
    columns: &[Column],
    properties: &[PropertyDef],
    panel: &WeakEntity<DatabasePanel>,
    cx: &App,
) -> AnyElement {
    let edit = |change: SortsEdit| {
        let panel = panel.clone();
        move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
            _ = panel.update(cx, |panel, cx| panel.update_view(|view| change(&mut view.sorts), cx));
        }
    };
    let pick = |picker: SortPicker| {
        let panel = panel.clone();
        move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
            _ = panel.update(cx, |panel, cx| panel.set_sort_picker(Some(picker), cx));
        }
    };
    let has_unused = columns.iter().any(|column| !sorts.iter().any(|sort| sort.column == *column));

    v_flex()
        .id("sort-content")
        .w_80()
        .gap_2()
        .when(sorts.is_empty(), |this| {
            this.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("No sorts. Videos keep their manual order."),
            )
        })
        .children(sorts.iter().enumerate().map(|(ix, sort)| {
            // A column has at most one rule, so it names the rule's
            // controls; `ix` only addresses the edit.
            let key = column_key(sort.column);
            let direction = sort.direction;
            let label = column_name(sort.column, properties).unwrap_or_default();
            h_flex()
                .gap_1()
                .child(
                    Button::new(SharedString::from(format!("sort-column-{key}")))
                        .outline()
                        .small()
                        .flex_1()
                        .min_w_0()
                        .accessibility_label(label.clone())
                        // Icon and label form one child so the caret's
                        // `justify_between` keeps them at the leading edge.
                        .child(
                            h_flex()
                                .gap_1p5()
                                .min_w_0()
                                .child(Icon::new(column_icon(sort.column, properties)).xsmall())
                                .child(div().min_w_0().truncate().child(label)),
                        )
                        .dropdown_caret(true)
                        .on_click(pick(SortPicker::Rule(ix))),
                )
                .child(
                    Button::new(SharedString::from(format!("sort-direction-{key}")))
                        .outline()
                        .small()
                        .w_32()
                        .icon(match direction {
                            SortDirection::Ascending => IconName::ArrowUp,
                            SortDirection::Descending => IconName::ArrowDown,
                        })
                        .label(match direction {
                            SortDirection::Ascending => "Ascending",
                            SortDirection::Descending => "Descending",
                        })
                        .on_click(edit(Box::new(move |sorts| {
                            if let Some(sort) = sorts.get_mut(ix) {
                                sort.direction = match sort.direction {
                                    SortDirection::Ascending => SortDirection::Descending,
                                    SortDirection::Descending => SortDirection::Ascending,
                                };
                            }
                        }))),
                )
                .child(
                    Button::new(SharedString::from(format!("sort-remove-{key}")))
                        .ghost()
                        .small()
                        .icon(IconName::X)
                        .tooltip("Remove sort")
                        .on_click(edit(Box::new(move |sorts| {
                            if ix < sorts.len() {
                                sorts.remove(ix);
                            }
                        }))),
                )
        }))
        .child(
            h_flex()
                .gap_1()
                .child(
                    Button::new("sort-add")
                        .ghost()
                        .small()
                        .icon(IconName::Plus)
                        .label("Add sort")
                        .disabled(!has_unused)
                        .on_click(pick(SortPicker::New)),
                )
                .when(!sorts.is_empty(), |this| {
                    this.child(
                        Button::new("sort-clear")
                            .ghost()
                            .small()
                            .label("Clear sorts")
                            .on_click(edit(Box::new(|sorts| sorts.clear()))),
                    )
                }),
        )
        .into_any_element()
}

/// The columns a sort rule can use, shown in place of the rules. A new rule
/// is offered the columns no rule sorts by yet.
fn sort_column_list(
    picker: SortPicker,
    sorts: &[SortSpec],
    columns: &[Column],
    properties: &[PropertyDef],
    panel: &WeakEntity<DatabasePanel>,
    cx: &App,
) -> AnyElement {
    let current = match picker {
        SortPicker::Rule(ix) => sorts.get(ix).map(|sort| sort.column),
        SortPicker::New => None,
    };
    let choices = columns
        .iter()
        .copied()
        .filter(|column| picker != SortPicker::New || !sorts.iter().any(|sort| sort.column == *column));
    let back = panel.clone();

    v_flex()
        .id("sort-content")
        .w_80()
        .gap_1()
        .child(
            h_flex()
                .gap_1()
                .child(
                    Button::new("sort-back")
                        .ghost()
                        .xsmall()
                        .icon(IconName::ChevronLeft)
                        .tooltip("Back to sorts")
                        .on_click(move |_, _, cx| {
                            _ = back.update(cx, |panel, cx| panel.set_sort_picker(None, cx));
                        }),
                )
                .child(section_label("Sort by", cx)),
        )
        .child(v_flex().id("sort-columns").max_h(rems(20.)).overflow_y_scroll().children(choices.map(|column| {
            let panel = panel.clone();
            Button::new(SharedString::from(format!("sort-field-{}", column_key(column))))
                .ghost()
                .small()
                .w_full()
                .child(
                    h_flex()
                        .flex_1()
                        .gap_1p5()
                        .min_w_0()
                        .child(
                            Icon::new(column_icon(column, properties)).xsmall().text_color(cx.theme().muted_foreground),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(column_name(column, properties).unwrap_or_default()),
                        )
                        .when(current == Some(column), |this| this.child(Icon::new(IconName::Check).xsmall())),
                )
                .on_click(move |_, _, cx| {
                    _ = panel.update(cx, |panel, cx| panel.choose_sort_column(picker, column, cx));
                })
        })))
        .into_any_element()
}

/// Points sort rule `ix` at `column`, dropping another rule on that column.
fn set_sort_column(sorts: &mut Vec<SortSpec>, ix: usize, column: Column) {
    if ix >= sorts.len() {
        return;
    }
    sorts[ix].column = column;
    let mut seen = Vec::new();
    sorts.retain(|sort| {
        let first = !seen.contains(&sort.column);
        seen.push(sort.column);
        first
    });
}

fn properties_popover(
    view: &DatabaseView,
    properties: &[PropertyDef],
    panel: &WeakEntity<DatabasePanel>,
    _: &App,
) -> AnyElement {
    let columns = resolve_columns(view, properties);
    let properties = properties.to_vec();
    let panel = panel.clone();

    Popover::new("properties-popover")
        .anchor(Anchor::TopRight)
        .trigger(toolbar_button("properties", IconName::SlidersHorizontal, "Properties"))
        .content(move |_, _, cx| {
            let popover = cx.entity();
            v_flex()
                .id("properties-content")
                .w_64()
                .gap_1()
                .child(section_label("Shown in table", cx))
                .children(columns.iter().map(|layout| {
                    let column = layout.column;
                    let is_title = column == Column::Builtin(BuiltinField::Title);
                    let panel = panel.clone();
                    h_flex()
                        .gap_2()
                        .py_0p5()
                        .child(
                            Icon::new(column_icon(column, &properties))
                                .xsmall()
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .child(column_name(column, &properties).unwrap_or_default()),
                        )
                        .child(
                            Switch::new(SharedString::from(format!("show-{}", column_key(column))))
                                .small()
                                .checked(layout.visible)
                                .disabled(is_title)
                                .on_click(move |visible: &bool, _, cx| {
                                    let visible = *visible;
                                    _ = panel.update(cx, |panel, cx| panel.set_column_visible(column, visible, cx));
                                }),
                        )
                }))
                .child(h_flex().pt_1().child({
                    let panel = panel.clone();
                    let popover = popover.clone();
                    Button::new("new-property").ghost().small().icon(IconName::Plus).label("New property…").on_click(
                        move |_, window, cx| {
                            popover.update(cx, |popover, cx| popover.dismiss(window, cx));
                            _ = panel.update(cx, |panel, cx| {
                                panel.open_property_editor(None, PropertyEditorMode::Create, window, cx)
                            });
                        },
                    )
                }))
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choosing_a_sorted_column_replaces_its_other_rule() {
        let title = Column::Builtin(BuiltinField::Title);
        let status = Column::Builtin(BuiltinField::Status);
        let mut sorts = vec![
            SortSpec { column: title, direction: SortDirection::Ascending },
            SortSpec { column: status, direction: SortDirection::Descending },
        ];
        set_sort_column(&mut sorts, 1, title);
        assert_eq!(sorts, vec![SortSpec { column: title, direction: SortDirection::Ascending }]);
    }

    #[test]
    fn status_filters_toggle_and_stay_ordered() {
        let mut filter = FilterSpec::default();
        toggle_status(&mut filter, 4);
        toggle_status(&mut filter, 0);
        assert_eq!(filter.statuses, vec![VideoStatus::Idea, VideoStatus::Ready]);
        toggle_status(&mut filter, 0);
        assert_eq!(filter.statuses, vec![VideoStatus::Ready]);
    }
}
