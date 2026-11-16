//! Material Symbols glyphs by ligature name.

use iced::alignment::{Horizontal, Vertical};
use iced::widget::text::{LineHeight, Shaping};
use iced::widget::{Text, text};
use iced::{Color, Element};

use crate::fonts;

pub const DEFAULT_SIZE: f32 = 20.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Icon<'a> {
    name: &'a str,
    size: f32,
    color: Option<Color>,
    filled: bool,
}

pub fn icon(name: &str) -> Icon<'_> {
    Icon { name, size: DEFAULT_SIZE, color: None, filled: false }
}

impl<'a> Icon<'a> {
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    pub fn fill(mut self, filled: bool) -> Self {
        self.filled = filled;
        self
    }

    pub fn into_text(self) -> Text<'a> {
        let font = if self.filled { fonts::SYMBOLS_FILL1 } else { fonts::SYMBOLS_FILL0 };
        text(self.name)
            .font(font)
            .shaping(Shaping::Advanced)
            .size(self.size)
            .line_height(LineHeight::Absolute(self.size.into()))
            .width(self.size)
            .height(self.size)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Center)
            .color_maybe(self.color)
    }
}

impl<'a, Message: 'a> From<Icon<'a>> for Element<'a, Message> {
    fn from(icon: Icon<'a>) -> Self {
        icon.into_text().into()
    }
}
