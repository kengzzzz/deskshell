//! The media card.

use std::time::Duration;

use deskshell_media::mpris::{Player, Players};
use deskshell_ui::text::{self, Tone};
use deskshell_ui::theme::dim;
use deskshell_ui::{Picture, Theme, clip_x, icon, scroll};
use iced::font::Weight;
use iced::widget::text::{Rich, Wrapping};
use iced::widget::{
    Column, Row, Space, Text, button, column, container, image, row, slider, tooltip,
};
use iced::{Alignment, Background, Border, Color, ContentFit, Element, Font, Length, padding};

use super::{Focus, Media, Message, PADDING};
use crate::panel::{self, Env};
use crate::toasts::measure::Measure;

const PREVIOUS: &str = "\u{f048}";
const PLAY: &str = "\u{f04b}";
const PAUSE: &str = "\u{f04c}";
const NEXT: &str = "\u{f051}";
const NOTE: &str = "\u{f001}";
const LARGE_ART: f32 = 96.0;
const SMALL_ART: f32 = 34.0;
const ROW_HEIGHT: f32 = 58.0;
const TITLE_HEIGHT: f32 = 3.0 * 17.0 * 1.3;
const SLIDER: f32 = 12.0;
const DISABLED: f32 = 0.4;
const TOOLTIP_DELAY: Duration = Duration::from_millis(600);

fn msg(message: Message) -> panel::Message {
    panel::Message::Media(message)
}

