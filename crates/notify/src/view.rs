//! Pure helpers for the notification UI.

use std::sync::Arc;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::model::{Entry, FALLBACK_NAME, Urgency};

pub fn relative_time(now: i64, then: i64, tz: &TimeZone) -> String {
    let minutes = now
        .saturating_sub(then)
        .div_euclid(60_000);
    if minutes < 1 {
        return "now".to_owned();
    }
    if minutes < 60 {
        return format!("{minutes}m");
    }
    if minutes < 24 * 60 {
        return format!("{}h", minutes / 60);
    }
    let local = |ms: i64| tz.to_datetime(Timestamp::from_millisecond(ms).unwrap_or_default());
    let then = local(then);
    let format = if then.year() == local(now).year() { "%b %-d" } else { "%b %-d %Y" };
    then.strftime(format).to_string()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Group<'a> {
    pub key: &'a str,
    pub app_icon: &'a str,
    pub desktop_entry: &'a str,
    pub critical: bool,
    pub entries: Vec<&'a Arc<Entry>>,
}

pub fn group_by_app(history: &[Arc<Entry>]) -> Vec<Group<'_>> {
    let mut groups: Vec<Group<'_>> = Vec::new();
    for entry in history {
        let key = if entry.app_name.is_empty() { FALLBACK_NAME } else { entry.app_name.as_str() };
        let index = match groups
            .iter()
            .position(|group| group.key == key)
        {
            Some(index) => index,
            None => {
                groups.push(Group {
                    key,
                    app_icon: "",
                    desktop_entry: "",
                    critical: false,
                    entries: Vec::new(),
                });
                groups.len() - 1
            }
        };
        let group = &mut groups[index];
        group.entries.push(entry);
        if group.app_icon.is_empty() {
            group.app_icon = &entry.app_icon;
        }
        if group.desktop_entry.is_empty() {
            group.desktop_entry = &entry.desktop_entry;
        }
        group.critical |= entry.urgency == Urgency::Critical;
    }
    groups
}

#[cfg(test)]
mod tests {
    use jiff::tz::Offset;

    use super::*;

    const MINUTE: i64 = 60_000;

    fn bangkok() -> TimeZone {
        TimeZone::fixed(Offset::constant(7))
    }

    fn ms(timestamp: &str) -> i64 {
        timestamp
            .parse::<Timestamp>()
            .unwrap()
            .as_millisecond()
    }

    #[test]
    fn minutes_and_hours() {
        let tz = bangkok();
        let now = ms("2026-11-12T10:00:00Z");
        assert_eq!(relative_time(now, now, &tz), "now");
        assert_eq!(relative_time(now, now - MINUTE + 1, &tz), "now");
        assert_eq!(relative_time(now, now - MINUTE, &tz), "1m");
        assert_eq!(
            relative_time(now, now - 59 * MINUTE - 59_999, &tz),
            "59m"
        );
        assert_eq!(relative_time(now, now - 60 * MINUTE, &tz), "1h");
        assert_eq!(
            relative_time(now, now - 1439 * MINUTE, &tz),
            "23h"
        );
    }

    #[test]
    fn the_future_is_now() {
        let now = ms("2026-11-12T10:00:00Z");
        assert_eq!(
            relative_time(now, now + 5 * MINUTE, &bangkok()),
            "now"
        );
    }

    #[test]
    fn dates_in_local_time() {
        let tz = bangkok();
        let now = ms("2026-11-12T10:00:00Z");
        assert_eq!(
            relative_time(now, now - 1440 * MINUTE, &tz),
            "Nov 11"
        );
        assert_eq!(
            relative_time(now, ms("2026-03-05T16:59:00Z"), &tz),
            "Mar 5"
        );
        assert_eq!(
            relative_time(now, ms("2026-03-05T17:00:00Z"), &tz),
            "Mar 6"
        );
        assert_eq!(
            relative_time(now, ms("2025-07-04T03:00:00Z"), &tz),
            "Jul 4 2025"
        );
    }

    #[test]
    fn the_year_follows_the_zone() {
        // 2026-12-31T18:00Z is already 2027 in UTC+7. A day earlier is still 2026 there.
        let tz = bangkok();
        let now = ms("2026-12-31T18:00:00Z");
        assert_eq!(
            relative_time(now, ms("2026-12-30T17:30:00Z"), &tz),
            "Dec 31 2026"
        );
        assert_eq!(
            relative_time(now, ms("2026-12-30T17:30:00Z"), &TimeZone::UTC),
            "Dec 30"
        );
    }

    fn entry(app_name: &str, app_icon: &str, desktop_entry: &str, urgency: Urgency) -> Arc<Entry> {
        Arc::new(Entry {
            uid: format!("{app_name}-{app_icon}-{desktop_entry}"),
            server_id: 1,
            received_at: 0,
            app_name: app_name.to_owned(),
            app_icon: app_icon.to_owned(),
            desktop_entry: desktop_entry.to_owned(),
            category: String::new(),
            summary: String::new(),
            body: String::new(),
            image: None,
            urgency,
            actions: Vec::new(),
            inline_reply: None,
            live: false,
        })
    }

    #[test]
    fn groups_by_first_appearance() {
        let history = [
            entry("Chat", "", "", Urgency::Normal),
            entry("Mail", "mail", "org.example.Mail", Urgency::Low),
            entry("Chat", "chat", "", Urgency::Critical),
            entry(
                "Chat",
                "other",
                "org.example.Chat",
                Urgency::Normal,
            ),
            entry("", "", "", Urgency::Normal),
        ];
        let groups = group_by_app(&history);
        let keys: Vec<_> = groups
            .iter()
            .map(|group| group.key)
            .collect();
        assert_eq!(keys, ["Chat", "Mail", "Notification"]);

        let chat = &groups[0];
        assert_eq!(chat.app_icon, "chat");
        assert_eq!(chat.desktop_entry, "org.example.Chat");
        assert!(chat.critical);
        assert_eq!(chat.entries.len(), 3);
        assert!(Arc::ptr_eq(chat.entries[1], &history[2]));

        assert!(!groups[1].critical);
        assert_eq!(groups[1].desktop_entry, "org.example.Mail");
        assert!(group_by_app(&[]).is_empty());
    }
}
