//! The pending packages with the version change highlighted, as the updates sheet's details.

use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use deskshell_system::{Package, Updates};
use deskshell_ui::{Theme, clip_x, icon, pill_button, scroll};
use iced::font::Weight;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Column, Space, container, row, text};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding};

pub const ROW: f32 = 32.0;
const AUR_ROW: f32 = 26.0;
const VISIBLE_ROWS: f32 = 6.0;
const LIST_MARGIN: f32 = 6.0;
const NAME_WIDTH: f32 = 230.0;
const LINE: f32 = 1.3;

pub fn pkgver(version: &str) -> &str {
    match version.rfind('-') {
        Some(index) if index > 0 => &version[..index],
        _ => version,
    }
}

pub fn version_parts<'a>(old: &'a str, new: &'a str) -> (&'a str, &'a str, &'a str) {
    let same = pkgver(old) == pkgver(new);
    let (from, to) = if same { (old, new) } else { (pkgver(old), pkgver(new)) };
    let mut shared = 0;
    for (index, (a, b)) in from.bytes().zip(to.bytes()).enumerate() {
        if a != b {
            break;
        }
        if b".-:+_~".contains(&a) {
            shared = index + 1;
        }
    }
    (from, &to[..shared], &to[shared..])
}

pub fn ago(now: SystemTime, time: SystemTime, out: &mut String) {
    let ms = |at: SystemTime| {
        at.duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis() as i128)
    };
    let minutes = (ms(now) - ms(time)).div_euclid(60_000);
    if minutes < 1 {
        out.push_str("just now");
    } else if minutes < 60 {
        let _ = write!(out, "{minutes} min ago");
    } else {
        let _ = write!(out, "{} h ago", minutes / 60);
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Row {
    name: String,
    from: String,
    shared: String,
    rest: String,
    reboot: bool,
}

#[derive(Debug, Default)]
pub struct List {
    header: String,
    error: String,
    checking: bool,
    rows: Vec<Row>,
    official: usize,
}

fn row_of(package: &Package, out: &mut Row) {
    package.name.clone_into(&mut out.name);
    let (from, shared, rest) = version_parts(&package.old, &package.new);
    out.from.clear();
    let _ = write!(out.from, "{from} → ");
    shared.clone_into(&mut out.shared);
    rest.clone_into(&mut out.rest);
    out.reboot = package.reboot;
}

impl List {
    pub fn sync(&mut self, updates: &Updates, now: SystemTime) {
        self.checking = updates.checking;
        self.header.clear();
        if updates.checking {
            self.header
                .push_str("Checking for updates…");
        } else {
            let count = updates.packages.len();
            let _ = write!(
                self.header,
                "{count} package{} · checked ",
                if count == 1 { "" } else { "s" }
            );
            ago(
                now,
                updates.checked_at.unwrap_or(UNIX_EPOCH),
                &mut self.header,
            );
        }
        self.error.clear();
        if let Some(error) = &updates.error {
            let _ = write!(self.error, "The last check failed: {error}");
        }
        let (official, aur) = deskshell_system::split_updates(&updates.packages);
        self.official = official.len();
        self.rows
            .resize_with(updates.packages.len(), Row::default);
        for (package, out) in official
            .into_iter()
            .chain(aur)
            .zip(&mut self.rows)
        {
            row_of(package, out);
        }
    }

    pub fn header(&self) -> &str {
        &self.header
    }

    pub fn error(&self) -> &str {
        &self.error
    }

    pub fn highlights(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.rows.iter().map(|row| {
            (
                row.from.as_str(),
                row.shared.as_str(),
                row.rest.as_str(),
            )
        })
    }

    pub fn content_height(&self) -> f32 {
        if self.rows.is_empty() {
            return ROW;
        }
        let aur = if self.rows.len() > self.official { AUR_ROW } else { 0.0 };
        self.rows.len() as f32 * ROW + aur
    }

    pub fn view<'a, M: Clone + 'a>(&'a self, theme: &'a Theme, refresh: M) -> Element<'a, M> {
        let colors = &theme.colors;
        let header = row![
            clip_x(
                container(
                    text(self.header.as_str())
                        .font(theme.font(Weight::Normal))
                        .size(12)
                        .line_height(LineHeight::Relative(LINE))
                        .color(colors.text_secondary)
                        .wrapping(Wrapping::None),
                )
                .width(Length::Fill),
            ),
            pill_button(theme, "refresh", "")
                .height(28.0)
                .on_press_maybe((!self.checking).then_some(refresh)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let mut rows = Column::with_capacity(self.rows.len() + 2);
        if self.rows.is_empty() {
            let empty = if self.checking {
                "Looking for pending packages…"
            } else {
                "No pending updates found."
            };
            rows = rows.push(
                container(
                    text(empty)
                        .font(theme.font(Weight::Normal))
                        .size(12)
                        .color(colors.text_muted),
                )
                .padding(Padding { left: 10.0, ..Padding::ZERO })
                .height(ROW)
                .center_y(ROW),
            );
        }
        for (index, entry) in self.rows.iter().enumerate() {
            if index == self.official {
                rows = rows.push(aur_header(theme));
            }
            rows = rows.push(package_row(theme, entry));
        }
        let height = self
            .content_height()
            .min(ROW * VISIBLE_ROWS);
        let background = colors.container;
        let list = container(
            scroll(rows)
                .width(Length::Fill)
                .height(height),
        )
        .padding(LIST_MARGIN)
        .width(Length::Fill)
        .height(height + 2.0 * LIST_MARGIN)
        .style(move |_| container::Style {
            background: Some(Background::Color(background)),
            border: Border { radius: 16.0.into(), ..Border::default() },
            ..container::Style::default()
        });
        let error = (!self.error.is_empty()).then(|| {
            clip_x(
                container(
                    text(self.error.as_str())
                        .font(theme.font(Weight::Normal))
                        .size(12)
                        .line_height(LineHeight::Relative(LINE))
                        .color(colors.warning)
                        .wrapping(Wrapping::None),
                )
                .width(Length::Fill),
            )
        });
        Column::with_capacity(3)
            .push(header)
            .push(error)
            .push(list)
            .spacing(8)
            .width(Length::Fill)
            .into()
    }
}

fn aur_header<'a, M: 'a>(theme: &Theme) -> Element<'a, M> {
    let colors = &theme.colors;
    let line = colors.border_muted;
    row![
        Space::new().width(10),
        text("AUR")
            .font(theme.font(Weight::Semibold))
            .size(11)
            .color(colors.text_muted),
        Space::new().width(8),
        container(Space::new())
            .width(Length::Fill)
            .height(1)
            .style(move |_| container::Style {
                background: Some(Background::Color(line)),
                ..container::Style::default()
            }),
        Space::new().width(10),
    ]
    .align_y(Alignment::Center)
    .height(AUR_ROW)
    .into()
}

fn package_row<'a, M: 'a>(theme: &'a Theme, entry: &'a Row) -> Element<'a, M> {
    let colors = &theme.colors;
    let name = clip_x(
        container(
            text(entry.name.as_str())
                .font(theme.font(Weight::Semibold))
                .size(13)
                .line_height(LineHeight::Relative(LINE))
                .color(colors.text_primary)
                .wrapping(Wrapping::None),
        )
        .width(NAME_WIDTH),
    );
    // Three texts, not one rich text: cosmic-text shapes a word in one font, so a bold
    // span inside "6.17.9" would stay regular.
    let part = |content: &'a str, weight: Weight, color: Color| {
        text(content)
            .font(theme.font(weight))
            .size(12)
            .line_height(LineHeight::Relative(LINE))
            .color(color)
            .wrapping(Wrapping::None)
    };
    let version = clip_x(
        container(row![
            part(&entry.from, Weight::Normal, colors.text_muted),
            part(
                &entry.shared,
                Weight::Normal,
                colors.text_secondary
            ),
            part(&entry.rest, Weight::Bold, colors.info),
        ])
        .width(Length::Fill),
    );
    let mut content = row![
        icon("package_2")
            .size(16.0)
            .color(colors.text_muted),
        name,
        version,
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    if entry.reboot {
        content = content.push(reboot_chip(theme));
    }
    container(content)
        .padding(Padding { left: 10.0, right: 8.0, ..Padding::ZERO })
        .width(Length::Fill)
        .height(ROW)
        .center_y(ROW)
        .into()
}

fn reboot_chip<'a, M: 'a>(theme: &Theme) -> Element<'a, M> {
    let colors = &theme.colors;
    let ink: Color = colors.warning;
    let base = colors.warning_container;
    container(
        row![
            icon("restart_alt")
                .size(14.0)
                .color(ink),
            text("Reboot")
                .font(theme.font(Weight::Semibold))
                .size(11)
                .color(ink),
        ]
        .spacing(4)
        .align_y(Alignment::Center),
    )
    .padding(Padding { left: 7.0, right: 7.0, ..Padding::ZERO })
    .height(22)
    .center_y(22)
    .style(move |_| container::Style {
        background: Some(Background::Color(base)),
        border: Border { radius: 11.0.into(), ..Border::default() },
        ..container::Style::default()
    })
    .into()
}
