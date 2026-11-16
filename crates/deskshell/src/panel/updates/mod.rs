//! The updates popover: the install step, progress, step chips, and Log, Stop and Hide.

pub mod progress;
mod view;

use std::sync::LazyLock;

use super::{Effect, Env};
use crate::updates::{self, Request, View};

pub const WIDTH: u32 = 580;
pub const HEIGHT: u32 = 206;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    Log,
    Stop,
    Hide,
    Redraw,
}

static IDLE: LazyLock<View> = LazyLock::new(View::default);

pub fn view_of<'a>(env: &Env<'a>) -> &'a View {
    match env.updates {
        Some(view) => view,
        None => &IDLE,
    }
}

#[derive(Debug, Default)]
pub struct Updates;

impl Updates {
    pub fn update(&mut self, message: Message, env: &Env<'_>, effects: &mut Vec<Effect>) {
        match message {
            Message::Log => effects
                .push(Effect::Spawn { argv: updates::log_argv(&updates::log_path()), cwd: None }),
            Message::Stop => {
                if view_of(env).can_stop {
                    effects.push(Effect::Updates(Request::Cancel));
                }
            }
            Message::Hide => effects.push(Effect::Close),
            Message::Redraw => {}
        }
    }

    pub fn view<'a>(&'a self, env: &Env<'a>) -> iced::Element<'a, Message> {
        view::view(env.theme, view_of(env))
    }
}
