//! The board view: one column per status, cards dragged between and within
//! columns.

use chrono::{DateTime, Utc};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
};
use videocrm_core::{VideoId, VideoRow, VideoStatus};

use super::colors::{format_color, free_tag_color, run_status_color, tag_colors, youtube_color};
use super::elements::{Chip, muted_icon};
use super::format;
use super::panel::DatabasePanel;
use super::query::status_visible;
use super::table::{EmptyReason, element_id, render_empty_state, video_menu};
use crate::ui::video_icon;

/// The payload of a card being dragged.
#[derive(Clone)]
pub(crate) struct DraggedCard {
    video: VideoId,
    title: SharedString,
    icon: Option<SharedString>,
}

impl Render for DraggedCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_64()
            .gap_1p5()
            .px_3()
            .py_2()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .shadow_lg()
            .opacity(0.92)
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .child(video_icon(self.icon.as_deref()))
            .child(div().min_w_0().truncate().child(self.title.clone()))
    }
}

impl DatabasePanel {
    pub(super) fn render_board(&self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.empty_reason(cx) == EmptyReason::NoVideos && self.shown_rows().next().is_none() {
            return render_empty_state(EmptyReason::NoVideos, &cx.weak_entity(), cx);
        }
        let Some(view) = self.active_view() else {
            return div().into_any_element();
        };
        let now = Utc::now();
        let statuses: Vec<VideoStatus> =
            VideoStatus::ALL.into_iter().filter(|status| status_visible(*status, &view.filter)).collect();

        div()
            .id("board")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_x_scroll()
            .track_scroll(&self.board_scroll)
            .horizontal_scrollbar(&self.board_scroll)
            .child(h_flex().h_full().items_stretch().gap_3().px_4().pb_4().children(statuses.into_iter().map(
                |status| {
                    let cards: Vec<&VideoRow> = self.shown_rows().filter(|row| row.video.status == status).collect();
                    self.render_board_column(status, &cards, now, cx)
                },
            )))
            .into_any_element()
    }

    fn render_board_column(
        &self,
        status: VideoStatus,
        cards: &[&VideoRow],
        now: DateTime<Utc>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = tag_colors(status.color(), cx);
        let status_key = status.label().to_lowercase();
        let new_in_column = |id: &str, cx: &mut Context<Self>| {
            Button::new(SharedString::from(format!("{id}-{status_key}")))
                .ghost()
                .small()
                .accessibility_label(format!("New {} video", status.label().to_lowercase()))
                .on_click(cx.listener(move |this, _, _, cx| this.create_video(Some(status), cx)))
        };
        let add_button = new_in_column("board-add", cx).icon(IconName::Plus).tooltip("New video");
        // A child instead of a label, so the content lines up with the cards'
        // leading edge rather than centering in the column.
        let new_button = new_in_column("board-new", cx).w_full().child(
            h_flex()
                .flex_1()
                .gap_1()
                .px_1()
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(IconName::Plus).xsmall())
                .child("New"),
        );
        let card_elements: Vec<AnyElement> = cards.iter().map(|row| self.render_card(row, status, now, cx)).collect();

        v_flex()
            .id(SharedString::from(format!("board-column-{status_key}")))
            .test_support()
            .w_64()
            .flex_shrink_0()
            .min_h_0()
            .rounded(cx.theme().radius_lg)
            .bg(colors.tint)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_2()
                    .px_2()
                    .py_2()
                    .child(Chip::new(status.label(), status.color()).dot(true))
                    .child(div().text_sm().text_color(cx.theme().muted_foreground).child(cards.len().to_string()))
                    .child(div().flex_1())
                    .child(add_button),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .px_2()
                    .pb_2()
                    .gap_1()
                    .drag_over::<DraggedCard>(|style, _, _, cx| style.bg(cx.theme().drop_target.opacity(0.35)))
                    .on_drop(cx.listener(move |this, card: &DraggedCard, _, cx| {
                        this.move_card(card.video, status, None, cx);
                    }))
                    .children(card_elements)
                    .child(new_button)
                    .overflow_y_scrollbar()
                    .id(SharedString::from(format!("board-cards-{status_key}"))),
            )
            .into_any_element()
    }

    fn render_card(
        &self,
        row: &VideoRow,
        status: VideoStatus,
        now: DateTime<Utc>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let today = format::local_day(now);
        let video = &row.video;
        let id = video.id;
        let title = video.title.trim();
        let dragged = DraggedCard {
            video: id,
            title: if title.is_empty() { "Untitled".into() } else { title.to_string().into() },
            icon: video.icon.clone().map(Into::into),
        };
        let panel = cx.weak_entity();

        let meta = h_flex()
            .gap_3()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .when_some(video.publish_date, |this, date| {
                this.child(h_flex().gap_1().child(muted_icon(IconName::Calendar, cx)).child(format::date(date, today)))
            })
            .when(row.script_words > 0, |this| {
                this.child(
                    h_flex()
                        .gap_1()
                        .child(muted_icon(IconName::FileText, cx))
                        .child(format::runtime(row.script_seconds)),
                )
            });
        let has_meta = video.publish_date.is_some() || row.script_words > 0;
        // The same derived states as the table: a finished cut and a video on
        // YouTube are not hidden by a later failed or cancelled attempt.
        let edit = row.edit_status();
        let youtube = row.youtube_state();
        let scheduled = format::is_scheduled(video.publish.publish_at, now);
        let has_pipeline = edit.is_some() || youtube.is_some();

        // The transparent top border is the drop indicator's slot, so showing
        // it does not move the card.
        div()
            .id(element_id("card-slot", id))
            .test_support()
            .border_t_2()
            .border_color(cx.theme().transparent)
            .drag_over::<DraggedCard>(|style, _, _, cx| style.border_color(cx.theme().drag_border))
            .on_drop(cx.listener(move |this, card: &DraggedCard, _, cx| {
                this.move_card(card.video, status, Some(id), cx);
            }))
            .child(
                v_flex()
                    .id(element_id("card", id))
                    .test_support()
                    .gap_1p5()
                    .px_3()
                    .py_2()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .shadow_xs()
                    .hover(|this| this.bg(cx.theme().secondary))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_video(id, cx)))
                    .on_drag(dragged, |card, _, _, cx| cx.new(|_| card.clone()))
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_start()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(video_icon(video.icon.as_deref()))
                            .child(div().min_w_0().line_clamp(2).text_ellipsis().map(|this| {
                                if title.is_empty() {
                                    this.text_color(cx.theme().muted_foreground).child("Untitled")
                                } else {
                                    this.child(title.to_string())
                                }
                            })),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_1()
                            .child(Chip::new(video.format.label(), format_color(video.format)))
                            .children(video.tags.iter().map(|tag| Chip::new(tag.clone(), free_tag_color(tag)))),
                    )
                    .when(has_meta, |this| this.child(meta))
                    .when(has_pipeline, |this| {
                        this.child(
                            h_flex()
                                .flex_wrap()
                                .gap_1()
                                .when_some(edit, |this, run| {
                                    this.child(
                                        Chip::new(format!("Edit: {}", run.label()), run_status_color(run)).dot(true),
                                    )
                                })
                                .when_some(youtube, |this, state| {
                                    this.child(
                                        Chip::new(
                                            format!("YouTube: {}", format::youtube(state, scheduled)),
                                            youtube_color(state, scheduled),
                                        )
                                        .dot(true),
                                    )
                                }),
                        )
                    })
                    .context_menu(move |menu, _, _| video_menu(menu, id, false, &panel)),
            )
            .into_any_element()
    }
}
