//! One notification entry: summary and time, body, actions, reply field, image and buttons.

mod inline;
pub mod markup;
pub mod pixels;

use deskshell_notify::{Entry, Urgency};
use deskshell_ui::text::{Tone, mixed};
use deskshell_ui::theme::dim;
use deskshell_ui::{Picture, Theme, clip_x, icon, pill_button, scroll, text_input};
use iced::advanced::text::Paragraph as _;
use iced::font::{Style as FontStyle, Weight};
use iced::widget::image::Handle;
use iced::widget::scrollable::Scrollbar;
use iced::widget::text::{LineHeight, Span, Wrapping};
use iced::widget::{
    Column, Row, Space, button, container, image, mouse_area, rich_text, scrollable, stack,
};
use iced::{
    Background, Border, Color, ContentFit, Element, Font, Length, Padding, Pixels, Size, alignment,
};

use inline::InlineSummary;
pub use markup::Body;

const SUMMARY_SIZE: f32 = 14.0;
const TIME_SIZE: f32 = 12.0;
const BODY_SIZE: f32 = 13.0;
// IBM Plex's line box is 1.3 em.
const LINE: f32 = 1.3;
const BODY_LINE: f32 = LINE * 1.2;
// IBM Plex's ascender in em. With equal line boxes the baseline sits this far down.
const ASCENT: f32 = 1.025;
const ACTION_HEIGHT: f32 = 34.0;
const ACTION_LABEL: f32 = 150.0;
const REPLY_HEIGHT: f32 = 38.0;
const THUMBNAIL: f32 = 64.0;
const SIDE: f32 = 28.0;
const HOVER_CLOSE: f32 = 22.0;

type Paragraph = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings<'a> {
    pub time_label: &'a str,
    pub expanded: bool,
    pub group_count: usize,
    pub group_expanded: bool,
    pub show_close: bool,
    pub close_on_hover: bool,
    pub time_inline: bool,
    pub primary_action: bool,
    pub collapsed_lines: u16,
    pub expanded_lines: u16,
    pub show_image: bool,
    pub allow_reply: bool,
}

impl Default for Settings<'_> {
    fn default() -> Self {
        Self {
            time_label: "",
            expanded: false,
            group_count: 0,
            group_expanded: false,
            show_close: true,
            close_on_hover: false,
            time_inline: false,
            primary_action: false,
            collapsed_lines: 3,
            expanded_lines: 12,
            show_image: true,
            allow_reply: true,
        }
    }
}

impl Settings<'_> {
    pub fn side_column(&self, truncated: bool) -> bool {
        (self.show_close && !self.close_on_hover) || truncated || self.expanded
    }
}

