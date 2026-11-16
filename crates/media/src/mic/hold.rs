//! The hold time and the meter rule for each hardware recording.

use std::collections::HashSet;
use std::time::Duration;

use tokio::time::Instant;

use super::model::MicUser;
use super::rules::{Readers, Recording};

#[derive(Debug)]
pub(crate) struct Holds {
    hold: Duration,
    entries: Vec<Entry>,
}

#[derive(Debug)]
struct Entry {
    output: u32,
    started: Instant,
    last: Readers,
}

impl Entry {
    fn hidden(&self) -> bool {
        self.last == Readers::Meters
    }
}

impl Holds {
    pub fn new(hold: Duration) -> Self {
        Self { hold, entries: Vec::new() }
    }

    pub fn note(&mut self, now: Instant, kinds: &[(u32, Readers)]) {
        self.track(now, kinds.iter().copied());
    }

    fn track(&mut self, now: Instant, live: impl Iterator<Item = (u32, Readers)> + Clone) {
        self.entries.retain(|e| {
            live.clone()
                .any(|(output, _)| output == e.output)
        });
        for (output, readers) in live {
            let entry = match self
                .entries
                .iter()
                .position(|e| e.output == output)
            {
                Some(i) => &mut self.entries[i],
                None => {
                    self.entries
                        .push_mut(Entry { output, started: now, last: Readers::Nobody })
                }
            };
            if readers != Readers::Nobody {
                entry.last = readers;
            }
        }
    }

    pub fn update(
        &mut self,
        now: Instant,
        live: &[Recording],
        users: &mut Vec<MicUser>,
    ) -> Option<Instant> {
        self.track(
            now,
            live.iter()
                .map(|r| (r.output, r.readers)),
        );
        users.clear();
        let mut unique = HashSet::new();
        let mut next: Option<Instant> = None;
        for entry in &self.entries {
            let due = entry.started + self.hold;
            if due > now {
                next = Some(next.map_or(due, |n| n.min(due)));
                continue;
            }
            if entry.hidden() {
                continue;
            }
            if let Some(recording) = live
                .iter()
                .find(|r| r.output == entry.output)
            {
                for user in &recording.users {
                    if unique.insert(user) {
                        users.push(user.clone());
                    }
                }
            }
        }
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOLD: Duration = Duration::from_secs(1);

    fn user(label: &str) -> MicUser {
        MicUser { pid: None, label: Some(label.to_owned()) }
    }

    fn rec(output: u32, labels: &[&str]) -> Recording {
        Recording {
            output,
            users: labels.iter().map(|l| user(l)).collect(),
            readers: Readers::Nobody,
        }
    }

    fn labels(users: &[MicUser]) -> Vec<&str> {
        users
            .iter()
            .filter_map(|u| u.label.as_deref())
            .collect()
    }

    #[test]
    fn a_probe_shorter_than_the_hold_never_shows() {
        let t0 = Instant::now();
        let mut holds = Holds::new(HOLD);
        let mut users = Vec::new();
        let next = holds.update(t0, &[rec(1, &["Browser"])], &mut users);
        assert!(users.is_empty());
        assert_eq!(next, Some(t0 + HOLD));
        let next = holds.update(t0 + Duration::from_millis(400), &[], &mut users);
        assert!(users.is_empty());
        assert_eq!(next, None);
        let next = holds.update(
            t0 + Duration::from_millis(500),
            &[rec(1, &["Browser"])],
            &mut users,
        );
        assert!(
            users.is_empty(),
            "a new probe starts the hold again"
        );
        assert_eq!(next, Some(t0 + Duration::from_millis(1500)));
    }

    #[test]
    fn shows_after_the_hold_and_hides_at_once() {
        let t0 = Instant::now();
        let mut holds = Holds::new(HOLD);
        let mut users = Vec::new();
        holds.update(t0, &[rec(1, &["Recorder"])], &mut users);
        let next = holds.update(t0 + HOLD, &[rec(1, &["Recorder"])], &mut users);
        assert_eq!(labels(&users), ["Recorder"]);
        assert_eq!(next, None);
        holds.update(t0 + HOLD * 5, &[], &mut users);
        assert!(users.is_empty());
    }

    #[test]
    fn keeps_the_order_in_which_uses_began() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(300);
        let mut holds = Holds::new(HOLD);
        let mut users = Vec::new();
        holds.update(t0, &[rec(5, &["Second"])], &mut users);
        let next = holds.update(
            t1,
            &[rec(2, &["Third"]), rec(5, &["Second"])],
            &mut users,
        );
        assert_eq!(next, Some(t0 + HOLD));
        holds.update(
            t0 + HOLD,
            &[rec(2, &["Third"]), rec(5, &["Second"])],
            &mut users,
        );
        assert_eq!(labels(&users), ["Second"]);
        holds.update(
            t1 + HOLD,
            &[rec(2, &["Third", "Second"]), rec(5, &["Second"])],
            &mut users,
        );
        assert_eq!(labels(&users), ["Second", "Third"]);
    }

