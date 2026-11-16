//! A search field pill with an icon, a mode chip, the input and an Esc chip.

use iced::font::Weight;
use iced::keyboard;
use iced::widget::text::Wrapping;
use iced::widget::{Row, container, text};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding};

use crate::text_input::{self, OnKey, TextInput};
use crate::{Theme, icon, key_chip};

pub const HEIGHT: f32 = 52.0;
const ICON: f32 = 22.0;
const TOKEN_HEIGHT: f32 = 28.0;
const TOKEN_TEXT: f32 = 12.0;
const INPUT_TEXT: f32 = 15.0;
const PLACEHOLDER_INDENT: f32 = 6.0;

pub struct SearchField<'a, Message> {
    theme: &'a Theme,
    value: &'a str,
    placeholder: &'a str,
    token: &'a str,
    esc_hint: bool,
    height: f32,
    id: Option<iced::widget::Id>,
    on_input: Option<Box<dyn Fn(String) -> Message + 'a>>,
    on_key: Option<OnKey<'a, Message>>,
}

pub fn search_field<'a, Message>(theme: &'a Theme, value: &'a str) -> SearchField<'a, Message> {
    SearchField {
        theme,
        value,
        placeholder: "",
        token: "",
        esc_hint: true,
        height: HEIGHT,
        id: None,
        on_input: None,
        on_key: None,
    }
}

impl<'a, Message: Clone + 'a> SearchField<'a, Message> {
    pub fn placeholder(mut self, placeholder: &'a str) -> Self {
        self.placeholder = placeholder;
        self
    }

    pub fn token(mut self, token: &'a str) -> Self {
        self.token = token;
        self
    }

    pub fn esc_hint(mut self, shown: bool) -> Self {
        self.esc_hint = shown;
        self
    }

    pub fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    pub fn id(mut self, id: impl Into<iced::widget::Id>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn on_input(mut self, on_input: impl Fn(String) -> Message + 'a) -> Self {
        self.on_input = Some(Box::new(on_input));
        self
    }

    /// Sees each key press before the input edits. A returned message takes the key.
    pub fn on_key(
        mut self,
        on_key: impl Fn(&keyboard::Key, keyboard::Modifiers) -> Option<Message> + 'a,
    ) -> Self {
        self.on_key = Some(Box::new(on_key));
        self
    }
}

impl<'a, Message: Clone + 'a> From<SearchField<'a, Message>> for Element<'a, Message> {
    fn from(field: SearchField<'a, Message>) -> Self {
        let SearchField {
            theme,
            value,
            placeholder,
            token,
            esc_hint,
            height,
            id,
            on_input,
            on_key,
        } = field;
        let colors = theme.colors;
        let mut row = Row::with_capacity(4)
            .spacing(10)
            .align_y(Alignment::Center)
            .push(
                icon("search")
                    .size(ICON)
                    .color(colors.text_muted),
            );
        if !token.is_empty() {
            let background = colors.accent_container;
            row = row.push(
                container(
                    text(token)
                        .font(theme.font(Weight::Semibold))
                        .size(TOKEN_TEXT)
                        .color(colors.accent)
                        .wrapping(Wrapping::None),
                )
                .padding([0, 10])
                .height(TOKEN_HEIGHT)
                .center_y(TOKEN_HEIGHT)
                .style(move |_| container::Style {
                    background: Some(Background::Color(background)),
                    border: Border { radius: (TOKEN_HEIGHT / 2.0).into(), ..Border::default() },
                    ..container::Style::default()
                }),
            );
        }
        let mut input: TextInput<'a, Message> = TextInput::new(placeholder, value)
            .keep_focus(true)
            .font(theme.font(Weight::Normal))
            .thai_font(theme.thai_font(Weight::Normal))
            .size(INPUT_TEXT)
            .placeholder_indent(PLACEHOLDER_INDENT)
            .width(Length::Fill)
            .style(move |_, _| text_input::Style {
                background: Background::Color(Color::TRANSPARENT),
                border: Border::default(),
                placeholder: colors.text_muted,
                value: colors.text_primary,
                selection: colors.accent,
                caret: colors.tint,
            });
        if let Some(id) = id {
            input = input.id(id);
        }
        if let Some(on_input) = on_input {
            input = input.on_input(on_input);
        }
        if let Some(on_key) = on_key {
            input = input.on_key(on_key);
        }
        row = row.push(input);
        if esc_hint {
            row = row.push(key_chip(theme, "Esc", false));
        }
        let background = colors.container_high;
        container(row)
            .padding(Padding { top: 0.0, right: 12.0, bottom: 0.0, left: 16.0 })
            .height(height)
            .center_y(height)
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(background)),
                border: Border { radius: (height / 2.0).into(), ..Border::default() },
                ..container::Style::default()
            })
            .into()
    }
}