impl Media {
    pub fn view<'a>(&'a self, env: &Env<'a>, players: &'a Players) -> Element<'a, panel::Message> {
        let theme = env.theme;
        let colors = &theme.colors;
        let player = players.active();
        let several = players.list.len() > 1;

        let mut content = Column::with_capacity(5)
            .spacing(16)
            .width(Length::Fill)
            .push(self.header(theme, player, several));
        content = match player.filter(|player| player.has_progress()) {
            // The slider's track sits on the top edge of the progress column, so the gap above
            // it shrinks by half the slider's height.
            Some(progress) => content.push(
                column![self.now_playing(theme, player), self.progress(theme, progress)]
                    .spacing(16.0 - SLIDER / 2.0),
            ),
            None => content.push(self.now_playing(theme, player)),
        };
        content = content.push(self.controls(theme, player));
        if several {
            content = content.push(self.players(theme, players));
        }

        let background = colors.glass;
        let border = colors.container_high;
        container(
            scroll(Measure::new(content, |height| {
                msg(Message::Measured(height))
            }))
            .width(Length::Fill),
        )
        .padding(PADDING)
        .width(Length::Fill)
        .style(move |_| container::Style {
            background: Some(Background::Color(background)),
            border: Border { color: border, width: 1.0, radius: 28.0.into() },
            ..container::Style::default()
        })
        .into()
    }

    fn header<'a>(
        &'a self,
        theme: &'a Theme,
        player: Option<&Player>,
        several: bool,
    ) -> Element<'a, panel::Message> {
        let colors = &theme.colors;
        let mut header = Row::with_capacity(3)
            .spacing(6)
            .height(26)
            .align_y(Alignment::Center)
            .push(
                Text::new("Now Playing")
                    .font(theme.font(Weight::Semibold))
                    .size(17)
                    .color(colors.text_primary)
                    .width(Length::Fill),
            );
        if player.is_some_and(Player::is_live_stream) {
            header = header.push(chip(
                Text::new("LIVE")
                    .font(theme.font(Weight::Bold))
                    .size(11)
                    .color(colors.danger),
                9.0,
                26.0,
                colors.danger_container,
            ));
        }
        if several {
            header = header.push(chip(
                row![
                    icon("queue_music")
                        .size(16.0)
                        .color(colors.text_secondary),
                    Text::new(self.count_text.as_str())
                        .font(theme.font(Weight::Semibold))
                        .size(12)
                        .color(colors.text_secondary),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
                10.0,
                26.0,
                colors.container_high,
            ));
        }
        header.into()
    }

    fn now_playing<'a>(
        &'a self,
        theme: &'a Theme,
        player: Option<&'a Player>,
    ) -> Element<'a, panel::Message> {
        let colors = &theme.colors;
        let title = player.map_or("Media", Player::title);
        let mut texts = Column::with_capacity(3)
            .spacing(4)
            .width(Length::Fill)
            .push(
                clip_x(
                    container(text::mixed(theme, title, Tone::Primary, Weight::Semibold).size(17))
                        .max_height(TITLE_HEIGHT),
                )
                .keep_bottom(),
            );
        if let Some(player) = player.filter(|player| !player.track.artist.is_empty()) {
            texts = texts.push(elided(
                text::mixed(
                    theme,
                    &player.track.artist,
                    Tone::Secondary,
                    Weight::Normal,
                )
                .size(14),
            ));
        }
        if let Some(player) = player.filter(|player| !player.identity.is_empty()) {
            texts = texts.push(
                container(chip(
                    text::mixed(
                        theme,
                        &player.identity,
                        Tone::Secondary,
                        Weight::Normal,
                    )
                    .size(12)
                    .wrapping(Wrapping::None),
                    10.0,
                    24.0,
                    colors.container_high,
                ))
                .padding(padding::top(4)),
            );
        }
        // The art sits at the top and the text column in the middle of the row, which is as
        // high as the taller of the two.
        row![
            self.artwork(theme, player, LARGE_ART),
            row![Space::new().height(LARGE_ART), texts]
                .align_y(Alignment::Center)
                .width(Length::Fill),
        ]
        .spacing(16)
        .into()
    }

    fn artwork<'a>(
        &'a self,
        theme: &Theme,
        player: Option<&'a Player>,
        side: f32,
    ) -> Element<'a, panel::Message> {
        let colors = &theme.colors;
        let large = side == LARGE_ART;
        let radius = if large { 20.0 } else { 10.0 };
        let art = player
            .map(|player| player.track.art_url.as_str())
            .filter(|url| !url.is_empty())
            .and_then(|url| self.art.ready(url));
        let content: Element<'a, panel::Message> = match art {
            Some(art) => {
                let handle = if large { &art.large.handle } else { &art.small.handle };
                Picture::new(
                    handle,
                    image(handle)
                        .content_fit(ContentFit::Cover)
                        .width(side)
                        .height(side),
                )
                .into()
            }
            None => Text::new(NOTE)
                .font(Font::with_name(theme.fonts.mono))
                .size((side * 0.35).round().max(12.0))
                .color(colors.accent)
                .into(),
        };
        let background = colors.surface_active;
        container(content)
            .center(side)
            .style(move |_| container::Style {
                background: Some(Background::Color(background)),
                border: Border { radius: radius.into(), ..Border::default() },
                ..container::Style::default()
            })
            .into()
    }

    fn progress<'a>(&'a self, theme: &'a Theme, player: &Player) -> Element<'a, panel::Message> {
        let colors = &theme.colors;
        let length = player
            .length()
            .unwrap_or_default()
            .as_secs_f64();
        let (accent, rail) = (colors.accent, colors.border_muted);
        let bar = slider(
            0.0..=length,
            self.shown_seconds(player),
            |seconds| msg(Message::Seek(seconds)),
        )
        .on_release(msg(Message::SeekDone))
        .step(0.001)
        .height(SLIDER)
        .style(move |_, _| slider::Style {
            rail: slider::Rail {
                backgrounds: (Background::Color(accent), Background::Color(rail)),
                width: 4.0,
                border: Border { radius: 2.0.into(), ..Border::default() },
            },
            handle: slider::Handle {
                shape: slider::HandleShape::Circle { radius: 6.0 },
                background: Background::Color(accent),
                border_width: 0.0,
                border_color: Color::TRANSPARENT,
            },
        });
        let time = |value: &'a str| {
            Text::new(value)
                .font(theme.font(Weight::Normal))
                .size(12)
                .color(colors.text_secondary)
        };
        column![
            container(bar).padding([0, 6]),
            row![time(&self.position).width(Length::Fill), time(&self.length)],
        ]
        .spacing(2.0 - SLIDER / 2.0)
        .into()
    }

    fn controls<'a>(
        &'a self,
        theme: &'a Theme,
        player: Option<&Player>,
    ) -> Element<'a, panel::Message> {
        let playing = player.is_some_and(Player::is_playing);
        let press = |allowed: fn(&Player) -> bool, message: Message| {
            player
                .is_some_and(allowed)
                .then_some(msg(message))
        };
        let controls = row![
            self.control(
                theme,
                Control {
                    glyph: PREVIOUS,
                    label: "Previous",
                    primary: false,
                    focus: Focus::Previous
                },
                press(Player::can_go_previous, Message::Previous),
            ),
            self.control(
                theme,
                Control {
                    glyph: if playing { PAUSE } else { PLAY },
                    label: if playing { "Pause" } else { "Play" },
                    primary: true,
                    focus: Focus::PlayPause,
                },
                press(Player::can_toggle_playing, Message::PlayPause),
            ),
            self.control(
                theme,
                Control { glyph: NEXT, label: "Next", primary: false, focus: Focus::Next },
                press(Player::can_go_next, Message::Next),
            ),
        ]
        .spacing(20)
        .align_y(Alignment::Center);
        container(controls)
            .center_x(Length::Fill)
            .into()
    }

    fn control<'a>(
        &self,
        theme: &'a Theme,
        control: Control,
        on_press: Option<panel::Message>,
    ) -> Element<'a, panel::Message> {
        let colors = &theme.colors;
        let Control { glyph, label, primary, focus } = control;
        let side = if primary { 64.0 } else { 48.0 };
        let (base, hovered, ink) = if primary {
            (colors.accent, colors.accent, colors.accent_text)
        } else {
            (
                colors.container_high,
                colors.container_highest,
                colors.text_primary,
            )
        };
        let enabled = on_press.is_some();
        let ink = if enabled { ink } else { dim(ink, DISABLED) };
        let ring = if self.focus == Some(focus) { 1.0 } else { 0.0 };
        let ring_color = colors.accent;
        let face = button(
            container(
                Text::new(glyph)
                    .font(Font::with_name(theme.fonts.mono))
                    .size(if primary { 22 } else { 18 })
                    .color(ink),
            )
            .center(Length::Fill),
        )
        .width(side)
        .height(side)
        .padding(0)
        .on_press_maybe(on_press)
        .style(move |_, status| button::Style {
            background: Some(Background::Color(match status {
                button::Status::Hovered | button::Status::Pressed => hovered,
                button::Status::Active => base,
                button::Status::Disabled => dim(base, DISABLED),
            })),
            text_color: ink,
            border: Border { color: ring_color, width: ring, radius: (side / 2.0).into() },
            ..button::Style::default()
        });
        if !enabled {
            return face.into();
        }
        let (surface, edge) = (colors.solid, colors.border);
        tooltip(
            face,
            container(
                Text::new(label)
                    .font(theme.font(Weight::Normal))
                    .size(12)
                    .color(colors.text_primary),
            )
            .padding([4, 8])
            .style(move |_| container::Style {
                background: Some(Background::Color(surface)),
                border: Border { color: edge, width: 1.0, radius: 6.0.into() },
                ..container::Style::default()
            }),
            tooltip::Position::Top,
        )
        .gap(3)
        .delay(TOOLTIP_DELAY)
        .into()
    }

    fn players<'a>(
        &'a self,
        theme: &'a Theme,
        players: &'a Players,
    ) -> Element<'a, panel::Message> {
        let label = container(
            Text::new("Players")
                .font(theme.font(Weight::Semibold))
                .size(12)
                .color(theme.colors.text_muted),
        )
        .padding(padding::left(4));
        players
            .list
            .iter()
            .enumerate()
            .fold(
                Column::with_capacity(players.list.len() + 1)
                    .spacing(8)
                    .width(Length::Fill)
                    .push(label),
                |list, (index, player)| {
                    list.push(self.player_row(
                        theme,
                        index,
                        player,
                        players.active == Some(index),
                    ))
                },
            )
            .into()
    }

    fn player_row<'a>(
        &'a self,
        theme: &'a Theme,
        index: usize,
        player: &'a Player,
        selected: bool,
    ) -> Element<'a, panel::Message> {
        let colors = &theme.colors;
        let playing = player.is_playing();
        let texts = column![
            elided(
                text::mixed(
                    theme,
                    player.title(),
                    Tone::Primary,
                    Weight::Normal
                )
                .size(13)
            ),
            elided(
                text::mixed(
                    theme,
                    &player.identity,
                    Tone::Muted,
                    Weight::Normal
                )
                .size(12)
            ),
        ]
        .spacing(4)
        .width(Length::Fill);
        let toggle = self.control(
            theme,
            Control {
                glyph: if playing { PAUSE } else { PLAY },
                label: if playing { "Pause this player" } else { "Play this player" },
                primary: false,
                focus: Focus::RowButton(index),
            },
            player
                .can_toggle_playing()
                .then_some(msg(Message::Toggle(index))),
        );
        let content = row![self.artwork(theme, Some(player), SMALL_ART), texts, toggle]
            .spacing(10)
            .height(Length::Fill)
            .align_y(Alignment::Center);
        let (high, highest, base) = (
            colors.container_high,
            colors.container_highest,
            colors.container,
        );
        button(content)
            .padding([0, 10])
            .width(Length::Fill)
            .height(ROW_HEIGHT)
            .on_press(msg(Message::Select(index)))
            .style(move |_, status| button::Style {
                background: Some(Background::Color(match status {
                    _ if selected => highest,
                    button::Status::Hovered | button::Status::Pressed => high,
                    button::Status::Active | button::Status::Disabled => base,
                })),
                border: Border { radius: 18.0.into(), ..Border::default() },
                ..button::Style::default()
            })
            .into()
    }
}

#[derive(Debug, Clone, Copy)]
struct Control {
    glyph: &'static str,
    label: &'static str,
    primary: bool,
    focus: Focus,
}

fn chip<'a>(
    content: impl Into<Element<'a, panel::Message>>,
    padding: f32,
    height: f32,
    background: Color,
) -> Element<'a, panel::Message> {
    container(content)
        .padding([0.0, padding])
        .center_y(height)
        .style(move |_| container::Style {
            background: Some(Background::Color(background)),
            border: Border { radius: (height / 2.0).into(), ..Border::default() },
            ..container::Style::default()
        })
        .into()
}

// iced has no ellipsis elide, so this only clips.
fn elided<'a>(content: Rich<'a, (), panel::Message>) -> Element<'a, panel::Message> {
    clip_x(container(content.wrapping(Wrapping::None)).width(Length::Fill)).into()
}