pub fn time_label(app_name: &str, time_text: &str) -> String {
    match (app_name.is_empty(), time_text.is_empty()) {
        (true, _) => time_text.to_owned(),
        (false, true) => app_name.to_owned(),
        (false, false) => format!("{app_name} · {time_text}"),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Content<'a> {
    pub entry: &'a Entry,
    pub body: &'a Body,
    pub image: Option<&'a Handle>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub show_all_actions: bool,
    pub hovered: bool,
    pub reply: String,
}

impl State {
    pub fn entry_changed(&mut self) {
        self.show_all_actions = false;
    }

    pub fn update(&mut self, message: Message, entry: &Entry, body: &Body) -> Option<Outcome> {
        match message {
            Message::Close => Some(Outcome::Close),
            Message::ToggleExpand => Some(Outcome::ToggleExpand),
            Message::ToggleGroup => Some(Outcome::ToggleGroup),
            Message::Action(index) => entry
                .actions
                .get(index)
                .map(|action| Outcome::Invoke(action.identifier.clone())),
            Message::ShowAllActions => {
                self.show_all_actions = true;
                None
            }
            Message::Hover(hovered) => {
                self.hovered = hovered;
                None
            }
            Message::ReplyInput(text) => {
                self.reply = text;
                None
            }
            Message::ReplySend => Some(Outcome::Reply(std::mem::take(&mut self.reply))),
            Message::Link(index) => body
                .links
                .get(index)
                .map(|link| Outcome::OpenLink(link.clone())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Close,
    ToggleExpand,
    ToggleGroup,
    Action(usize),
    ShowAllActions,
    Hover(bool),
    ReplyInput(String),
    ReplySend,
    Link(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Close,
    ToggleExpand,
    ToggleGroup,
    Invoke(String),
    Reply(String),
    OpenLink(String),
}

pub fn label_width(theme: &Theme, content: &str) -> f32 {
    if content.is_empty() {
        return 0.0;
    }
    line_width(theme, content, TIME_SIZE, Weight::Normal)
}

fn line_width(theme: &Theme, content: &str, size: f32, weight: Weight) -> f32 {
    let thai = theme.thai_font(weight);
    let spans: Vec<Span<'_, (), Font>> = deskshell_ui::text::thai_runs(content)
        .map(|(run, is_thai)| {
            let span = Span::new(run);
            if is_thai { span.font(thai) } else { span }
        })
        .collect();
    Paragraph::with_spans(iced::advanced::Text {
        content: spans.as_slice(),
        bounds: Size::INFINITE,
        size: Pixels(size),
        line_height: LineHeight::Relative(LINE),
        font: theme.font(weight),
        align_x: iced::advanced::text::Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping: iced::advanced::text::Shaping::Advanced,
        wrapping: Wrapping::None,
    })
    .min_width()
}

/// It shapes the text, so call it once per entry and width, not while drawing.
pub fn truncated(
    theme: &Theme,
    entry: &Entry,
    body: &Body,
    summary_width: f32,
    body_width: f32,
    collapsed_lines: u16,
) -> bool {
    if line_width(
        theme,
        &entry.summary,
        SUMMARY_SIZE,
        Weight::Semibold,
    ) > summary_width + 0.5
    {
        return true;
    }
    if body.is_empty() {
        return false;
    }
    let spans = body_spans(theme, body, Color::BLACK, Color::BLACK);
    let paragraph = Paragraph::with_spans(iced::advanced::Text {
        content: spans.as_slice(),
        bounds: Size::new(body_width.max(1.0), f32::INFINITY),
        size: Pixels(BODY_SIZE),
        line_height: LineHeight::Relative(BODY_LINE),
        font: theme.font(Weight::Normal),
        align_x: iced::advanced::text::Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping: iced::advanced::text::Shaping::Advanced,
        wrapping: Wrapping::WordOrGlyph,
    });
    paragraph.min_height() > f32::from(collapsed_lines) * BODY_SIZE * BODY_LINE + 0.5
}

fn body_spans<'a>(
    theme: &Theme,
    body: &'a Body,
    color: Color,
    link_color: Color,
) -> Vec<Span<'a, usize, Font>> {
    body.runs
        .iter()
        .map(|run| {
            let style = run.style;
            let weight = if style.bold { Weight::Bold } else { Weight::Normal };
            let mut font = if style.mono {
                Font { weight, ..Font::MONOSPACE }
            } else if run.thai {
                theme.thai_font(weight)
            } else {
                theme.font(weight)
            };
            if style.italic {
                font.style = FontStyle::Italic;
            }
            let mut span = Span::new(run.text.as_str())
                .font(font)
                .underline(style.underline)
                .strikethrough(style.strike);
            span = match style.link {
                Some(index) => span.link(index).color(link_color),
                None => span.color(
                    style
                        .color
                        .map_or(color, |c| Color { a: c.a * color.a, ..c }),
                ),
            };
            if let Some(scale) = style.scale {
                span = span.size(BODY_SIZE * scale);
            }
            span
        })
        .collect()
}

pub fn view<'a>(
    theme: &'a Theme,
    content: Content<'a>,
    state: &'a State,
    settings: &Settings<'a>,
    alpha: f32,
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let ink = |color: Color| dim(color, alpha);
    let entry = content.entry;
    let hover_close = settings.close_on_hover && settings.show_close && state.hovered;

    let summary = mixed(
        theme,
        &entry.summary,
        Tone::Primary,
        Weight::Semibold,
    )
    .size(SUMMARY_SIZE)
    .line_height(LineHeight::Relative(LINE))
    .color(ink(colors.text_primary))
    .wrapping(if settings.expanded { Wrapping::WordOrGlyph } else { Wrapping::None });
    let inline_time = (settings.time_inline && !settings.time_label.is_empty()).then(|| {
        Element::from(
            mixed(
                theme,
                settings.time_label,
                Tone::Muted,
                Weight::Normal,
            )
            .size(TIME_SIZE)
            .line_height(LineHeight::Relative(LINE))
            .color(ink(colors.text_muted))
            .wrapping(Wrapping::None),
        )
    });
    let summary_lines = if settings.expanded { 6.0 } else { 1.0 };
    let summary = if settings.expanded { clip_x(summary).keep_bottom() } else { clip_x(summary) };
    let mut top = Row::with_capacity(3)
        .spacing(8)
        .push(InlineSummary::new(
            summary,
            inline_time,
            summary_lines * SUMMARY_SIZE * LINE,
            ASCENT * (SUMMARY_SIZE - TIME_SIZE),
        ));
    if !settings.time_inline && !settings.time_label.is_empty() && !hover_close {
        top = top.push(clip_x(
            container(
                mixed(
                    theme,
                    settings.time_label,
                    Tone::Muted,
                    Weight::Normal,
                )
                .size(TIME_SIZE)
                .line_height(LineHeight::Relative(LINE))
                .color(ink(colors.text_muted))
                .wrapping(Wrapping::None),
            )
            .max_width(140)
            .padding(Padding { top: 2.0, ..Padding::ZERO }),
        ));
    }
    if hover_close {
        top = top.push(round_button(
            theme,
            "close",
            HOVER_CLOSE,
            17.0,
            None,
            alpha,
            Message::Close,
        ));
    }

    let mut left = Column::with_capacity(4)
        .spacing(5)
        .width(Length::Fill)
        .push(top);
    if !content.body.is_empty() {
        left = left.push(body_view(theme, content, settings, alpha));
    }
    if entry.live && !entry.actions.is_empty() {
        left = left.push(actions_view(theme, entry, state, settings, alpha));
    }
    if settings.allow_reply
        && entry.live
        && let Some(placeholder) = &entry.inline_reply
    {
        left = left.push(reply_view(theme, placeholder, state, alpha));
    }

    let mut root = Row::with_capacity(3)
        .spacing(8)
        .push(left);
    if settings.show_image
        && let Some(handle) = content.image
    {
        // Cover relies on the renderer's clip bounds, which wgpu honours and the tiny-skia
        // offscreen renderer does not.
        let thumbnail = image(handle.clone())
            .width(THUMBNAIL)
            .height(THUMBNAIL)
            .content_fit(ContentFit::Cover)
            .border_radius(12)
            .opacity(alpha);
        let background = ink(colors.container_high);
        root = root.push(
            container(Picture::new(handle, thumbnail))
                .width(THUMBNAIL)
                .height(THUMBNAIL)
                .style(move |_| container::Style {
                    background: Some(Background::Color(background)),
                    border: Border { radius: 12.0.into(), ..Border::default() },
                    ..container::Style::default()
                }),
        );
    }
    if settings.side_column(content.truncated) {
        let mut side = Column::with_capacity(3)
            .spacing(4)
            .width(SIDE)
            .height(Length::Fill);
        if settings.show_close && !settings.close_on_hover {
            side = side.push(round_button(
                theme,
                "close",
                SIDE,
                18.0,
                None,
                alpha,
                Message::Close,
            ));
        }
        side = side.push(Space::new().height(Length::Fill));
        if content.truncated || settings.expanded {
            side = side.push(round_button(
                theme,
                if settings.expanded { "expand_less" } else { "expand_more" },
                SIDE,
                20.0,
                Some(colors.container_high),
                alpha,
                Message::ToggleExpand,
            ));
        }
        root = root.push(side);
    }
    // The side column fills the entry's height, which comes from the content. Without
    // this the row would take the fill on, and a parent row would cut it to its others.
    let root = root.height(Length::Shrink);

    if settings.close_on_hover {
        mouse_area(root)
            .on_enter(Message::Hover(true))
            .on_exit(Message::Hover(false))
            .into()
    } else {
        root.into()
    }
}

fn body_view<'a>(
    theme: &'a Theme,
    content: Content<'a>,
    settings: &Settings<'a>,
    alpha: f32,
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let color = if content.entry.urgency == Urgency::Critical {
        colors.text_primary
    } else {
        colors.text_secondary
    };
    let spans = body_spans(
        theme,
        content.body,
        dim(color, alpha),
        dim(colors.tint, alpha),
    );
    let body = rich_text(spans)
        .size(BODY_SIZE)
        .line_height(LineHeight::Relative(BODY_LINE))
        .font(theme.font(Weight::Normal))
        .wrapping(Wrapping::WordOrGlyph)
        .on_link_click(Message::Link);
    if settings.expanded {
        let limit = f32::from(settings.expanded_lines) * (BODY_SIZE * BODY_LINE).ceil();
        let scroller = dim(colors.text_muted, alpha);
        container(
            scroll(container(body).padding(Padding { right: 8.0, ..Padding::ZERO }))
                .scrollbar(
                    Scrollbar::new()
                        .width(3)
                        .scroller_width(3)
                        .margin(0),
                )
                .style(move |theme, status| {
                    let mut style = scrollable::default(theme, status);
                    style.vertical_rail.background = None;
                    style.vertical_rail.border = Border::default();
                    style.vertical_rail.scroller.background = Background::Color(scroller);
                    style.vertical_rail.scroller.border =
                        Border { radius: 2.0.into(), ..Border::default() };
                    style
                }),
        )
        .max_height(limit)
        .into()
    } else {
        clip_x(
            container(body).max_height(f32::from(settings.collapsed_lines) * BODY_SIZE * BODY_LINE),
        )
        .keep_bottom()
        .into()
    }
}

fn actions_view<'a>(
    theme: &'a Theme,
    entry: &'a Entry,
    state: &State,
    settings: &Settings<'a>,
    alpha: f32,
) -> Element<'a, Message> {
    let colors = &theme.colors;
    let shown = if state.show_all_actions { entry.actions.len() } else { 2 };
    let mut pills = Row::with_capacity(shown.min(entry.actions.len()) + 1).spacing(6);
    for (index, action) in entry
        .actions
        .iter()
        .take(shown)
        .enumerate()
    {
        let primary = settings.primary_action && index == 0;
        let (base, ink) = if primary {
            (colors.accent, colors.accent_text)
        } else {
            (colors.container_highest, colors.text_primary)
        };
        pills = pills.push(
            pill_button(theme, "", &action.label)
                .base(dim(base, alpha))
                .ink(dim(ink, alpha))
                .max_label_width(ACTION_LABEL)
                .height(ACTION_HEIGHT)
                .on_press(Message::Action(index)),
        );
    }
    if entry.actions.len() > 2 && !state.show_all_actions {
        pills = pills.push(
            pill_button(theme, "more_horiz", "")
                .base(dim(colors.container_high, alpha))
                .ink(dim(colors.text_primary, alpha))
                .width(ACTION_HEIGHT)
                .height(ACTION_HEIGHT)
                .on_press(Message::ShowAllActions),
        );
    }
    container(pills.wrap().vertical_spacing(6))
        .padding(Padding { top: 8.0, ..Padding::ZERO })
        .into()
}

