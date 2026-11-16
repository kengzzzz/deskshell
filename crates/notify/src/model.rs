//! The notification state the UI renders and the commands it sends.

use std::sync::Arc;

use crate::focus::{Rect, Timer};

pub(crate) const FALLBACK_NAME: &str = "Notification";
pub(crate) const REPLY_PLACEHOLDER: &str = "Reply…";

/// Both lists are newest first, and an entry that shows in both is the same `Arc`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    pub history: Vec<Arc<Entry>>,
    pub popups: Vec<Arc<Entry>>,
    pub dnd: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub uid: String,
    pub server_id: u32,
    pub received_at: i64,
    pub app_name: String,
    pub app_icon: String,
    pub desktop_entry: String,
    pub category: String,
    pub summary: String,
    pub body: String,
    pub image: Option<Image>,
    pub urgency: Urgency,
    pub actions: Vec<Action>,
    pub inline_reply: Option<String>,
    pub live: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Image {
    Pixels(Arc<Pixels>),
    Path(String),
}

/// Checked so that every row fits in `data`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    /// Senders may pad rows, so this can exceed `width * channels`.
    pub rowstride: u32,
    pub has_alpha: bool,
    pub channels: u32,
    pub data: Box<[u8]>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Urgency {
    Low,
    #[default]
    Normal,
    Critical,
}

impl Urgency {
    pub fn from_level(level: i64) -> Self {
        match level {
            0 => Self::Low,
            2 => Self::Critical,
            _ => Self::Normal,
        }
    }

    pub fn level(self) -> u8 {
        match self {
            Self::Low => 0,
            Self::Normal => 1,
            Self::Critical => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub identifier: String,
    pub label: String,
}

pub fn action_label(identifier: &str, text: &str) -> String {
    let text = text.trim();
    if !text.is_empty() {
        return text.to_owned();
    }
    match identifier.trim() {
        "default" => "Open".to_owned(),
        "" => "Action".to_owned(),
        identifier => identifier.to_owned(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Dismiss(String),
    Clear,
    ToggleDnd,
    HidePopup(String),
    ClearPopups,
    Invoke(String, String),
    Reply(String, String),
    Urgent { address: u64, class: String },
    FocusTimer { timer: Timer, window: Option<Rect>, monitors: Vec<Rect> },
    Post(Post),
    PostWith(Post, Extra),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extra {
    pub image_path: Option<String>,
    pub resident: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Post {
    pub owner: String,
    pub app_name: String,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<(String, String)>,
    pub urgency: Urgency,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_labels() {
        assert_eq!(action_label("reply", "  Reply  "), "Reply");
        assert_eq!(action_label("default", ""), "Open");
        assert_eq!(action_label("default", "  "), "Open");
        assert_eq!(action_label(" mark-read ", ""), "mark-read");
        assert_eq!(action_label("", ""), "Action");
        assert_eq!(action_label("default", "View"), "View");
    }

    #[test]
    fn urgency_levels() {
        assert_eq!(Urgency::from_level(0), Urgency::Low);
        assert_eq!(Urgency::from_level(1), Urgency::Normal);
        assert_eq!(Urgency::from_level(2), Urgency::Critical);
        assert_eq!(Urgency::from_level(7), Urgency::Normal);
        assert_eq!(Urgency::Critical.level(), 2);
    }
}
