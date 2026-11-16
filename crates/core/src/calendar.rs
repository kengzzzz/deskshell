//! Pure parts of the calendar panel and the holiday feed: navigation, grids and iCalendar parsing.

use std::fmt::Write as _;
use std::sync::Arc;

use jiff::civil::Date;
use jiff::{SignedDuration, Span};

pub const DAY_CELLS: usize = 42;
pub const PICKER_CELLS: usize = 16;
pub const UPCOMING: usize = 5;
pub const MAX_EVENT_DAYS: usize = 366;

pub const WEEKDAYS_SHORT: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];
pub const WEEKDAYS: [&str; 7] =
    ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
pub const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
pub const MONTHS_SHORT: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
pub const MONTHS_UPPER: [&str; 12] =
    ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
pub const DAY_NUMBERS: [&str; 31] = [
    "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16", "17",
    "18", "19", "20", "21", "22", "23", "24", "25", "26", "27", "28", "29", "30", "31",
];

/// Their 42-cell grids stay inside the years `jiff` can hold, -9999 to 9999.
const MONTH_MIN: i32 = -9999 * 12 + 1;
const MONTH_MAX: i32 = 9999 * 12 + 10;

fn month_slot(month: i8) -> usize {
    (month.clamp(1, 12) - 1)
        .unsigned_abs()
        .into()
}

pub fn month_name(month: i8) -> &'static str {
    MONTHS[month_slot(month)]
}

pub fn month_short(month: i8) -> &'static str {
    MONTHS_SHORT[month_slot(month)]
}

pub fn month_upper(month: i8) -> &'static str {
    MONTHS_UPPER[month_slot(month)]
}

pub fn weekday_name(date: Date) -> &'static str {
    WEEKDAYS[date
        .weekday()
        .to_sunday_zero_offset()
        .unsigned_abs() as usize]
}