fn reply_view<'a>(
    theme: &'a Theme,
    placeholder: &'a str,
    state: &'a State,
    alpha: f32,
) -> Element<'a, Message> {
    let colors = theme.colors;
    let send = button(
        container(
            icon("arrow_upward")
                .size(17.0)
                .fill(true)
                .color(dim(colors.accent_text, alpha)),
        )
        .center(30),
    )
    .padding(0)
    .width(30)
    .height(30)
    .on_press(Message::ReplySend)
    .style(move |_, _| button::Style {
        background: Some(Background::Color(dim(colors.accent, alpha))),
        border: Border { radius: 15.0.into(), ..Border::default() },
        ..button::Style::default()
    });
    let vertical = (REPLY_HEIGHT - BODY_SIZE * LINE) / 2.0;
    let input = text_input(placeholder, &state.reply)
        .on_input(Message::ReplyInput)
        .on_submit(Message::ReplySend)
        .font(theme.font(Weight::Normal))
        .thai_font(theme.thai_font(Weight::Normal))
        .size(BODY_SIZE)
        .line_height(LineHeight::Relative(LINE))
        .padding(Padding { top: vertical, right: 42.0, bottom: vertical, left: 14.0 })
        .placeholder_indent(6.0)
        .width(Length::Fill)
        .style(move |_, status| {
            let focused = matches!(status, text_input::Status::Focused { .. });
            text_input::Style {
                background: Background::Color(dim(colors.container_high, alpha)),
                border: Border {
                    radius: (REPLY_HEIGHT / 2.0).into(),
                    width: 1.0,
                    color: if focused { dim(colors.tint, alpha) } else { Color::TRANSPARENT },
                },
                placeholder: dim(colors.text_muted, alpha),
                value: dim(colors.text_primary, alpha),
                selection: dim(colors.accent, alpha),
                caret: dim(colors.text_primary, alpha),
            }
        });
    container(stack![
        input,
        container(send)
            .align_right(Length::Fill)
            .center_y(REPLY_HEIGHT)
            .padding(Padding { right: 4.0, ..Padding::ZERO }),
    ])
    .padding(Padding { top: 8.0, ..Padding::ZERO })
    .height(REPLY_HEIGHT + 8.0)
    .into()
}

