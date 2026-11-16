//! The calendar popover and the Thai holiday feed it shows.

pub mod feed;
mod view;

use std::fmt::Write as _;
use std::ops::Range;
use std::sync::Arc;

use deskshell_core::calendar::{self as core, DAY_CELLS, Header, PICKER_CELLS, PickerCell, View};
use iced::futures::{Stream, stream};
use iced::{Element, Subscription};
use jiff::Zoned;
use jiff::civil::Date;

use super::Env;
use crate::clock;

pub use deskshell_core::calendar::Holidays;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Message {
    Today,
    Navigate(i32),
    Wheel(f32),
    Promote,
    Select(Date),
    Cell(PickerCell),
    Hover(usize),
    Unhover(usize),
    Midnight(Date),
    Refresh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayCell {
    pub date: Date,
    pub label: &'static str,
    pub today: bool,
    pub selected: bool,
    pub dim: bool,
    pub holiday: bool,
    pub dot: bool,
}

#[derive(Debug, Default)]
struct Cache {
    title: String,
    year: String,
    days: Vec<DayCell>,
    pickers: Vec<PickerCell>,
    labels: [String; PICKER_CELLS],
    weekday: String,
    date: String,
    era: String,
    selected: Range<usize>,
    upcoming: Range<usize>,
    relative: Vec<String>,
}

#[derive(Debug)]
pub struct Calendar {
    calendar: core::Calendar,
    holidays: Arc<Holidays>,
    hovered: Option<usize>,
    clock: fn() -> Date,
    cache: Cache,
}

impl Default for Calendar {
    fn default() -> Self {
        Self::with_clock(local_today)
    }
}

pub fn local_today() -> Date {
    Zoned::now().date()
}

impl Calendar {
    pub fn with_clock(clock: fn() -> Date) -> Self {
        let mut calendar = Self {
            calendar: core::Calendar::new(clock()),
            holidays: Arc::default(),
            hovered: None,
            clock,
            cache: Cache::default(),
        };
        calendar.rebuild();
        calendar
    }

    pub fn open(&mut self) {
        self.calendar
            .reset_to_today((self.clock)());
        self.hovered = None;
        self.rebuild();
    }

    pub fn set_holidays(&mut self, holidays: Arc<Holidays>) {
        self.holidays = holidays;
        self.rebuild();
    }

    pub fn state(&self) -> &core::Calendar {
        &self.calendar
    }

    pub fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    pub fn update(&mut self, message: Message) {
        match message {
            Message::Today => self
                .calendar
                .reset_to_today((self.clock)()),
            Message::Navigate(offset) => self.calendar.navigate(offset),
            // An event without vertical motion goes back.
            Message::Wheel(y) => self
                .calendar
                .navigate(if y < 0.0 { 1 } else { -1 }),
            Message::Promote => {
                if self.calendar.can_promote() {
                    self.calendar.promote_view();
                }
            }
            Message::Select(date) => self.calendar.select_day(date),
            Message::Cell(cell) => self.calendar.select_cell(cell),
            Message::Hover(index) => {
                self.hovered = Some(index);
                return;
            }
            Message::Unhover(index) => {
                if self.hovered == Some(index) {
                    self.hovered = None;
                }
                return;
            }
            Message::Midnight(today) => {
                if today == self.calendar.today() {
                    return;
                }
                self.calendar.set_today(today);
            }
            Message::Refresh => return,
        }
        if self.calendar.view() != View::Days {
            self.hovered = None;
        }
        self.rebuild();
    }

    pub fn view<'a>(&'a self, env: &Env<'a>) -> Element<'a, super::Message> {
        view::view(self, env.theme).map(super::Message::Calendar)
    }

    pub fn days(&self) -> &[DayCell] {
        &self.cache.days
    }

    pub fn pickers(&self) -> impl Iterator<Item = (&PickerCell, &str)> {
        self.cache.pickers.iter().zip(
            self.cache
                .labels
                .iter()
                .map(String::as_str),
        )
    }

    pub fn title(&self) -> (&str, &str) {
        (&self.cache.title, &self.cache.year)
    }

    pub fn summary(&self) -> (&str, &str, &str) {
        (
            &self.cache.weekday,
            &self.cache.date,
            &self.cache.era,
        )
    }

    pub fn selected_holidays(&self) -> &[core::Holiday] {
        &self.holidays.entries()[self.cache.selected.clone()]
    }

    pub fn upcoming(&self) -> impl Iterator<Item = (&core::Holiday, &str)> {
        self.holidays.entries()[self.cache.upcoming.clone()]
            .iter()
            .zip(
                self.cache
                    .relative
                    .iter()
                    .map(String::as_str),
            )
    }

    pub fn empty_text(&self) -> Option<&'static str> {
        if !self.cache.upcoming.is_empty() {
            None
        } else if self.holidays.is_loaded() {
            Some("No upcoming holidays")
        } else {
            Some("Holiday calendar unavailable")
        }
    }

    fn rebuild(&mut self) {
        let calendar = &self.calendar;
        let holidays = &*self.holidays;
        let cache = &mut self.cache;
        let today = calendar.today();
        let selected = calendar.selected();

        cache.title.clear();
        cache.year.clear();
        match calendar.header() {
            Header::Month { month, year } => {
                cache
                    .title
                    .push_str(core::month_name(month));
                let _ = write!(cache.year, "{year}");
            }
            Header::Year(year) => {
                let _ = write!(cache.title, "{year}");
            }
            Header::Decade(start) => {
                let _ = write!(cache.title, "{start} – {}", start + 9);
            }
        }

        cache.days.clear();
        cache.days.reserve(DAY_CELLS);
        for date in calendar.day_cells() {
            let on = holidays.on(date);
            cache.days.push(DayCell {
                date,
                label: core::day_number(date),
                today: date == today,
                selected: date == selected,
                dim: calendar.is_dim(date),
                holiday: on.iter().any(|entry| !entry.observance),
                dot: !on.is_empty(),
            });
        }

        cache.pickers.clear();
        cache
            .pickers
            .extend(calendar.picker_cells());
        let months = calendar.view() == View::Months;
        for (label, cell) in cache
            .labels
            .iter_mut()
            .zip(&cache.pickers)
        {
            label.clear();
            if months {
                label.push_str(core::month_short(cell.month));
            } else {
                let _ = write!(label, "{}", cell.year);
            }
        }

        cache.weekday.clear();
        cache
            .weekday
            .push_str(core::weekday_name(selected));
        if selected != today {
            cache.weekday.push_str(" · ");
            core::relative_text(
                core::days_between(today, selected),
                &mut cache.weekday,
            );
        }
        cache.date.clear();
        let _ = write!(
            cache.date,
            "{} {}",
            selected.day(),
            core::month_name(selected.month())
        );
        cache.era.clear();
        let era = i32::from(selected.year()) + 543;
        let _ = if selected.year() == today.year() {
            write!(cache.era, "B.E. {era}")
        } else {
            write!(cache.era, "{} · B.E. {era}", selected.year())
        };

        cache.selected = holidays.range_on(selected);
        cache.upcoming = holidays.upcoming(today, cache.selected.len());
        let rows = cache.upcoming.len();
        cache
            .relative
            .resize_with(rows, String::new);
        for (text, entry) in cache
            .relative
            .iter_mut()
            .zip(&holidays.entries()[cache.upcoming.clone()])
        {
            text.clear();
            core::relative_text(core::days_between(today, entry.date), text);
        }
    }
}

/// Wakes at every minute boundary, so a suspend over midnight is caught within a minute.
pub fn subscription() -> Subscription<Message> {
    Subscription::run(dates).map(Message::Midnight)
}

fn dates() -> impl Stream<Item = Date> {
    stream::unfold(local_today(), |last| async move {
        loop {
            tokio::time::sleep(clock::until_next_minute(&Zoned::now())).await;
            let today = local_today();
            if today != last {
                return Some((today, today));
            }
        }
    })
}
