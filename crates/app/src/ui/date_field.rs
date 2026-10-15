//! A date field, with a time of day when asked for: a button showing the value
//! that opens the kit's calendar, and its time field, in a popover.
//!
//! The kit's `DatePicker` draws its calendar in a frame of fixed pixels, while
//! the day cells are sized in rems: once the interface is zoomed they outgrow
//! the popup, and a click on the last days or the next-month arrow lands outside
//! it and closes it. This field draws the calendar as wide as the zoom needs.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Weekday};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::calendar::{Calendar, CalendarEvent, CalendarState, Date, Matcher};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::time_field::{TimeField, TimeFieldEvent, TimeFieldState};
use gpui_kit::component::{ActiveTheme as _, Icon, Selectable as _, Sizable, Size, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Pixels, RenderOnce,
    SharedString, Styled as _, Subscription, Window, div, px,
};

use super::zoom::zoomed;

/// The kit's small calendar at 100%: seven day cells and its padding.
pub const CALENDAR_WIDTH: Pixels = px(220.);

/// The kit's small calendar, as wide as its day cells at the current zoom.
pub fn calendar(state: &Entity<CalendarState>, first_day: Weekday, cx: &App) -> Calendar {
    Calendar::new(state).small().first_day_of_week(first_day).w(zoomed(CALENDAR_WIDTH, cx))
}

pub enum DateFieldEvent {
    /// The creator picked a date or a time, or cleared the field.
    Change(Option<NaiveDateTime>),
}

pub struct DateFieldState {
    calendar: Entity<CalendarState>,
    /// `None` when the field holds a date only.
    time: Option<Entity<TimeFieldState>>,
    value: Option<NaiveDateTime>,
    format: SharedString,
    first_day_of_week: Weekday,
    open: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DateFieldEvent> for DateFieldState {}

impl DateFieldState {
    /// A field for a date.
    pub fn date(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new(false, window, cx)
    }

    /// A field for a date and a time of day, to the minute.
    pub fn date_time(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new(true, window, cx)
    }

    fn new(with_time: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let calendar = cx.new(|cx| CalendarState::new(window, cx));
        let time = with_time.then(|| cx.new(|cx| TimeFieldState::new(window, cx)));
        let mut subscriptions = vec![cx.subscribe(&calendar, |this, _, event: &CalendarEvent, cx| {
            if let CalendarEvent::Selected(Date::Single(Some(date))) = event {
                this.pick_date(*date, cx);
            }
        })];
        if let Some(time) = &time {
            subscriptions.push(cx.subscribe(time, |this, _, event: &TimeFieldEvent, cx| {
                let TimeFieldEvent::Change(time) = event;
                this.pick_time(*time, cx);
            }));
        }
        Self {
            calendar,
            time,
            value: None,
            format: "%b %-d, %Y".into(),
            first_day_of_week: Weekday::Sun,
            open: false,
            _subscriptions: subscriptions,
        }
    }

    /// How the field shows its value, in `chrono`'s format.
    pub fn format(mut self, format: impl Into<SharedString>) -> Self {
        self.format = format.into();
        self
    }

    pub fn first_day_of_week(mut self, day: Weekday) -> Self {
        self.first_day_of_week = day;
        self
    }

    pub fn set_first_day_of_week(&mut self, day: Weekday, cx: &mut Context<Self>) {
        self.first_day_of_week = day;
        cx.notify();
    }

    /// Days before `day` can't be picked.
    pub fn disabled_before(self, day: NaiveDate, window: &mut Window, cx: &mut Context<Self>) -> Self {
        self.calendar
            .update(cx, |calendar, cx| calendar.set_disabled_matcher(Matcher::interval(Some(day), None), window, cx));
        self
    }

    pub fn value(&self) -> Option<NaiveDateTime> {
        self.value
    }

    /// Shows `value`, without reporting it as a change.
    pub fn set_value(&mut self, value: Option<NaiveDateTime>, window: &mut Window, cx: &mut Context<Self>) {
        self.value = value;
        self.show_value(window, cx);
        if let (Some(time), Some(value)) = (&self.time, value) {
            time.update(cx, |time, cx| time.set_time(value.time(), window, cx));
        }
        cx.notify();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    fn set_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.open = open;
        if open {
            // The calendar opens on the month of the value.
            self.show_value(window, cx);
        }
        cx.notify();
    }

