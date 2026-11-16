//! The calendar panel with a fixed clock and the made-up feed in `fixtures/holidays.ics`.

use std::sync::Arc;

use deskshell::panel::calendar::{Calendar, Message};
use deskshell::panel::{self, Contents, Sources};
use deskshell::panels::{Manager, Op, Panel};
use deskshell_core::calendar::{Holidays, PickerCell, View};
use deskshell_ui::Theme;
use jiff::civil::{Date, date};

const FEED: &str = include_str!("fixtures/holidays.ics");

fn today() -> Date {
    date(2026, 11, 13)
}

fn next_day() -> Date {
    date(2026, 11, 14)
}

fn opened() -> Calendar {
    let mut calendar = Calendar::with_clock(today);
    calendar.set_holidays(Arc::new(Holidays::parse(FEED)));
    calendar.open();
    calendar
}

fn upcoming(calendar: &Calendar) -> Vec<(Date, String, String)> {
    calendar
        .upcoming()
        .map(|(holiday, relative)| {
            (
                holiday.date,
                holiday.name.to_string(),
                relative.to_owned(),
            )
        })
        .collect()
}

#[test]
fn open_resets_to_today() {
    let mut calendar = opened();
    calendar.update(Message::Navigate(5));
    calendar.update(Message::Promote);
    calendar.update(Message::Select(date(2030, 1, 1)));
    calendar.update(Message::Promote);
    calendar.update(Message::Promote);
    assert_eq!(calendar.state().view(), View::Years);
    calendar.open();
    assert_eq!(calendar.state().view(), View::Days);
    assert_eq!(calendar.state().shown(), (2026, 11));
    assert_eq!(calendar.state().selected(), today());
    assert_eq!(calendar.state().today(), today());
    assert_eq!(calendar.hovered(), None);
}

#[test]
fn the_month_grid() {
    let calendar = opened();
    let days = calendar.days();
    assert_eq!(days.len(), 42);
    assert_eq!(
        days[0].date,
        date(2026, 11, 1),
        "November 2026 starts on a Sunday"
    );
    assert_eq!(days[41].date, date(2026, 12, 12));
    let flagged: Vec<usize> = days
        .iter()
        .enumerate()
        .filter(|(_, cell)| cell.dot)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(flagged, [12, 19, 23, 24, 34, 39]);
    let today = &days[12];
    assert!(
        today.today && today.selected && today.dot && !today.holiday,
        "an observance"
    );
    assert_eq!(today.label, "13");
    assert!(days[19].holiday, "Founders Day");
    assert!(
        !days[23].holiday && days[23].dot,
        "an observance over two days"
    );
    assert!(
        days[34].holiday && days[34].dim,
        "December 5 in the next month"
    );
    assert_eq!(
        days.iter()
            .filter(|cell| cell.dim)
            .count(),
        12
    );
    assert_eq!(calendar.title(), ("November", "2026"));
}

#[test]
fn the_wheel_navigates_in_each_view() {
    let mut calendar = opened();
    calendar.update(Message::Wheel(-1.0));
    assert_eq!(
        calendar.state().shown(),
        (2026, 12),
        "down is the next month"
    );
    calendar.update(Message::Wheel(-0.25));
    assert_eq!(calendar.state().shown(), (2027, 1));
    calendar.update(Message::Wheel(1.0));
    assert_eq!(calendar.state().shown(), (2026, 12));
    calendar.update(Message::Wheel(0.0));
    assert_eq!(
        calendar.state().shown(),
        (2026, 11),
        "no vertical motion goes back, as `angleDelta.y < 0 ? 1 : -1` does"
    );
    calendar.update(Message::Promote);
    calendar.update(Message::Wheel(-1.0));
    assert_eq!(calendar.state().shown(), (2027, 11), "a year");
    calendar.update(Message::Promote);
    calendar.update(Message::Wheel(-1.0));
    assert_eq!(calendar.state().shown(), (2037, 11), "a decade");
    assert_eq!(calendar.title(), ("2030 – 2039", ""));
}

