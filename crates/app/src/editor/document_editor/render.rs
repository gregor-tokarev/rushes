//! Presentation of the document editor: block rows, gutter, menus.

use std::ops::Range;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::base::input::Textarea;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Side, Sizable as _, ThemeStyled as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, Bounds, ClickEvent, Context, DragMoveEvent, ElementId, FontStyle,
    FontWeight, HighlightStyle, InteractiveElement as _, InteractiveText, IntoElement, MouseButton, MouseDownEvent,
    ParentElement as _, Pixels, Rems, Render, SharedString, StatefulInteractiveElement as _, StrikethroughStyle,
    Styled as _, StyledText, TestSupportExt as _, UnderlineStyle, WeakEntity, Window, anchored, deferred, div,
    prelude::FluentBuilder as _, px, rems,
};
use rushes_core::input::normalize_link;
use rushes_core::{Block, BlockId, BlockKind};

use super::geometry::{self, RenderedRow};
use super::{BlockMenu, DocumentEditor, DraggedBlock, DropTarget, SlashMenu};
use crate::editor::blocks::{self, number_label};
use crate::editor::inline::RichText;
use crate::editor::markdown::is_list;
use crate::editor::palette::EditorPalette;
use crate::editor::slash::BLOCK_TYPES;
use crate::ui::{menu_item, notification, scrolling_menu};

/// Width of the gutter holding the add button and drag handle. The same
/// space is reserved on the trailing side so text stays centered.
pub(super) const GUTTER: Rems = Rems(3.5);
/// Readable width of the text column.
const TEXT_WIDTH: Rems = Rems(46.);
/// Tallest the slash menu grows before it scrolls.
const SLASH_MENU_MAX_HEIGHT: Rems = Rems(22.);
/// Indent of one list level.
const INDENT_STEP: Rems = Rems(1.5);
/// The input engine wraps text this far before its right edge, so rendered
/// text reserves the same space to wrap identically when a block switches
/// between editing and reading (gpui-base `RIGHT_MARGIN`).
const INPUT_WRAP_MARGIN: f32 = 10.;

/// Element id of one part of a block, stable across reordering.
fn block_element_id(id: BlockId, part: &'static str) -> ElementId {
    ElementId::NamedChild(Arc::new(ElementId::Uuid(id.0)), part.into())
}

/// Hint of an empty document: no blocks, or one empty paragraph.
const EMPTY_DOCUMENT_HINT: &str = "Start writing, or type '/' for commands";

/// Placeholder of an empty block of this kind.
fn placeholder(kind: BlockKind) -> &'static str {
    match kind {
        BlockKind::Paragraph => "Type '/' for commands",
        BlockKind::Heading1 => "Heading 1",
        BlockKind::Heading2 => "Heading 2",
        BlockKind::Heading3 => "Heading 3",
        BlockKind::Bulleted | BlockKind::Numbered => "List",
        BlockKind::Todo => "To-do",
        BlockKind::Quote => "Empty quote",
        BlockKind::Callout => "Type something…",
        BlockKind::Code => "Code",
        BlockKind::Divider => "",
        BlockKind::TalkingPoint => "The point to make in your own words…",
        BlockKind::BRoll => "Describe the shot…",
        BlockKind::OnScreen => "Text shown on screen…",
    }
}

/// What an empty block shows in place of its text: the template's guidance,
/// the empty-document hint when it is the document's only paragraph, or the
/// hint of its kind.
fn placeholder_text(block: &Block, lone: bool) -> &str {
    match block.placeholder.as_deref() {
        Some(guidance) => guidance,
        None if lone && block.kind == BlockKind::Paragraph => EMPTY_DOCUMENT_HINT,
        None => placeholder(block.kind),
    }
}

/// The placeholder a block's input shows. Template guidance is drawn by the
/// editor instead, because the input lays its placeholder out on one line and
/// guidance is often longer than that.
pub(super) fn input_placeholder(block: &Block, lone: bool) -> SharedString {
    match block.placeholder {
        Some(_) => SharedString::default(),
        None => placeholder_text(block, lone).to_owned().into(),
    }
}