    fn show_value(&self, window: &mut Window, cx: &mut Context<Self>) {
        let date = Date::Single(self.value.map(|value| value.date()));
        self.calendar.update(cx, |calendar, cx| calendar.set_date(date, window, cx));
    }

    fn pick_date(&mut self, date: NaiveDate, cx: &mut Context<Self>) {
        let time = match &self.time {
            Some(time) => time.read(cx).time(),
            None => NaiveTime::MIN,
        };
        self.value = Some(date.and_time(time));
        // A date alone is done once it's picked; a time may still follow it.
        if self.time.is_none() {
            self.open = false;
        }
        cx.emit(DateFieldEvent::Change(self.value));
        cx.notify();
    }

    fn pick_time(&mut self, time: NaiveTime, cx: &mut Context<Self>) {
        // Without a date, the time waits for one.
        let Some(value) = self.value else { return };
        self.value = Some(value.date().and_time(time));
        cx.emit(DateFieldEvent::Change(self.value));
        cx.notify();
    }

    fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.value = None;
        self.open = false;
        self.show_value(window, cx);
        cx.emit(DateFieldEvent::Change(None));
        cx.notify();
    }
}

/// The field for a [`DateFieldState`].
#[derive(IntoElement)]
pub struct DateField {
    id: SharedString,
    state: Entity<DateFieldState>,
    name: SharedString,
    placeholder: SharedString,
    cleanable: bool,
    size: Size,
}

impl DateField {
    /// `name` is what assistive technology announces, with the value shown.
    pub fn new(id: impl Into<SharedString>, name: impl Into<SharedString>, state: &Entity<DateFieldState>) -> Self {
        Self {
            id: id.into(),
            state: state.clone(),
            name: name.into(),
            placeholder: SharedString::default(),
            cleanable: false,
            size: Size::Medium,
        }
    }

    /// What the field says while it has no value.
    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Offers to clear the value.
    pub fn cleanable(mut self, cleanable: bool) -> Self {
        self.cleanable = cleanable;
        self
    }
}

impl Sizable for DateField {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl RenderOnce for DateField {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let field = self.state.read(cx);
        let (open, value, first_day) = (field.open, field.value, field.first_day_of_week);
        let (calendar, time) = (field.calendar.clone(), field.time.clone());
        let shown = value.map(|value| SharedString::from(value.format(&field.format).to_string()));
        let muted = cx.theme().muted_foreground;
        // The text inside the button isn't its name: the name says what it is and what it holds.
        let announced = format!("{}: {}", self.name, shown.as_ref().unwrap_or(&self.placeholder));
        let trigger = Button::new(self.id.clone())
            .outline()
            .with_size(self.size)
            .w_full()
            .selected(open)
            .accessibility_label(announced)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .justify_between()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .when(shown.is_none(), |this| this.text_color(muted))
                            .child(shown.unwrap_or(self.placeholder)),
                    )
                    .child(Icon::new(IconName::Calendar).xsmall().text_color(muted)),
            );
        let state = self.state.clone();
        let clear = (self.cleanable && value.is_some()).then(|| self.state.clone());
        Popover::new(SharedString::from(format!("{}-popover", self.id)))
            .open(open)
            .on_open_change(move |open: &bool, window: &mut Window, cx: &mut App| {
                state.update(cx, |state, cx| state.set_open(*open, window, cx));
            })
            .trigger(trigger)
            .content(move |_, _, cx| {
                v_flex()
                    .gap_2()
                    .child(self::calendar(&calendar, first_day, cx))
                    .when_some(time.clone(), |this, time| {
                        this.child(
                            h_flex()
                                .justify_between()
                                .gap_3()
                                .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Time"))
                                .child(TimeField::new(&time).small()),
                        )
                    })
                    .when_some(clear.clone(), |this, state| {
                        this.child(
                            Button::new("clear-date")
                                .ghost()
                                .small()
                                .icon(IconName::X)
                                .label("Clear date")
                                .on_click(move |_, window, cx| state.update(cx, |state, cx| state.clear(window, cx))),
                        )
                    })
            })
    }
}