#[test]
fn the_header_buttons() {
    let mut calendar = opened();
    calendar.update(Message::Navigate(1));
    assert_eq!(calendar.state().shown(), (2026, 12));
    assert_eq!(calendar.title(), ("December", "2026"));
    calendar.update(Message::Navigate(-1));
    calendar.update(Message::Navigate(-1));
    assert_eq!(calendar.state().shown(), (2026, 10));

    calendar.update(Message::Promote);
    assert_eq!(calendar.state().view(), View::Months);
    assert_eq!(calendar.title(), ("2026", ""));
    calendar.update(Message::Promote);
    assert_eq!(calendar.state().view(), View::Years);
    assert_eq!(calendar.title(), ("2020 – 2029", ""));
    calendar.update(Message::Promote);
    assert_eq!(
        calendar.state().view(),
        View::Years,
        "the decade header takes no clicks"
    );

    calendar.update(Message::Select(date(2026, 11, 20)));
    calendar.update(Message::Navigate(4));
    calendar.update(Message::Today);
    assert_eq!(calendar.state().shown(), (2026, 11));
    assert_eq!(calendar.state().selected(), today());
    assert_eq!(calendar.state().view(), View::Days);
}

#[test]
fn the_pickers() {
    let mut calendar = opened();
    calendar.update(Message::Promote);
    let cells: Vec<(PickerCell, String)> = calendar
        .pickers()
        .map(|(cell, label)| (*cell, label.to_owned()))
        .collect();
    assert_eq!(cells.len(), 16);
    assert_eq!(cells[0].1, "Jan");
    assert_eq!(
        cells[10],
        (
            PickerCell { year: 2026, month: 11, dim: false, today: true },
            "Nov".to_owned()
        )
    );
    assert_eq!(
        cells[13],
        (
            PickerCell { year: 2027, month: 2, dim: true, today: false },
            "Feb".to_owned()
        )
    );
    calendar.update(Message::Cell(cells[13].0));
    assert_eq!(calendar.state().view(), View::Days);
    assert_eq!(calendar.state().shown(), (2027, 2));
    assert_eq!(calendar.days()[0].date, date(2027, 1, 31));
    assert!(calendar.days()[14].dot, "Garden Day");

    calendar.update(Message::Promote);
    calendar.update(Message::Promote);
    let years: Vec<(PickerCell, String)> = calendar
        .pickers()
        .map(|(cell, label)| (*cell, label.to_owned()))
        .collect();
    assert_eq!(years[0].1, "2020");
    assert!(years[6].0.today);
    assert_eq!(
        years[12],
        (
            PickerCell { year: 2032, month: 2, dim: true, today: false },
            "2032".to_owned()
        )
    );
    calendar.update(Message::Cell(years[3].0));
    assert_eq!(calendar.state().view(), View::Months);
    assert_eq!(calendar.state().shown(), (2023, 2));
}

#[test]
fn selecting_a_day() {
    let mut calendar = opened();
    assert_eq!(
        calendar.summary(),
        ("Friday", "13 November", "B.E. 2569")
    );
    calendar.update(Message::Select(next_day()));
    assert_eq!(
        calendar.summary(),
        ("Saturday · Tomorrow", "14 November", "B.E. 2569")
    );
    assert!(calendar.days()[13].selected && !calendar.days()[12].selected);

    calendar.update(Message::Select(date(2026, 12, 5)));
    assert_eq!(
        calendar.state().shown(),
        (2026, 12),
        "a dim cell shows its month"
    );
    assert_eq!(
        calendar.summary(),
        ("Saturday · 22d", "5 December", "B.E. 2569")
    );

    calendar.update(Message::Select(date(2027, 1, 1)));
    assert_eq!(
        calendar.summary(),
        ("Friday · 49d", "1 January", "2027 · B.E. 2570")
    );
    calendar.update(Message::Select(date(2026, 11, 12)));
    assert_eq!(calendar.summary().0, "Thursday · Yesterday");
    calendar.update(Message::Select(date(2025, 12, 31)));
    assert_eq!(
        calendar.summary(),
        (
            "Wednesday · 317d ago",
            "31 December",
            "2025 · B.E. 2568"
        )
    );
}