/// Opens a link from a document in the browser or mail app. Anything else
/// (`file:`, `javascript:`, app schemes) is refused with a short notice, so a
/// pasted or imported document cannot launch local files or programs.
fn open_link(url: &str, window: &mut Window, cx: &mut App) {
    match normalize_link(url) {
        Some(url) => cx.open_url(&url),
        None => window.push_notification(
            notification::warning("Only web and email links open from a document.", cx).title("Link not opened"),
            cx,
        ),
    }
}

/// Typography and vertical rhythm of a block kind.
#[derive(Clone, Copy)]
struct Metrics {
    text_size: Rems,
    line_height: Rems,
    /// Space above the row; headings get more to separate sections.
    space_above: Rems,
    /// Inset from the row top to the first line inside a boxed block.
    inset: Rems,
}

impl Metrics {
    fn of(kind: BlockKind) -> Self {
        let body = Metrics { text_size: Rems(1.), line_height: Rems(1.5), space_above: Rems(0.125), inset: Rems(0.) };
        match kind {
            BlockKind::Heading1 => {
                Metrics { text_size: Rems(1.875), line_height: Rems(2.375), space_above: Rems(1.5), ..body }
            }
            BlockKind::Heading2 => {
                Metrics { text_size: Rems(1.5), line_height: Rems(2.), space_above: Rems(1.125), ..body }
            }
            BlockKind::Heading3 => {
                Metrics { text_size: Rems(1.25), line_height: Rems(1.75), space_above: Rems(0.75), ..body }
            }
            BlockKind::Code => {
                Metrics { text_size: Rems(0.875), line_height: Rems(1.375), space_above: Rems(0.5), inset: Rems(1.) }
            }
            BlockKind::Callout => Metrics { space_above: Rems(0.375), inset: Rems(0.875), ..body },
            BlockKind::TalkingPoint | BlockKind::BRoll | BlockKind::OnScreen => {
                Metrics { space_above: Rems(0.375), inset: Rems(0.5), ..body }
            }
            _ => body,
        }
    }
}

/// What a block row needs from the editor-wide render pass.
struct RowContext {
    palette: EditorPalette,
    editing: Option<BlockId>,
    dragging: bool,
    /// The document has a single block, which shows the empty-document hint
    /// while it is an empty paragraph.
    lone: bool,
}

