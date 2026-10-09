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
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, Context, Div, FontWeight, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, SharedString, Styled as _, WeakEntity, Window, div,
};
use videocrm_core::{
    BuiltinField, Column, DatabaseView, FilterSpec, PropertyDef, SortDirection, SortSpec,
    VideoFormat, VideoStatus, ViewId, ViewKind,
};

use super::colors::format_color;
use super::elements::Chip;
use super::icons::{column_icon, view_icon};
use super::panel::DatabasePanel;
use super::property_editor::PropertyEditorMode;
use super::query::{active_filter_count, column_name, resolve_columns};

impl DatabasePanel {
    pub(super) fn render_toolbar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = self.active_view().cloned();
        let panel = cx.weak_entity();
        h_flex()
            .w_full()
            .flex_shrink_0()
            .gap_2()
            .px_4()
            .pt_2()
            .pb_2()
            .child(self.render_view_tabs(window, cx))
            .child(div().flex_1())
            .when_some(view, |this, view| {
                this.child(filter_popover(&view.filter, &panel, cx))
                    .child(sort_popover(&view, self.properties(), &panel, cx))
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
                    .primary()
                    .small()
                    .icon(IconName::Plus)
                    .label("New video")
                    .on_click(cx.listener(|this, _, _, cx| this.create_video(None, cx))),
            )
    }

    fn render_view_tabs(&self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = cx.weak_entity();
        let views: Vec<(ViewId, String, ViewKind)> =
            self.views().iter().map(|view| (view.id, view.name.clone(), view.kind)).collect();
        let view_ids: Vec<ViewId> = views.iter().map(|(id, ..)| *id).collect();
        let can_delete = views.len() > 1;

        let tabs = TabBar::new("database-views")
            .underline()
            .small()
            .when_some(self.active_index(), |this, ix| this.selected_index(ix))
            .children(views.into_iter().map(|(id, name, kind)| {
                Tab::new()
                    .label(name)
                    .icon(Icon::new(view_icon(kind)))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _, _, _| this.menu_view = Some(id)),
                    )
            }))
            .on_click(cx.listener(move |this, ix: &usize, window, cx| {
                if let Some(id) = view_ids.get(*ix) {
                    this.select_view(*id, window, cx);
                }
            }));

        let menu_panel = panel.clone();
        let active = self.active_view().map(|view| view.id);
        h_flex()
            .min_w_0()
            .gap_1()
            .child(
                div()
                    .id("database-view-tabs")
                    .min_w_0()
                    .child(tabs)
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
                Button::new("add-view")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip("Add view")
                    .dropdown_menu(move |menu, _, _| {
                        let add = |kind: ViewKind| {
                            let panel = panel.clone();
                            move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                                _ = panel.update(cx, |panel, cx| panel.create_view(kind, window, cx));
                            }
                        };
                        menu.label("Add a view")
                            .item(PopupMenuItem::new("Table").icon(view_icon(ViewKind::Table)).on_click(add(ViewKind::Table)))
                            .item(PopupMenuItem::new("Board").icon(view_icon(ViewKind::Board)).on_click(add(ViewKind::Board)))
                    })
            })
    }

    pub(super) fn open_rename_view(&mut self, id: ViewId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.views().iter().find(|view| view.id == id).map(|view| view.name.clone()) else {
            return;
        };
        let input = self.view_name.clone();
        input.update(cx, |input, cx| {
            input.set_value(name, window, cx);
            input.focus(window, cx);
        });
        let panel = cx.weak_entity();
        window.open_dialog(cx, move |dialog, window, _| {
            let panel = panel.clone();
            let input = input.clone();
            dialog
                .title("Rename view")
                .width(window.rem_size() * 24.)
                .child(Input::new(&input).id("view-name"))
                .footer(
                    gpui_kit::component::dialog::DialogFooter::new()
                        .child(gpui_kit::component::dialog::DialogClose::new().child(
                            Button::new("cancel-rename-view").outline().label("Cancel"),
                        ))
                        .child(gpui_kit::component::dialog::DialogAction::new().child(
                            Button::new("save-view-name").primary().label("Rename"),
                        )),
                )
                .on_ok(move |_, _, cx| {
                    let name = input.read(cx).value().to_string();
                    _ = panel.update(cx, |panel, cx| panel.rename_view(id, &name, cx));
                    true
                })
        });
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
    .item(
        PopupMenuItem::new("Delete view…")
            .icon(IconName::Trash)
            .disabled(!can_delete)
            .on_click(move |_, window, cx| {
                _ = delete.update(cx, |panel, cx| panel.confirm_delete_view(id, window, cx));
            }),
    )
}

/// A ghost toolbar trigger that keeps its pressed look while its popover is open.
fn toolbar_button(id: &'static str, icon: IconName, label: &'static str) -> Button {
    Button::new(id).ghost().small().icon(icon).label(label)
}

fn section_label(label: &'static str, cx: &App) -> Div {
    div()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(cx.theme().muted_foreground)
        .child(label)
}