#[test]
fn the_side_pane_with_holidays() {
    let mut calendar = opened();
    let names: Vec<&str> = calendar
        .selected_holidays()
        .iter()
        .map(|holiday| &*holiday.name)
        .collect();
    assert_eq!(names, ["Harvest Moon Day"]);
    assert!(calendar.selected_holidays()[0].observance);
    assert_eq!(
        upcoming(&calendar),
        [
            (
                date(2026, 11, 13),
                "Harvest Moon Day".to_owned(),
                "Today".to_owned()
            ),
            (
                date(2026, 11, 20),
                "Founders Day".to_owned(),
                "7d".to_owned()
            ),
            (
                date(2026, 11, 24),
                "River Lights Festival (เทศกาลแสงไฟ)".to_owned(),
                "11d".to_owned()
            ),
            (
                date(2026, 11, 25),
                "River Lights Festival (เทศกาลแสงไฟ)".to_owned(),
                "12d".to_owned()
            ),
        ],
        "5 rows less one for the selected day's holiday"
    );
    assert_eq!(calendar.empty_text(), None);

    calendar.update(Message::Select(date(2026, 12, 5)));
    let day: Vec<(&str, bool)> = calendar
        .selected_holidays()
        .iter()
        .map(|holiday| (&*holiday.name, holiday.observance))
        .collect();
    assert_eq!(
        day,
        [
            (
                "National Unity Day, a long name that has to wrap onto a second line",
                false
            ),
            ("Lantern Evening", true),
        ],
        "public holidays first"
    );
    assert_eq!(upcoming(&calendar).len(), 3, "5 rows less two");

    calendar.update(Message::Select(date(2026, 11, 16)));
    assert!(calendar.selected_holidays().is_empty());
    assert_eq!(upcoming(&calendar).len(), 5);
    assert_eq!(upcoming(&calendar)[4].0, date(2026, 12, 5));
}

#[test]
fn the_empty_texts() {
    let mut calendar = Calendar::with_clock(today);
    calendar.open();
    assert_eq!(
        calendar.empty_text(),
        Some("Holiday calendar unavailable")
    );
    assert_eq!(calendar.upcoming().count(), 0);
    assert!(
        calendar
            .days()
            .iter()
            .all(|cell| !cell.dot)
    );

    calendar.set_holidays(Arc::new(Holidays::parse(
        "BEGIN:VEVENT\r\nDTSTART;VALUE=DATE:20200101\r\nDTEND;VALUE=DATE:20200102\r\nSUMMARY:Past\r\nEND:VEVENT\r\n",
    )));
    assert_eq!(
        calendar.empty_text(),
        Some("No upcoming holidays")
    );

    calendar.set_holidays(Arc::new(Holidays::parse(FEED)));
    calendar.update(Message::Refresh);
    assert_eq!(calendar.empty_text(), None);
    assert!(
        calendar.days()[19].holiday,
        "a new feed rebuilds the grid"
    );
}

#[test]
fn hover_follows_enter_and_leave() {
    let mut calendar = opened();
    calendar.update(Message::Hover(3));
    assert_eq!(calendar.hovered(), Some(3));
    calendar.update(Message::Hover(4));
    calendar.update(Message::Unhover(3));
    assert_eq!(
        calendar.hovered(),
        Some(4),
        "a late leave of the old cell changes nothing"
    );
    calendar.update(Message::Navigate(1));
    assert_eq!(
        calendar.hovered(),
        Some(4),
        "the pointer stays over the cell"
    );
    calendar.update(Message::Promote);
    assert_eq!(calendar.hovered(), None, "the grid is gone");
    calendar.update(Message::Hover(1));
    calendar.update(Message::Unhover(1));
    assert_eq!(calendar.hovered(), None);
}

#[test]
fn midnight_moves_today_only() {
    let mut calendar = opened();
    calendar.update(Message::Navigate(1));
    calendar.update(Message::Midnight(next_day()));
    assert_eq!(calendar.state().today(), next_day());
    assert_eq!(
        calendar.state().selected(),
        today(),
        "the selection stays"
    );
    assert_eq!(calendar.state().shown(), (2026, 12));
    calendar.update(Message::Navigate(-1));
    assert!(calendar.days()[13].today && !calendar.days()[12].today);
    assert_eq!(calendar.summary().0, "Friday · Yesterday");
    assert_eq!(
        upcoming(&calendar)[0].2,
        "6d",
        "Founders Day from the new today"
    );
}

#[test]
fn the_seam_routes_the_calendar() {
    let theme = Theme::default();
    let sources = Sources::default();
    let env = sources.env(&theme);
    let mut contents = Contents::default();
    let _ = contents.opened(Panel::Calendar, &env);
    let mut effects = Vec::new();
    let _ = contents.update(
        panel::Message::Calendar(Message::Navigate(1)),
        &env,
        &mut effects,
    );
    assert!(
        effects.is_empty(),
        "the calendar asks the shell for nothing"
    );
    assert!(
        contents
            .view(Panel::Calendar, &env)
            .is_some()
    );

    assert!(
        Panel::Calendar.focus_grab(),
        "an outside click closes it"
    );
    let mut manager = Manager::default();
    let open = manager
        .command(Panel::Calendar, Op::Toggle, None, || 7_u32)
        .open
        .unwrap();
    assert_eq!(manager.dismiss(open.id), Some(7));
    assert!(manager.current().is_none());
}