fn round_button<'a>(
    theme: &Theme,
    name: &'a str,
    side: f32,
    icon_size: f32,
    idle: Option<Color>,
    alpha: f32,
    message: Message,
) -> Element<'a, Message> {
    let colors = theme.colors;
    let (rest_ink, hover_ink) = match idle {
        Some(_) => (colors.text_primary, colors.text_primary),
        None => (colors.text_muted, colors.text_primary),
    };
    button(container(icon(name).size(icon_size)).center(side))
        .padding(0)
        .width(side)
        .height(side)
        .on_press(message)
        .style(move |_, status| {
            let hovered = matches!(
                status,
                button::Status::Hovered | button::Status::Pressed
            );
            let background = if hovered { Some(colors.container_highest) } else { idle };
            button::Style {
                background: background.map(|color| Background::Color(dim(color, alpha))),
                text_color: dim(if hovered { hover_ink } else { rest_ink }, alpha),
                border: Border { radius: (side / 2.0).into(), ..Border::default() },
                ..button::Style::default()
            }
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(summary: &str, live: bool) -> Entry {
        Entry {
            uid: String::from("1-1"),
            server_id: 1,
            received_at: 1,
            app_name: String::from("Chat"),
            app_icon: String::new(),
            desktop_entry: String::new(),
            category: String::new(),
            summary: summary.to_owned(),
            body: String::new(),
            image: None,
            urgency: Urgency::Normal,
            actions: vec![
                deskshell_notify::Action {
                    identifier: String::from("default"),
                    label: String::from("Open"),
                },
                deskshell_notify::Action {
                    identifier: String::from("mark"),
                    label: String::from("Mark read"),
                },
            ],
            inline_reply: Some(String::from("Reply…")),
            live,
        }
    }

    #[test]
    fn time_labels() {
        assert_eq!(time_label("", "now"), "now");
        assert_eq!(time_label("Chat", "now"), "Chat · now");
        assert_eq!(time_label("Chat", ""), "Chat");
        assert_eq!(time_label("", ""), "");
    }

    #[test]
    fn the_side_column() {
        let toast = Settings { time_inline: true, primary_action: true, ..Settings::default() };
        assert!(
            toast.side_column(false),
            "the close button always shows in a toast"
        );
        let hub = Settings { close_on_hover: true, ..Settings::default() };
        assert!(!hub.side_column(false));
        assert!(hub.side_column(true));
        assert!(Settings { expanded: true, ..hub }.side_column(false));
    }

    #[test]
    fn messages_become_outcomes() {
        let entry = entry("Hello", true);
        let body = Body::parse(r#"<a href="https://example.org">x</a>"#);
        let mut state = State::default();
        assert_eq!(
            state.update(Message::Action(1), &entry, &body),
            Some(Outcome::Invoke(String::from("mark")))
        );
        assert_eq!(
            state.update(Message::Action(9), &entry, &body),
            None
        );
        assert_eq!(
            state.update(Message::ShowAllActions, &entry, &body),
            None
        );
        assert!(state.show_all_actions);
        state.entry_changed();
        assert!(!state.show_all_actions);
        assert_eq!(
            state.update(
                Message::ReplyInput(String::from(" hi ")),
                &entry,
                &body
            ),
            None
        );
        assert_eq!(
            state.update(Message::ReplySend, &entry, &body),
            Some(Outcome::Reply(String::from(" hi ")))
        );
        assert!(
            state.reply.is_empty(),
            "the field clears after a send"
        );
        assert_eq!(
            state.update(Message::Link(0), &entry, &body),
            Some(Outcome::OpenLink(String::from(
                "https://example.org"
            )))
        );
        assert_eq!(
            state.update(Message::Close, &entry, &body),
            Some(Outcome::Close)
        );
        assert_eq!(
            state.update(Message::ToggleExpand, &entry, &body),
            Some(Outcome::ToggleExpand)
        );
        assert_eq!(
            state.update(Message::Hover(true), &entry, &body),
            None
        );
        assert!(state.hovered);
    }
}