impl Render for DocumentEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let row = RowContext {
            palette: EditorPalette::new(cx),
            editing: self.editing(window, cx).map(|(ix, _)| self.document.blocks[ix].id),
            dragging: cx.has_active_drag(),
            lone: self.document.blocks.len() == 1,
        };
        let numbers = blocks::numbering(&self.document.blocks);
        // Blocks that must stay interactive even when far from the viewport.
        let pinned = [
            row.editing,
            self.focused,
            self.hovered,
            self.selected,
            self.block_menu.as_ref().map(|menu| menu.block),
            self.slash.as_ref().map(|menu| menu.block),
            self.drop_target.map(|target| target.block),
        ];
        let rem_size = window.rem_size();
        let geometry = self.geometry.borrow();
        let mut rendered = Vec::with_capacity(self.document.blocks.len());
        let rows: Vec<AnyElement> = self
            .document
            .blocks
            .iter()
            .zip(&numbers)
            .map(|(block, number)| {
                let fingerprint = geometry::fingerprint(block, *number);
                let spacer = (!pinned.contains(&Some(block.id)))
                    .then(|| geometry.spacer_height(block.id, fingerprint, rem_size))
                    .flatten();
                rendered.push(RenderedRow { id: block.id, fingerprint, spacer: spacer.is_some() });
                match spacer {
                    Some(height) => div().w_full().h(height).into_any_element(),
                    None => self.render_block(block, *number, &row, window, cx),
                }
            })
            .collect();
        drop(geometry);
        let record = {
            let geometry = self.geometry.clone();
            let scroll = self.scroll_handle.clone();
            let covered_top = self.covered_top.clone();
            let focused = self.focused;
            move |bounds: Vec<Bounds<Pixels>>, window: &mut Window, _: &mut App| {
                geometry.borrow_mut().record(&rendered, &bounds, focused, scroll.as_ref(), covered_top.get(), window)
            }
        };

        v_flex()
            .id("document-editor")
            .key_context(crate::editor::CONTEXT)
            .track_focus(&self.focus_handle)
            .capture_action(cx.listener(Self::on_enter))
            .capture_action(cx.listener(Self::on_backspace))
            .capture_action(cx.listener(Self::on_delete))
            .capture_action(cx.listener(Self::on_move_up))
            .capture_action(cx.listener(Self::on_move_down))
            .capture_action(cx.listener(Self::on_move_left))
            .capture_action(cx.listener(Self::on_move_right))
            .capture_action(cx.listener(Self::on_indent))
            .capture_action(cx.listener(Self::on_outdent))
            .capture_action(cx.listener(Self::on_escape))
            .capture_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::toggle_bold))
            .on_action(cx.listener(Self::toggle_italic))
            .on_action(cx.listener(Self::toggle_code))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::delete_selected))
            .on_action(cx.listener(Self::insert_below_selected))
            .on_action(cx.listener(Self::clear_selection))
            .on_action(cx.listener(Self::open_focused_block_menu))
            .w_full()
            .flex_1()
            .items_center()
            .text_color(row.palette.text)
            .child(
                v_flex()
                    .w_full()
                    .flex_1()
                    .max_w(Rems(TEXT_WIDTH.0 + 2. * GUTTER.0))
                    .children(rows)
                    .on_children_prepainted(record)
                    .child(
                        // The empty area below the last block, generous so a
                        // short or empty document is easy to click into.
                        div()
                            .id("document-end")
                            .test_support()
                            .w_full()
                            .flex_1()
                            .min_h(rems(10.))
                            .cursor_text()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                    // The root would otherwise take focus back from the new block.
                                    cx.stop_propagation();
                                    this.focus_end(window, cx)
                                }),
                            )
                            .when(self.document.blocks.is_empty(), |this| {
                                this.child(
                                    div()
                                        .px(GUTTER)
                                        .line_height(Metrics::of(BlockKind::Paragraph).line_height)
                                        .text_color(row.palette.placeholder)
                                        .child(EMPTY_DOCUMENT_HINT),
                                )
                            }),
                    ),
            )
            .when_some(self.block_menu.as_ref(), |this, menu| this.child(render_block_menu(menu)))
    }
}