pub fn day_number(date: Date) -> &'static str {
    DAY_NUMBERS[(date.day().clamp(1, 31) - 1).unsigned_abs() as usize]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    Days,
    Months,
    Years,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PickerCell {
    pub year: i16,
    pub month: i8,
    pub dim: bool,
    pub today: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Header {
    Month {
        /// 1 to 12.
        month: i8,
        year: i16,
    },
    Year(i16),
    Decade(i16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Calendar {
    today: Date,
    selected: Date,
    year: i16,
    month: i8,
    view: View,
}

impl Calendar {
    pub fn new(today: Date) -> Self {
        let mut calendar = Self {
            today,
            selected: today,
            year: today.year(),
            month: today.month(),
            view: View::Days,
        };
        calendar.show(i32::from(today.year()), i32::from(today.month()));
        calendar
    }

    pub fn reset_to_today(&mut self, today: Date) {
        *self = Self::new(today);
    }

    pub fn set_today(&mut self, today: Date) {
        self.today = today;
    }

    pub fn today(&self) -> Date {
        self.today
    }

    pub fn selected(&self) -> Date {
        self.selected
    }

    pub fn shown(&self) -> (i16, i8) {
        (self.year, self.month)
    }

    pub fn view(&self) -> View {
        self.view
    }

    fn show(&mut self, year: i32, month: i32) {
        let slot = year
            .saturating_mul(12)
            .saturating_add(month.saturating_sub(1))
            .clamp(MONTH_MIN, MONTH_MAX);
        self.year = slot.div_euclid(12) as i16;
        self.month = (slot.rem_euclid(12) + 1) as i8;
    }

    pub fn shift_month(&mut self, offset: i32) {
        self.show(
            i32::from(self.year),
            i32::from(self.month).saturating_add(offset),
        );
    }

    pub fn navigate(&mut self, offset: i32) {
        let year = i32::from(self.year);
        match self.view {
            View::Days => self.shift_month(offset),
            View::Months => self.show(
                year.saturating_add(offset)
                    .clamp(1, 9998),
                i32::from(self.month),
            ),
            View::Years => self.show(
                year.saturating_add(offset.saturating_mul(10))
                    .clamp(1, 9984),
                i32::from(self.month),
            ),
        }
    }

    pub fn promote_view(&mut self) {
        self.view = match self.view {
            View::Days => View::Months,
            View::Months | View::Years => View::Years,
        };
    }

    pub fn can_promote(&self) -> bool {
        self.view != View::Years
    }

    pub fn select_day(&mut self, date: Date) {
        self.selected = date;
        if date.month() != self.month || date.year() != self.year {
            self.show(i32::from(date.year()), i32::from(date.month()));
        }
        self.view = View::Days;
    }

    pub fn select_cell(&mut self, cell: PickerCell) {
        self.show(i32::from(cell.year), i32::from(cell.month));
        self.view = if self.view == View::Months { View::Days } else { View::Months };
    }

    pub fn day_cells(&self) -> [Date; DAY_CELLS] {
        let first = Date::new(self.year, self.month, 1).unwrap_or_default();
        let back = i64::from(first.weekday().to_sunday_zero_offset());
        std::array::from_fn(|index| add_days(first, index as i64 - back).unwrap_or(first))
    }

    pub fn is_dim(&self, date: Date) -> bool {
        date.month() != self.month || date.year() != self.year
    }

    pub fn picker_cells(&self) -> [PickerCell; PICKER_CELLS] {
        let year = i32::from(self.year);
        let decade = year - year % 10;
        let today = self.today;
        std::array::from_fn(|index| {
            let index = index as i32;
            if self.view == View::Months {
                let cell_year = (year + index / 12) as i16;
                let month = (index % 12 + 1) as i8;
                PickerCell {
                    year: cell_year,
                    month,
                    dim: index >= 12,
                    today: cell_year == today.year() && month == today.month(),
                }
            } else {
                let cell_year = (decade + index) as i16;
                PickerCell {
                    year: cell_year,
                    month: self.month,
                    dim: index >= 10,
                    today: cell_year == today.year(),
                }
            }
        })
    }

    pub fn header(&self) -> Header {
        match self.view {
            View::Days => Header::Month { month: self.month, year: self.year },
            View::Months => Header::Year(self.year),
            View::Years => Header::Decade(self.year - self.year % 10),
        }
    }
}

fn add_days(date: Date, days: i64) -> Option<Date> {
    let hours = days.checked_mul(24)?;
    if hours.unsigned_abs() > 24 * 7_400_000 {
        return None;
    }
    date.checked_add(SignedDuration::from_hours(hours))
        .ok()
}

pub fn days_between(from: Date, to: Date) -> i64 {
    to.duration_since(from).as_secs() / 86_400
}

pub fn relative_text(days: i64, out: &mut String) {
    match days {
        0 => out.push_str("Today"),
        1 => out.push_str("Tomorrow"),
        -1 => out.push_str("Yesterday"),
        days if days > 0 => {
            let _ = write!(out, "{days}d");
        }
        days => {
            let _ = write!(out, "{}d ago", days.unsigned_abs());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holiday {
    pub date: Date,
    pub name: Arc<str>,
    pub observance: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Holidays {
    entries: Vec<Holiday>,
}

impl Holidays {
    pub fn parse(text: &str) -> Self {
        let unfolded = unfold(text);
        let mut entries = Vec::new();
        for block in unfolded.split("BEGIN:VEVENT").skip(1) {
            let event = Event::read(block);
            let Some(start) = event.start.and_then(date_digits) else {
                continue;
            };
            let Some(summary) = event
                .summary
                .filter(|summary| !summary.is_empty())
            else {
                continue;
            };
            let Some(first) = js_date(start.0, start.1, start.2) else {
                continue;
            };
            let last = match event.end.and_then(date_digits) {
                Some(end) => js_date(end.0, end.1, end.2 - 1),
                None => Some(first),
            };
            let Some(last) = last else {
                continue;
            };
            let name: Arc<str> = Arc::from(clean_text(summary));
            let observance = event
                .description
                .is_some_and(|text| text.starts_with("Observance"));
            let mut day = Some(first);
            let mut count = 0;
            while let Some(date) = day
                && date <= last
                && count < MAX_EVENT_DAYS
            {
                entries.push(Holiday { date, name: Arc::clone(&name), observance });
                count += 1;
                day = date.tomorrow().ok();
            }
        }
        entries.sort_by_key(|entry| (entry.date, entry.observance));
        Self { entries }
    }

    pub fn entries(&self) -> &[Holiday] {
        &self.entries
    }

    pub fn is_loaded(&self) -> bool {
        !self.entries.is_empty()
    }

    pub fn on(&self, date: Date) -> &[Holiday] {
        &self.entries[self.range(
            |entry| entry.date < date,
            |entry| entry.date <= date,
        )]
    }

    pub fn range_on(&self, date: Date) -> std::ops::Range<usize> {
        self.range(
            |entry| entry.date < date,
            |entry| entry.date <= date,
        )
    }

    pub fn in_month(&self, year: i16, month: i8) -> &[Holiday] {
        let key = (year, month);
        &self.entries[self.range(
            |entry| (entry.date.year(), entry.date.month()) < key,
            |entry| (entry.date.year(), entry.date.month()) <= key,
        )]
    }

    pub fn upcoming(&self, today: Date, selected: usize) -> std::ops::Range<usize> {
        let start = self
            .entries
            .partition_point(|entry| entry.date < today);
        let len = UPCOMING - selected.min(2);
        start..self.entries.len().min(start + len)
    }

    fn range(
        &self,
        before: impl Fn(&Holiday) -> bool,
        through: impl Fn(&Holiday) -> bool,
    ) -> std::ops::Range<usize> {
        let start = self.entries.partition_point(before);
        let end = self.entries.partition_point(through);
        start..end.max(start)
    }
}

#[derive(Debug, Default)]
struct Event<'a> {
    start: Option<&'a str>,
    end: Option<&'a str>,
    summary: Option<&'a str>,
    description: Option<&'a str>,
}

impl<'a> Event<'a> {
    fn read(block: &'a str) -> Self {
        let mut event = Self::default();
        for line in block.split('\n') {
            let line = line.trim_suffix('\r');
            let Some((name, value)) = field(line) else {
                continue;
            };
            let slot = match name {
                "DTSTART" => &mut event.start,
                "DTEND" => &mut event.end,
                "SUMMARY" => &mut event.summary,
                "DESCRIPTION" => &mut event.description,
                _ => continue,
            };
            if slot.is_none() {
                *slot = Some(value);
            }
        }
        event
    }
}

fn unfold(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        let fold = match bytes[index] {
            b'\r'
                if bytes.get(index + 1) == Some(&b'\n')
                    && matches!(bytes.get(index + 2), Some(b' ' | b'\t')) =>
            {
                3
            }
            b'\n' if matches!(bytes.get(index + 1), Some(b' ' | b'\t')) => 2,
            _ => 0,
        };
        if fold == 0 {
            index += 1;
        } else {
            out.push_str(&text[start..index]);
            index += fold;
            start = index;
        }
    }
    out.push_str(&text[start..]);
    out
}

/// Like JS `.`, the value must not hold a line terminator.
fn field(line: &str) -> Option<(&str, &str)> {
    let name = line
        .bytes()
        .take_while(|byte| byte.is_ascii_uppercase() || *byte == b'-')
        .count();
    if name == 0 {
        return None;
    }
    let colon = line.find(':')?;
    let value = &line[colon + 1..];
    if value.contains(['\r', '\n', '\u{2028}', '\u{2029}']) {
        return None;
    }
    Some((&line[..name], value))
}

fn date_digits(value: &str) -> Option<(i32, i32, i32)> {
    let digits = value.as_bytes().get(..8)?;
    if !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let number = |from: usize, to: usize| {
        digits[from..to]
            .iter()
            .fold(0, |number, digit| {
                number * 10 + i32::from(digit - b'0')
            })
    };
    Some((number(0, 4), number(4, 6), number(6, 8)))
}

/// Carries like JS `new Date(year, month - 1, day)`: day 0 is the last of the month
/// before, and years 0 to 99 mean 1900 to 1999.
fn js_date(year: i32, month: i32, day: i32) -> Option<Date> {
    let year = if (0..=99).contains(&year) { year + 1900 } else { year };
    let january = Date::new(i16::try_from(year).ok()?, 1, 1).ok()?;
    let months = Span::new().try_months(month - 1).ok()?;
    add_days(
        january.checked_add(months).ok()?,
        i64::from(day - 1),
    )
}

pub fn clean_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(
                chars
                    .next_if_map(|next| match next {
                        ',' | ';' | '\\' => Ok(next),
                        'n' | 'N' => Ok(' '),
                        other => Err(other),
                    })
                    .unwrap_or('\\'),
            ),
            '\u{200B}' => {}
            c => out.push(c),
        }
    }
    let end = out.trim_end_matches(is_js_space).len();
    out.truncate(end);
    let start = out.len()
        - out
            .trim_start_matches(is_js_space)
            .len();
    out.drain(..start);
    out
}

/// Qt's JS `trim()` set: `char::is_whitespace` and U+FEFF.
fn is_js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{FEFF}'
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    fn at(year: i16, month: i8) -> Calendar {
        let mut calendar = Calendar::new(date(2026, 11, 13));
        calendar.show(i32::from(year), i32::from(month));
        calendar
    }

    #[test]
    fn reset_to_today() {
        let mut calendar = Calendar::new(date(2026, 11, 13));
        assert_eq!(calendar.shown(), (2026, 11));
        assert_eq!(calendar.selected(), date(2026, 11, 13));
        assert_eq!(calendar.view(), View::Days);
        calendar.promote_view();
        calendar.navigate(3);
        calendar.select_day(date(2020, 2, 29));
        calendar.reset_to_today(date(2027, 1, 2));
        assert_eq!(calendar, Calendar::new(date(2027, 1, 2)));
        assert_eq!(calendar.today(), date(2027, 1, 2));
        assert_eq!(calendar.shown(), (2027, 1));
    }

    #[test]
    fn midnight_moves_only_today() {
        let mut calendar = Calendar::new(date(2026, 12, 31));
        calendar.shift_month(-2);
        calendar.set_today(date(2027, 1, 1));
        assert_eq!(calendar.today(), date(2027, 1, 1));
        assert_eq!(calendar.selected(), date(2026, 12, 31));
        assert_eq!(calendar.shown(), (2026, 10));
    }

    #[test]
    fn grid_for_months_starting_on_each_weekday() {
        let cases = [
            ((2026, 11), date(2026, 11, 1), date(2026, 12, 12)),
            ((2026, 6), date(2026, 5, 31), date(2026, 7, 11)),
            ((2026, 9), date(2026, 8, 30), date(2026, 10, 10)),
            ((2026, 4), date(2026, 3, 29), date(2026, 5, 9)),
            ((2026, 1), date(2025, 12, 28), date(2026, 2, 7)),
            ((2027, 1), date(2026, 12, 27), date(2027, 2, 6)),
            ((2026, 8), date(2026, 7, 26), date(2026, 9, 5)),
            ((2026, 2), date(2026, 2, 1), date(2026, 3, 14)),
            ((2028, 2), date(2028, 1, 30), date(2028, 3, 11)),
        ];
        for ((year, month), first, last) in cases {
            let calendar = at(year, month);
            let cells = calendar.day_cells();
            assert_eq!(cells[0], first, "{year}-{month}");
            assert_eq!(cells[41], last, "{year}-{month}");
            assert!(
                cells
                    .windows(2)
                    .all(|pair| pair[0].tomorrow().unwrap() == pair[1]),
                "consecutive days"
            );
            assert_eq!(
                cells[0]
                    .weekday()
                    .to_sunday_zero_offset(),
                0
            );
        }
        let calendar = at(2026, 12);
        let cells = calendar.day_cells();
        assert_eq!(cells[0], date(2026, 11, 29));
        assert!(calendar.is_dim(cells[0]));
        assert!(!calendar.is_dim(cells[2]));
        assert!(calendar.is_dim(cells[41]), "January 2027");
        let dim = cells
            .iter()
            .filter(|cell| calendar.is_dim(**cell))
            .count();
        assert_eq!(dim, 42 - 31);
    }

    #[test]
    fn shift_month_across_years() {
        let mut calendar = at(2026, 11);
        calendar.shift_month(2);
        assert_eq!(calendar.shown(), (2027, 1));
        calendar.shift_month(-13);
        assert_eq!(calendar.shown(), (2025, 12));
        calendar.shift_month(-12 * 3);
        assert_eq!(calendar.shown(), (2022, 12));
        assert_eq!(
            calendar.selected(),
            date(2026, 11, 13),
            "the selection stays"
        );
    }

    #[test]
    fn the_days_view_stops_at_jiffs_years() {
        let mut calendar = at(9999, 10);
        calendar.shift_month(5);
        assert_eq!(calendar.shown(), (9999, 11));
        assert_eq!(calendar.day_cells()[41], date(9999, 12, 11));
        calendar.shift_month(i32::MIN);
        assert_eq!(calendar.shown(), (-9999, 2));
        assert!(calendar.day_cells()[0] >= date(-9999, 1, 1));
        calendar.shift_month(i32::MAX);
        assert_eq!(calendar.shown(), (9999, 11));
    }

    #[test]
    fn navigate_in_each_view() {
        let mut calendar = at(2026, 11);
        calendar.navigate(1);
        assert_eq!(calendar.shown(), (2026, 12));
        calendar.navigate(-2);
        assert_eq!(calendar.shown(), (2026, 10));

        calendar.promote_view();
        assert_eq!(calendar.view(), View::Months);
        calendar.navigate(1);
        assert_eq!(
            calendar.shown(),
            (2027, 10),
            "a year, the month stays"
        );
        calendar.navigate(-3);
        assert_eq!(calendar.shown(), (2024, 10));

        calendar.promote_view();
        assert_eq!(calendar.view(), View::Years);
        calendar.navigate(1);
        assert_eq!(calendar.shown(), (2034, 10), "a decade");
        calendar.navigate(-2);
        assert_eq!(calendar.shown(), (2014, 10));
        calendar.promote_view();
        assert_eq!(
            calendar.view(),
            View::Years,
            "the decade view is the top"
        );
        assert!(!calendar.can_promote());
    }

    #[test]
    fn navigate_clamps() {
        let mut calendar = at(9997, 5);
        calendar.promote_view();
        calendar.navigate(5);
        assert_eq!(calendar.shown(), (9998, 5));
        calendar.navigate(-20_000);
        assert_eq!(calendar.shown(), (1, 5));

        calendar.promote_view();
        calendar.navigate(1_000);
        assert_eq!(calendar.shown(), (9984, 5));
        calendar.navigate(-1);
        assert_eq!(calendar.shown(), (9974, 5));
        let mut calendar = at(6, 3);
        calendar.promote_view();
        calendar.promote_view();
        calendar.navigate(-1);
        assert_eq!(calendar.shown(), (1, 3));
        assert_eq!(calendar.header(), Header::Decade(0));
        assert_eq!(calendar.picker_cells()[0].year, 0);
    }

    #[test]
    fn the_month_picker() {
        let mut calendar = at(2026, 3);
        calendar.promote_view();
        assert_eq!(calendar.header(), Header::Year(2026));
        let cells = calendar.picker_cells();
        assert_eq!(
            cells[0],
            PickerCell { year: 2026, month: 1, dim: false, today: false }
        );
        assert_eq!(
            cells[10],
            PickerCell { year: 2026, month: 11, dim: false, today: true }
        );
        assert_eq!(
            cells[12],
            PickerCell { year: 2027, month: 1, dim: true, today: false }
        );
        assert_eq!(
            cells[15],
            PickerCell { year: 2027, month: 4, dim: true, today: false }
        );
        calendar.select_cell(cells[14]);
        assert_eq!(calendar.shown(), (2027, 3));
        assert_eq!(calendar.view(), View::Days);
    }

    #[test]
    fn the_decade_picker() {
        let mut calendar = at(2026, 3);
        calendar.promote_view();
        calendar.promote_view();
        assert_eq!(calendar.header(), Header::Decade(2020));
        let cells = calendar.picker_cells();
        assert_eq!(
            cells[0],
            PickerCell { year: 2020, month: 3, dim: false, today: false }
        );
        assert_eq!(
            cells[6],
            PickerCell { year: 2026, month: 3, dim: false, today: true }
        );
        assert_eq!(
            cells[10],
            PickerCell { year: 2030, month: 3, dim: true, today: false }
        );
        assert_eq!(cells[15].year, 2035);
        calendar.select_cell(cells[11]);
        assert_eq!(calendar.shown(), (2031, 3));
        assert_eq!(
            calendar.view(),
            View::Months,
            "a year opens the month picker"
        );
        let month_cells = calendar.picker_cells();
        assert!(
            month_cells
                .iter()
                .all(|cell| !cell.today),
            "2031 holds no today"
        );
    }

    #[test]
    fn select_day_shows_its_month() {
        let mut calendar = at(2026, 11);
        calendar.select_day(date(2026, 11, 20));
        assert_eq!(calendar.shown(), (2026, 11));
        assert_eq!(calendar.selected(), date(2026, 11, 20));
        calendar.select_day(date(2026, 12, 5));
        assert_eq!(calendar.shown(), (2026, 12));
        calendar.promote_view();
        calendar.select_day(date(2027, 4, 13));
        assert_eq!(calendar.shown(), (2027, 4));
        assert_eq!(calendar.view(), View::Days);
    }

    #[test]
    fn header() {
        let calendar = at(2026, 11);
        assert_eq!(
            calendar.header(),
            Header::Month { month: 11, year: 2026 }
        );
        assert!(calendar.can_promote());
        assert_eq!(month_name(11), "November");
        assert_eq!(month_short(9), "Sep");
        assert_eq!(month_upper(12), "DEC");
        assert_eq!(weekday_name(date(2026, 11, 13)), "Friday");
        assert_eq!(day_number(date(2026, 11, 13)), "13");
        assert_eq!(day_number(date(2026, 12, 31)), "31");
    }

    #[test]
    fn relative_texts() {
        let today = date(2026, 11, 13);
        let mut out = String::new();
        for (day, expected) in [
            (date(2026, 11, 13), "Today"),
            (date(2026, 11, 14), "Tomorrow"),
            (date(2026, 11, 12), "Yesterday"),
            (date(2026, 12, 5), "22d"),
            (date(2027, 11, 13), "365d"),
            (date(2026, 10, 31), "13d ago"),
            (date(2024, 11, 13), "730d ago"),
        ] {
            out.clear();
            relative_text(days_between(today, day), &mut out);
            assert_eq!(out, expected, "{day}");
        }
    }

    const FEED: &str = "BEGIN:VCALENDAR\r\n\
        PRODID:-//Example//Holidays//EN\r\n\
        VERSION:2.0\r\n\
        SUMMARY:Not an event\r\n\
        BEGIN:VEVENT\r\n\
        DTSTART;VALUE=DATE:20261205\r\n\
        DTEND;VALUE=DATE:20261206\r\n\
        DTSTAMP:20260101T000000Z\r\n\
        UID:one@example.org\r\n\
        CLASS:PUBLIC\r\n\
        DESCRIPTION:Public holiday\r\n\
        SUMMARY:Founders Day\r\n\
        END:VEVENT\r\n\
        BEGIN:VEVENT\r\n\
        DTSTART;VALUE=DATE:20261205\r\n\
        DTEND;VALUE=DATE:20261206\r\n\
        DESCRIPTION:Observance\\nTo hide observances\\, go to settings\r\n\
        SUMMARY:Lantern Evening\r\n\
        END:VEVENT\r\n\
        BEGIN:VEVENT\r\n\
        DTSTART;VALUE=DATE:20261204\r\n\
        DTEND;VALUE=DATE:20261206\r\n\
        DESCRIPTION:Public holiday\r\n\
        SUMMARY:Harvest Fes\r\n tival (\\, folded\r\n\twith a tab\\; and \\\\n)\u{200B}\r\n\
        END:VEVENT\r\n\
        BEGIN:VEVENT\r\n\
        DTSTART:20261231T170000Z\r\n\
        DTEND:20270101T170000Z\r\n\
        SUMMARY:  Year End  \r\n\
        END:VEVENT\r\n\
        BEGIN:VEVENT\r\n\
        DTSTART;VALUE=DATE:20270101\r\n\
        DTEND;VALUE=DATE:20270102\r\n\
        SUMMARY:New Year\r\n\
        SUMMARY:Second summary\r\n\
        DESCRIPTION:Public holiday\r\n\
        BEGIN:VALARM\r\n\
        DESCRIPTION:Observance reminder\r\n\
        END:VALARM\r\n\
        END:VEVENT\r\n\
        BEGIN:VEVENT\r\n\
        DTSTART;VALUE=DATE:2027011\r\n\
        SUMMARY:Broken start\r\n\
        END:VEVENT\r\n\
        BEGIN:VEVENT\r\n\
        DTSTART;VALUE=DATE:20270120\r\n\
        SUMMARY:\r\n\
        END:VEVENT\r\n\
        BEGIN:VEVENT\r\n\
        DTSTART;VALUE=DATE:20270125\r\n\
        END:VEVENT\r\n\
        BEGIN:VEVENT\r\n\
        DTSTART:20270301T090000\r\n\
        DTEND:20270301T100000\r\n\
        SUMMARY:Same day timed\r\n\
        END:VEVENT\r\n\
        BEGIN:VEVENT\r\n\
        summary:lower case names do not count\r\n\
        DTSTART;VALUE=DATE:20270302\r\n\
        DTEND;VALUE=DATE:20270303\r\n\
        SUMMARY:\u{200B}\r\n\
        DESCRIPTION:Observance\r\n\
        END:VEVENT\r\n\
        END:VCALENDAR\r\n";

    fn names(entries: &[Holiday]) -> Vec<(Date, &str, bool)> {
        entries
            .iter()
            .map(|entry| (entry.date, &*entry.name, entry.observance))
            .collect()
    }

    #[test]
    fn parse_the_feed() {
        let holidays = Holidays::parse(FEED);
        let harvest = "Harvest Festival (, foldedwith a tab; and \\n)";
        assert_eq!(
            names(holidays.entries()),
            [
                (date(2026, 12, 4), harvest, false),
                (date(2026, 12, 5), "Founders Day", false),
                (date(2026, 12, 5), harvest, false),
                (date(2026, 12, 5), "Lantern Evening", true),
                (date(2026, 12, 31), "Year End", false),
                (date(2027, 1, 1), "New Year", false),
                (date(2027, 3, 2), "", true),
            ]
        );
        assert!(holidays.is_loaded());
    }

    #[test]
    fn parse_lf_and_crlf_alike() {
        let lf = FEED.replace("\r\n", "\n");
        assert_eq!(Holidays::parse(&lf), Holidays::parse(FEED));
    }

    #[test]
    fn parse_skips_bad_entries() {
        let holidays = Holidays::parse(FEED);
        for day in [date(2027, 1, 11), date(2027, 1, 20), date(2027, 1, 25), date(2027, 3, 1)] {
            assert!(holidays.on(day).is_empty(), "{day}");
        }
        assert!(!Holidays::parse("").is_loaded());
        assert!(!Holidays::parse("<html>captive portal</html>").is_loaded());
        assert!(!Holidays::parse("BEGIN:VEVENT\nSUMMARY:No start\nEND:VEVENT\n").is_loaded());
    }

    #[test]
    fn parse_normalizes_dates_like_js() {
        let text = "BEGIN:VEVENT\nDTSTART:20261232\nDTEND:20261233\nSUMMARY:Carried\nEND:VEVENT\n\
            BEGIN:VEVENT\nDTSTART:20261300\nDTEND:20261301\nSUMMARY:Day zero\nEND:VEVENT\n\
            BEGIN:VEVENT\nDTSTART:00500101\nDTEND:00500102\nSUMMARY:Short year\nEND:VEVENT\n\
            BEGIN:VEVENT\nDTSTART:20270228\nDTEND:20270301\nSUMMARY:Exclusive end\nEND:VEVENT\n";
        let holidays = Holidays::parse(text);
        assert_eq!(
            names(holidays.entries()),
            [
                (date(1950, 1, 1), "Short year", false),
                (date(2026, 12, 31), "Day zero", false),
                (date(2027, 1, 1), "Carried", false),
                (date(2027, 2, 28), "Exclusive end", false),
            ]
        );
    }

    #[test]
    fn an_event_without_an_end_is_one_day() {
        let text = "BEGIN:VEVENT\nDTSTART:20270405\nSUMMARY:Open ended\nEND:VEVENT\n";
        let holidays = Holidays::parse(text);
        assert_eq!(
            names(holidays.entries()),
            [(date(2027, 4, 5), "Open ended", false)]
        );
    }

    #[test]
    fn long_events_stop_at_a_year() {
        let text = "BEGIN:VEVENT\nDTSTART:10000101\nDTEND:99990101\nSUMMARY:Forever\nEND:VEVENT\n";
        let holidays = Holidays::parse(text);
        assert_eq!(holidays.entries().len(), MAX_EVENT_DAYS);
        assert_eq!(holidays.entries()[0].date, date(1000, 1, 1));
    }

    #[test]
    fn holidays_on_a_day_and_in_a_month() {
        let holidays = Holidays::parse(FEED);
        let day = holidays.on(date(2026, 12, 5));
        assert_eq!(day.len(), 3);
        assert!(!day[0].observance && !day[1].observance && day[2].observance);
        assert_eq!(holidays.range_on(date(2026, 12, 5)), 1..4);
        assert!(
            holidays
                .on(date(2026, 12, 6))
                .is_empty()
        );
        assert_eq!(holidays.range_on(date(2026, 12, 6)), 4..4);
        assert_eq!(holidays.in_month(2026, 12).len(), 5);
        assert_eq!(holidays.in_month(2027, 1).len(), 1);
        assert!(holidays.in_month(2027, 2).is_empty());
        assert_eq!(holidays.in_month(2027, 3).len(), 1);
    }

    #[test]
    fn upcoming_from_today() {
        let holidays = Holidays::parse(FEED);
        let today = date(2026, 12, 5);
        assert_eq!(holidays.upcoming(today, 0), 1..6);
        assert_eq!(holidays.upcoming(today, 1), 1..5);
        assert_eq!(holidays.upcoming(today, 2), 1..4);
        assert_eq!(
            holidays.upcoming(today, 3),
            1..4,
            "at most 2 count"
        );
        assert_eq!(holidays.upcoming(date(2027, 1, 2), 0), 6..7);
        assert_eq!(holidays.upcoming(date(2027, 3, 3), 0), 7..7);
        assert_eq!(Holidays::default().upcoming(today, 0), 0..0);
    }

    #[test]
    fn clean_text_cases() {
        for (raw, expected) in [
            ("Plain", "Plain"),
            ("a\\,b\\;c", "a,b;c"),
            ("line\\none\\Ntwo", "line one two"),
            ("back\\\\slash", "back\\slash"),
            ("\\\\n is not a space", "\\n is not a space"),
            ("\\x stays", "\\x stays"),
            ("trailing\\", "trailing\\"),
            ("zero\u{200B}width", "zerowidth"),
            ("\\\u{200B}n", "\\n"),
            ("\u{FEFF}\u{A0} spaced \u{3000}\t", "spaced"),
            ("\u{85}kept\u{85}", "kept"),
            ("\u{2028}line\u{2029}", "line"),
            ("\u{180E}kept\u{180E}", "\u{180E}kept\u{180E}"),
            ("\\n edge \\n", "edge"),
        ] {
            assert_eq!(clean_text(raw), expected, "{raw:?}");
        }
    }

    #[test]
    fn fields_like_the_regex() {
        assert_eq!(
            field("DTSTART;VALUE=DATE:20260101"),
            Some(("DTSTART", "20260101"))
        );
        assert_eq!(field("SUMMARY:a: b"), Some(("SUMMARY", "a: b")));
        assert_eq!(field("X-1:y"), Some(("X-", "y")));
        assert_eq!(field("summary:x"), None);
        assert_eq!(field("NOCOLON"), None);
        assert_eq!(field("SUMMARY:a\rb"), None);
        assert_eq!(field("SUMMARY:a\u{2028}b"), None);
        assert_eq!(
            field("SUMMARY;X=\u{2028}:ok"),
            Some(("SUMMARY", "ok"))
        );
        assert_eq!(
            unfold("a\r\n b\n\tc\r\n\r\n d\r\nx"),
            "abc\r\nd\r\nx"
        );
        assert_eq!(date_digits("2026120"), None);
        assert_eq!(date_digits("2026x205"), None);
        assert_eq!(date_digits("20261205T00"), Some((2026, 12, 5)));
    }
}