fn filter_popover(filter: &FilterSpec, panel: &WeakEntity<DatabasePanel>, cx: &App) -> AnyElement {
    let count = active_filter_count(filter);
    let trigger = toolbar_button("filter", IconName::ListFilter, "Filter").when(count > 0, |this| {
        this.child(
            div()
                .px_1p5()
                .rounded_full()
                .text_xs()
                .bg(cx.theme().primary)
                .text_color(cx.theme().primary_foreground)
                .child(count.to_string()),
        )
    });
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
                .child(
                    v_flex()
                        .gap_1p5()
                        .child(section_label("Status", cx))
                        .children(VideoStatus::ALL.into_iter().enumerate().map(|(ix, status)| {
                            h_flex()
                                .gap_2()
                                .child(
                                    Checkbox::new(SharedString::from(format!("filter-status-{}", status.label())))
                                        .checked(filter.statuses.contains(&status))
                                        .on_click(edit(toggle_status, ix)),
                                )
                                .child(Chip::new(status.label(), status.color()).dot(true))
                        })),
                )
                .child(
                    v_flex()
                        .gap_1p5()
                        .child(section_label("Format", cx))
                        .children(VideoFormat::ALL.into_iter().enumerate().map(|(ix, format)| {
                            h_flex()
                                .gap_2()
                                .child(
                                    Checkbox::new(SharedString::from(format!("filter-format-{}", format.label())))
                                        .checked(filter.formats.contains(&format))
                                        .on_click(edit(toggle_format, ix)),
                                )
                                .child(Chip::new(format.label(), format_color(format)))
                        })),
                )
                .child(
                    Switch::new("filter-hide-archived")
                        .label("Hide archived")
                        .checked(filter.hide_archived)
                        .on_click({
                            let panel = panel.clone();
                            move |hide: &bool, _, cx| {
                                let hide = *hide;
                                _ = panel.update(cx, |panel, cx| {
                                    panel.update_view(|view| view.filter.hide_archived = hide, cx)
                                });
                            }
                        }),
                )
                .when(active_filter_count(&filter) > 0, |this| {
                    let panel = panel.clone();
                    this.child(
                        Button::new("filter-clear")
                            .ghost()
                            .small()
                            .label("Clear filters")
                            .on_click(move |_, window, cx| {
                                _ = panel.update(cx, |panel, cx| panel.clear_filters(window, cx));
                            }),
                    )
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

fn sort_popover(
    view: &DatabaseView,
    properties: &[PropertyDef],
    panel: &WeakEntity<DatabasePanel>,
    cx: &App,
) -> AnyElement {
    let sorts = view.sorts.clone();
    let columns: Vec<Column> = resolve_columns(view, properties).into_iter().map(|layout| layout.column).collect();
    let properties: Vec<PropertyDef> = properties.to_vec();
    let panel = panel.clone();
    let trigger = toolbar_button("sort", IconName::ArrowUpDown, "Sort").when(!sorts.is_empty(), |this| {
        this.text_color(cx.theme().primary)
    });

    Popover::new("sort-popover")
        .anchor(Anchor::TopRight)
        .trigger(trigger)
        .content(move |_, _, cx| {
            let edit = |change: Box<dyn Fn(&mut Vec<SortSpec>)>| {
                let panel = panel.clone();
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                    _ = panel.update(cx, |panel, cx| panel.update_view(|view| change(&mut view.sorts), cx));
                }
            };
            let unused: Vec<Column> =
                columns.iter().copied().filter(|column| !sorts.iter().any(|sort| sort.column == *column)).collect();

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
                    let direction = sort.direction;
                    let label = column_name(sort.column, &properties).unwrap_or_default();
                    let choices = columns.clone();
                    let picker_properties = properties.clone();
                    let picker_panel = panel.clone();
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new(("sort-column", ix))
                                .outline()
                                .small()
                                .flex_1()
                                .icon(column_icon(sort.column, &properties))
                                .label(label)
                                .dropdown_caret(true)
                                .dropdown_menu(move |menu, _, _| {
                                    choices.iter().fold(menu.max_h(gpui_kit::px(320.)).scrollable(true), |menu, column| {
                                        let column = *column;
                                        let panel = picker_panel.clone();
                                        menu.item(
                                            PopupMenuItem::new(column_name(column, &picker_properties).unwrap_or_default())
                                                .icon(column_icon(column, &picker_properties))
                                                .on_click(move |_, _, cx| {
                                                    _ = panel.update(cx, |panel, cx| {
                                                        panel.update_view(|view| set_sort_column(&mut view.sorts, ix, column), cx)
                                                    });
                                                }),
                                        )
                                    })
                                }),
                        )
                        .child(
                            Button::new(("sort-direction", ix))
                                .outline()
                                .small()
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
                            Button::new(("sort-remove", ix))
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
                                .disabled(unused.is_empty())
                                .on_click(edit(Box::new(move |sorts| {
                                    if let Some(column) = unused.first() {
                                        sorts.push(SortSpec { column: *column, direction: SortDirection::Ascending });
                                    }
                                }))),
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
        })
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
                        .child(Icon::new(column_icon(column, &properties)).xsmall().text_color(cx.theme().muted_foreground))
                        .child(div().flex_1().min_w_0().truncate().text_sm().child(column_name(column, &properties).unwrap_or_default()))
                        .child(
                            Switch::new(SharedString::from(format!("show-{}", super::table::column_key(column))))
                                .small()
                                .checked(layout.visible)
                                .disabled(is_title)
                                .on_click(move |visible: &bool, _, cx| {
                                    let visible = *visible;
                                    _ = panel.update(cx, |panel, cx| panel.set_column_visible(column, visible, cx));
                                }),
                        )
                }))
                .child(
                    div().pt_1().child({
                        let panel = panel.clone();
                        let popover = popover.clone();
                        Button::new("new-property")
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .label("New property…")
                            .on_click(move |_, window, cx| {
                                popover.update(cx, |popover, cx| popover.dismiss(window, cx));
                                _ = panel.update(cx, |panel, cx| {
                                    panel.open_property_editor(None, PropertyEditorMode::Create, window, cx)
                                });
                            })
                    }),
                )
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