impl DocumentEditor {
    fn render_block(
        &self,
        block: &Block,
        number: usize,
        row: &RowContext,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = block.id;
        let metrics = Metrics::of(block.kind);
        let menu_open = self.block_menu.as_ref().is_some_and(|menu| menu.block == id);
        let highlighted = menu_open || self.selected == Some(id);
        // Only the hovered row builds its gutter buttons; with hundreds of
        // blocks, hidden buttons on every row dominate the frame time.
        let gutter = if menu_open || self.hovered == Some(id) {
            self.render_gutter(block, metrics, row.palette, cx).into_any_element()
        } else {
            div().flex_none().w(GUTTER).into_any_element()
        };
        let drop_after =
            self.drop_target.filter(|target| row.dragging && target.block == id).map(|target| target.after);
        let indent = if is_list(block.kind) { block.indent.min(blocks::MAX_INDENT) } else { 0 };

        let content = div()
            .relative()
            .flex_1()
            .min_w_0()
            .ml(Rems(INDENT_STEP.0 * f32::from(indent)))
            .rounded(cx.theme().radius)
            .when(highlighted, |this| this.bg(row.palette.selected))
            .text_size(metrics.text_size)
            .line_height(metrics.line_height)
            .when(block.kind.has_text(), |this| {
                // Clicks on a block's padding or marker put the caret at its end.
                this.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                        // The focused block's input already placed the caret
                        // (focusing an element prevents the default).
                        if window.default_prevented() {
                            return;
                        }
                        cx.stop_propagation();
                        this.click_text(id, usize::MAX, window, cx);
                    }),
                )
            })
            .child(self.render_content(block, number, metrics, row, cx))
            .when_some(self.slash.as_ref().filter(|menu| menu.block == id && row.editing == Some(id)), |this, menu| {
                let above = self.slash_menu_opens_above(menu, window, cx);
                let anchor = div().absolute().left_0().map(|this| if above { this.top_0() } else { this.top_full() });
                this.child(anchor.child(self.render_slash_menu(menu, above, cx)))
            });

        h_flex()
            .id(block_element_id(id, "row"))
            .test_support()
            .relative()
            .w_full()
            .items_start()
            .pt(metrics.space_above)
            .pb(Rems(0.125))
            .child(gutter)
            .child(content)
            .child(div().flex_none().w(GUTTER))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| this.set_hovered(id, *hovered, cx)))
            .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<DraggedBlock>, _, cx| {
                if !event.bounds.contains(&event.event.position) {
                    return;
                }
                let target = DropTarget { block: id, after: event.event.position.y > event.bounds.center().y };
                if this.drop_target != Some(target) {
                    this.drop_target = Some(target);
                    cx.notify();
                }
            }))
            .on_drop(
                cx.listener(move |this, dragged: &DraggedBlock, window, cx| {
                    this.drop_block(dragged.id, id, window, cx)
                }),
            )
            .when_some(drop_after, |this, after| {
                this.child(
                    div()
                        .absolute()
                        .left(GUTTER)
                        .right(GUTTER)
                        .h_0p5()
                        .rounded_full()
                        .bg(row.palette.drop_indicator)
                        .map(|this| if after { this.bottom_0() } else { this.top_0() }),
                )
            })
            .into_any_element()
    }

    /// The add button and drag handle of a hovered row.
    fn render_gutter(
        &self,
        block: &Block,
        metrics: Metrics,
        palette: EditorPalette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = block.id;
        let label: SharedString = match block.text.lines().next().filter(|line| !line.trim().is_empty()) {
            Some(line) => line.chars().take(60).collect::<String>().into(),
            None => block.kind.label().into(),
        };
        let mut handle = Button::new(block_element_id(id, "handle"))
            .ghost()
            .xsmall()
            .icon(Icon::new(IconName::GripVertical).text_color(palette.muted))
            .tooltip("Drag to move, click for options")
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.open_block_menu(id, event.position(), window, cx)
            }));
        handle.interactivity().on_drag(DraggedBlock { id, label }, |dragged, _, _, cx| {
            let label = dragged.label.clone();
            cx.new(|_| DragPreview { label })
        });

        h_flex()
            .flex_none()
            .w(GUTTER)
            .h(metrics.line_height)
            .mt(metrics.inset)
            .pr_1()
            .gap_0p5()
            .justify_end()
            .child(
                Button::new(block_element_id(id, "add"))
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::Plus).text_color(palette.muted))
                    .tooltip("Add a block below")
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, window, cx| this.insert_with_menu(id, window, cx)),
                    ),
            )
            .child(handle)
    }

    fn render_content(
        &self,
        block: &Block,
        number: usize,
        metrics: Metrics,
        row: &RowContext,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = row.palette;
        let radius = cx.theme().radius;
        let mono = cx.theme().mono_font_family.clone();
        let text = self.render_text(block, row, cx);
        let marker = |child: AnyElement| {
            div().flex_none().w(INDENT_STEP).h(metrics.line_height).flex().items_center().justify_center().child(child)
        };
        match block.kind {
            BlockKind::Paragraph => text,
            BlockKind::Heading1 | BlockKind::Heading2 | BlockKind::Heading3 => {
                div().font_weight(FontWeight::SEMIBOLD).child(text).into_any_element()
            }
            BlockKind::Bulleted => h_flex()
                .items_start()
                .child(marker(div().child(blocks::bullet(block.indent)).into_any_element()))
                .child(text)
                .into_any_element(),
            BlockKind::Numbered => h_flex()
                .items_start()
                .child(marker(div().child(number_label(number, block.indent)).into_any_element()))
                .child(text)
                .into_any_element(),
            BlockKind::Todo => {
                let id = block.id;
                let checkbox = Checkbox::new(block_element_id(id, "check"))
                    .checked(block.checked)
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| this.set_checked(id, *checked, cx)));
                h_flex()
                    .items_start()
                    .child(marker(
                        // Toggling the checkbox does not move the caret into the item.
                        div()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(checkbox)
                            .into_any_element(),
                    ))
                    .child(text)
                    .into_any_element()
            }
            BlockKind::Quote => {
                div().border_l_3().border_color(palette.quote_bar).pl_4().my_1().child(text).into_any_element()
            }
            BlockKind::Callout => h_flex()
                .items_start()
                .gap_2()
                .px_4()
                .py(metrics.inset)
                .rounded(radius)
                .bg(palette.surface)
                .child(marker(Icon::new(IconName::Lightbulb).text_color(palette.muted).into_any_element()))
                .child(text)
                .into_any_element(),
            BlockKind::Code => div()
                .px_4()
                .py(metrics.inset)
                .rounded(radius)
                .bg(palette.surface)
                .font_family(mono)
                .child(text)
                .into_any_element(),
            BlockKind::Divider => {
                let id = block.id;
                div()
                    .id(block_element_id(id, "divider"))
                    .test_support()
                    .w_full()
                    .h(metrics.line_height)
                    .flex()
                    .items_center()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.select_block(id, window, cx);
                        }),
                    )
                    // A one-device-pixel rule, like the theme's separators.
                    .child(div().w_full().h(px(1.)).bg(palette.rule))
                    .into_any_element()
            }
            BlockKind::TalkingPoint | BlockKind::BRoll | BlockKind::OnScreen => {
                let (colors, icon, label) = match block.kind {
                    BlockKind::TalkingPoint => (palette.talking_point, IconName::Mic, "TALKING POINT"),
                    BlockKind::BRoll => (palette.broll, IconName::Clapperboard, "B-ROLL"),
                    _ => (palette.onscreen, IconName::Type, "ON SCREEN"),
                };
                h_flex()
                    .items_start()
                    .gap_3()
                    .px_3()
                    .py(metrics.inset)
                    .rounded(radius)
                    .bg(colors.fill)
                    .child(
                        h_flex()
                            .flex_none()
                            .h(metrics.line_height)
                            .gap_1p5()
                            .text_color(colors.label)
                            .child(Icon::new(icon).small())
                            .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).child(label)),
                    )
                    .child(text)
                    .into_any_element()
            }
        }
    }

    /// The block's text: an input while it has focus, formatted text
    /// otherwise. Both inherit the block's typography and wrap at the same
    /// width so switching never changes the block's height.
    fn render_text(&self, block: &Block, row: &RowContext, cx: &mut Context<Self>) -> AnyElement {
        let id = block.id;
        let palette = row.palette;
        let cell = div().id(block_element_id(id, "text")).test_support().flex_1().min_w_0();

        if row.editing == Some(id)
            && let Some(input) = self.inputs.get(&id)
        {
            // The invisible copy of the raw text sizes the cell from the first
            // frame; the input's own auto-grow catches up a frame later.
            // Template guidance is that copy made visible, wrapped like text.
            let guidance = block.guidance();
            let sizer = match guidance {
                Some(guidance) => guidance.to_owned(),
                None if block.text.is_empty() => placeholder_text(block, row.lone).to_owned(),
                None => block.text.clone(),
            };
            return cell
                .grid()
                .grid_cols(1)
                .child(
                    div()
                        .row_start(1)
                        .col_start(1)
                        .pr(px(INPUT_WRAP_MARGIN))
                        .map(|this| match guidance {
                            Some(_) => this.text_color(palette.placeholder),
                            None => this.invisible(),
                        })
                        .child(sizer),
                )
                .child(div().row_start(1).col_start(1).w_full().child(Textarea::new(&input.state)))
                .into_any_element();
        }

        let rich = RichText::parse(&block.text);
        let cell = cell.pr(px(INPUT_WRAP_MARGIN)).cursor_text();
        if rich.text().is_empty() {
            // An empty paragraph stays blank unless it carries guidance or is
            // all the document has, which would otherwise look like nothing.
            let shows_placeholder = block.placeholder.is_some()
                || match block.kind {
                    BlockKind::Paragraph => row.lone,
                    BlockKind::Divider => false,
                    _ => true,
                };
            let hint = shows_placeholder.then(|| SharedString::from(placeholder_text(block, row.lone).to_owned()));
            return cell
                .min_h(Metrics::of(block.kind).line_height)
                .text_color(palette.placeholder)
                .when_some(hint, |this, hint| this.aria_label(hint.clone()).child(hint))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.click_text(id, 0, window, cx);
                    }),
                )
                .into_any_element();
        }

        let completed = block.kind == BlockKind::Todo && block.checked;
        let mono = cx.theme().mono_font_family.clone();
        let mut highlights = Vec::new();
        let mut mono_ranges = Vec::new();
        for span in rich.spans() {
            let mut style = HighlightStyle::default();
            if span.style.bold {
                style.font_weight = Some(FontWeight::BOLD);
            }
            if span.style.italic {
                style.font_style = Some(FontStyle::Italic);
            }
            if span.style.code {
                style.background_color = Some(palette.surface);
                mono_ranges.push((span.range.clone(), mono.clone()));
            }
            if span.link.is_some() {
                style.color = Some(palette.link);
                style.underline = Some(UnderlineStyle { thickness: px(1.), color: Some(palette.link), wavy: false });
            }
            if completed {
                style.color = Some(palette.muted);
                style.strikethrough = Some(StrikethroughStyle { thickness: px(1.), color: Some(palette.muted) });
            }
            if style != HighlightStyle::default() {
                highlights.push((span.range.clone(), style));
            }
        }

        let (link_ranges, urls): (Vec<Range<usize>>, Vec<String>) =
            rich.links().map(|(range, url)| (range, url.to_owned())).unzip();
        let styled =
            StyledText::new(rich.text().to_owned()).with_highlights(highlights).with_font_family_overrides(mono_ranges);
        let layout = styled.layout().clone();
        let text = InteractiveText::new(block_element_id(id, "rich"), styled)
            .on_click(link_ranges.clone(), move |ix, window, cx| open_link(&urls[ix], window, cx));

        cell.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                let rendered = layout.index_for_position(event.position).unwrap_or_else(|closest| closest);
                cx.stop_propagation();
                // Links open on click and leave the block in reading mode.
                if link_ranges.iter().any(|range| range.contains(&rendered)) {
                    return;
                }
                let offset = rich.source_offset(rendered);
                this.click_text(id, offset, window, cx);
            }),
        )
        .child(text)
        .into_any_element()
    }

    /// Opens the slash menu above its block when the space below, judged
    /// from the input's previous layout, cannot fit it but the space above can.
    fn slash_menu_opens_above(&self, menu: &SlashMenu, window: &Window, cx: &App) -> bool {
        let Some(bounds) = self.inputs.get(&menu.block).and_then(|input| input.state.read(cx).text_bounds()) else {
            return false;
        };
        let rows = menu.items().len().max(1) as f32;
        let height = window.rem_size() * SLASH_MENU_MAX_HEIGHT.0.min(2.5 + 3. * rows);
        let below = window.viewport_size().height - bounds.bottom();
        below < height && bounds.top() > below
    }

    fn render_slash_menu(&self, menu: &SlashMenu, above: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let items = menu.items();
        let heading = if menu.query.is_empty() { "Basic blocks" } else { "Blocks" };
        let rows = items.iter().enumerate().map(|(ix, ty)| {
            let kind = ty.kind;
            let selected = ix == menu.selected;
            h_flex()
                .id(ElementId::NamedChild(Arc::new("slash-item".into()), ty.label().into()))
                .test_support()
                .gap_2p5()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .when(selected, |this| this.bg(theme.accent).text_color(theme.accent_foreground))
                .child(
                    div()
                        .flex_none()
                        .size_9()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.background)
                        .text_color(theme.foreground)
                        .child(Icon::new(ty.icon)),
                )
                .child(
                    v_flex()
                        .min_w_0()
                        .child(div().text_sm().child(ty.label()))
                        .child(div().text_xs().text_color(theme.muted_foreground).child(ty.description)),
                )
                .on_mouse_move(cx.listener(move |this, _, _, cx| {
                    if let Some(menu) = this.slash.as_mut().filter(|menu| menu.selected != ix) {
                        menu.selected = ix;
                        cx.notify();
                    }
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.choose_slash_item(kind, window, cx);
                    }),
                )
        });

        let anchor = if above { Anchor::BottomLeft } else { Anchor::TopLeft };
        deferred(
            anchored().anchor(anchor).snap_to_window_with_margin(px(8.)).child(
                v_flex()
                    .id("slash-menu")
                    .test_support()
                    .occlude()
                    .map(|this| if above { this.mb_1() } else { this.mt_1() })
                    .w(rems(20.))
                    .max_h(SLASH_MENU_MAX_HEIGHT)
                    .overflow_y_scroll()
                    .track_scroll(&menu.scroll)
                    .p_1()
                    .popover_style(cx)
                    .text_size(rems(1.))
                    .line_height(rems(1.25))
                    .font_weight(FontWeight::NORMAL)
                    .child(div().px_2().py_1().text_xs().text_color(theme.muted_foreground).child(heading))
                    .children(rows)
                    .when(items.is_empty(), |this| {
                        this.child(
                            div().px_2().py_1p5().text_sm().text_color(theme.muted_foreground).child("No results"),
                        )
                    }),
            ),
        )
        .with_priority(1)
    }
}