    #[test]
    fn a_feed_that_changes_its_users_keeps_its_time() {
        let t0 = Instant::now();
        let mut holds = Holds::new(HOLD);
        let mut users = Vec::new();
        holds.update(t0, &[rec(1, &["Reader"])], &mut users);
        holds.update(t0 + HOLD, &[rec(1, &["Reader"])], &mut users);
        assert_eq!(labels(&users), ["Reader"]);
        let next = holds.update(t0 + HOLD * 3, &[rec(1, &["Creator"])], &mut users);
        assert_eq!(labels(&users), ["Creator"]);
        assert_eq!(next, None);
    }

    #[test]
    fn one_entry_per_user_across_recordings() {
        let t0 = Instant::now();
        let mut holds = Holds::new(HOLD);
        let mut users = Vec::new();
        let live = [rec(1, &["App"]), rec(2, &["App", "Other"])];
        holds.update(t0, &live, &mut users);
        holds.update(t0 + HOLD, &live, &mut users);
        assert_eq!(labels(&users), ["App", "Other"]);
    }

    fn feed(output: u32, labels: &[&str], readers: Readers) -> Recording {
        Recording { readers, ..rec(output, labels) }
    }

    fn run(steps: &[Recording]) -> Vec<Vec<String>> {
        let t0 = Instant::now();
        let mut holds = Holds::new(HOLD);
        let mut users = Vec::new();
        holds.update(t0, &steps[..1], &mut users);
        let mut shown = Vec::new();
        for (i, step) in steps.iter().enumerate() {
            let now = t0 + HOLD * (i as u32 + 1);
            holds.update(now, std::slice::from_ref(step), &mut users);
            shown.push(
                labels(&users)
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            );
        }
        shown
    }

    #[test]
    fn a_meter_alone_on_the_virtual_mic_shows_nothing() {
        let shown = run(&[feed(1, &["Producer"], Readers::Meters)]);
        assert_eq!(shown, [Vec::<String>::new()]);
    }

    #[test]
    fn a_meter_and_an_app_name_the_app() {
        let shown =
            run(&[feed(1, &["Producer"], Readers::Meters), feed(1, &["Recorder"], Readers::Apps)]);
        assert_eq!(shown, [vec![], vec!["Recorder"]]);
    }

    #[test]
    fn the_tail_after_a_meter_shows_nothing() {
        let shown = run(&[
            feed(1, &["Producer"], Readers::Meters),
            feed(1, &["Producer"], Readers::Nobody),
            feed(1, &["Producer"], Readers::Nobody),
        ]);
        assert_eq!(shown, [Vec::<String>::new(), vec![], vec![]]);
    }

    #[test]
    fn an_app_that_leaves_while_a_meter_stays_hides_the_feed() {
        let shown = run(&[
            feed(1, &["Recorder"], Readers::Apps),
            feed(1, &["Producer"], Readers::Meters),
            feed(1, &["Producer"], Readers::Nobody),
        ]);
        assert_eq!(shown, [vec!["Recorder"], vec![], vec![]]);
    }

    #[test]
    fn the_tail_after_an_app_shows_the_feed() {
        let shown =
            run(&[feed(1, &["Recorder"], Readers::Apps), feed(1, &["Producer"], Readers::Nobody)]);
        assert_eq!(shown, [vec!["Recorder"], vec!["Producer"]]);
    }

    #[test]
    fn a_producer_that_never_had_readers_shows() {
        let shown = run(&[
            feed(1, &["Producer"], Readers::Nobody),
            feed(1, &["Producer"], Readers::Nobody),
        ]);
        assert_eq!(shown, [vec!["Producer"], vec!["Producer"]]);
    }

    #[test]
    fn the_memory_goes_with_the_recording() {
        let t0 = Instant::now();
        let mut holds = Holds::new(HOLD);
        let mut users = Vec::new();
        holds.update(
            t0,
            &[feed(1, &["Producer"], Readers::Meters)],
            &mut users,
        );
        holds.update(t0 + HOLD, &[], &mut users);
        assert!(holds.entries.is_empty());
        let again = [feed(1, &["Producer"], Readers::Nobody)];
        holds.update(t0 + HOLD * 2, &again, &mut users);
        holds.update(t0 + HOLD * 3, &again, &mut users);
        assert_eq!(labels(&users), ["Producer"]);
    }
}