fn render_block_menu(menu: &BlockMenu) -> impl IntoElement {
    deferred(
        anchored()
            .position(menu.position)
            .anchor(Anchor::TopRight)
            .snap_to_window_with_margin(px(8.))
            .child(menu.menu.clone()),
    )
    .with_priority(1)
}

/// What the block menu needs to know about its block.
pub(super) struct MenuTarget {
    pub id: BlockId,
    pub kind: BlockKind,
    pub is_first: bool,
    pub is_last: bool,
}

/// Items of the block handle's menu.
pub(super) fn block_menu(
    menu: PopupMenu,
    editor: WeakEntity<DocumentEditor>,
    target: &MenuTarget,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let id = target.id;
    let current = target.kind;
    let command = |run: fn(&mut DocumentEditor, BlockId, &mut Window, &mut Context<DocumentEditor>)| {
        let editor = editor.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            editor.update(cx, |editor, cx| run(editor, id, window, cx)).ok();
        }
    };
    let turn_into = {
        let editor = editor.clone();
        move |menu: PopupMenu, window: &mut Window, _: &mut Context<PopupMenu>| {
            BLOCK_TYPES.iter().fold(scrolling_menu(menu, window).check_side(Side::Right), |menu, ty| {
                let editor = editor.clone();
                let kind = ty.kind;
                menu.item(menu_item(ty.label()).icon(Icon::new(ty.icon)).checked(kind == current).on_click(
                    move |_, window, cx| {
                        editor.update(cx, |editor, cx| editor.turn_into(id, kind, window, cx)).ok();
                    },
                ))
            })
        }
    };
    menu.min_w(window.rem_size() * 14.)
        .submenu_with_icon(Some(Icon::new(IconName::Repeat2)), "Turn into", window, cx, turn_into)
        .separator()
        .item(menu_item("Duplicate").icon(Icon::new(IconName::Copy)).on_click(command(DocumentEditor::duplicate_block)))
        .item(
            menu_item("Move up")
                .icon(Icon::new(IconName::ArrowUp))
                .disabled(target.is_first)
                .on_click(command(|editor, id, window, cx| editor.move_block_by(id, -1, window, cx))),
        )
        .item(
            menu_item("Move down")
                .icon(Icon::new(IconName::ArrowDown))
                .disabled(target.is_last)
                .on_click(command(|editor, id, window, cx| editor.move_block_by(id, 1, window, cx))),
        )
        .separator()
        .item(menu_item("Delete").icon(Icon::new(IconName::Trash)).on_click(command(DocumentEditor::delete_block)))
}

/// Floating label under the pointer while a block is dragged.
struct DragPreview {
    label: SharedString,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_2()
            .px_3()
            .py_1p5()
            .max_w(rems(20.))
            .popover_style(cx)
            .text_sm()
            .child(Icon::new(IconName::GripVertical).small().text_color(cx.theme().muted_foreground))
            .child(div().min_w_0().truncate().child(self.label.clone()))
    }
}
