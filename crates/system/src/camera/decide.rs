//! The camera service's rules as pure functions. Nothing here touches the system.

use std::time::{Duration, Instant};

use super::CameraUser;

/// `rdev` is set only for a character device, so a regular file that stands in for a node in
/// tests never matches another by device number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileId {
    pub dev: (u32, u32),
    pub ino: u64,
    pub rdev: Option<(u32, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Last {
    Open { wake: u64 },
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Node {
    pub name: String,
    /// `None` while adding the watch fails, until the next `IN_ATTRIB` for the name.
    pub wd: Option<i32>,
    pub id: Option<FileId>,
    pub hardware: bool,
    /// Deleted from the dev root. The entry stays until `IN_IGNORED`, so the late close of a
    /// held node is recognized.
    pub gone: bool,
    pub last: Option<Last>,
    pub held_since: Option<Instant>,
    pub users: Vec<(CameraUser, Instant)>,
}

impl Node {
    pub fn new(name: &str, hardware: bool) -> Self {
        Self {
            name: name.to_owned(),
            wd: None,
            id: None,
            hardware,
            gone: false,
            last: None,
            held_since: None,
            users: Vec::new(),
        }
    }

    fn virtual_node(&self) -> bool {
        !self.hardware && !self.gone
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Hold {
    pub node: usize,
    pub pid: u32,
    pub pipewire: bool,
    pub fds: u32,
    pub grew: u64,
}

impl Hold {
    fn user(&self) -> CameraUser {
        if self.pipewire { CameraUser::PipeWire } else { CameraUser::Process(self.pid) }
    }
}

pub(crate) fn uses(nodes: &[Node], holds: &[Hold], out: &mut Vec<(usize, CameraUser)>) {
    out.clear();
    for (index, node) in nodes.iter().enumerate() {
        if !node.hardware || node.gone {
            continue;
        }
        let mut readable = false;
        for holder in holds
            .iter()
            .filter(|hold| hold.node == index)
        {
            readable = true;
            let mut named = false;
            for own in holds
                .iter()
                .filter(|hold| hold.pid == holder.pid && nodes[hold.node].virtual_node())
            {
                let mut others = false;
                for reader in holds
                    .iter()
                    .filter(|hold| hold.node == own.node && hold.pid != holder.pid)
                {
                    push(out, index, reader.user());
                    others = true;
                }
                let unexplained = matches!(
                    nodes[own.node].last,
                    Some(Last::Open { wake }) if own.grew < wake
                );
                if !others && unexplained {
                    push(out, index, CameraUser::Unknown);
                    others = true;
                }
                named |= others;
            }
            if !named {
                push(out, index, holder.user());
            }
        }
        if !readable && matches!(node.last, Some(Last::Open { .. })) {
            push(out, index, CameraUser::Unknown);
        }
    }
}

/// A group acts as one holder, so a producer's children, such as a worker that only maps the
/// virtual node, never count as its readers or as users of their own.
pub(crate) fn merge_groups(holds: &mut Vec<Hold>, roots: &[(u32, u32)]) {
    for index in 0..holds.len() {
        let pid = holds[index].pid;
        let Some(&(_, root)) = roots
            .iter()
            .find(|(holder, _)| *holder == pid)
        else {
            continue;
        };
        if root == pid {
            continue;
        }
        let pipewire = holds
            .iter()
            .find(|hold| hold.pid == root)
            .is_some_and(|hold| hold.pipewire);
        holds[index].pid = root;
        holds[index].pipewire = pipewire;
    }
    let mut index = 0;
    while index < holds.len() {
        let Hold { node, pid, fds, .. } = holds[index];
        if let Some(first) = holds[..index]
            .iter_mut()
            .find(|hold| hold.node == node && hold.pid == pid)
        {
            first.fds += fds;
            holds.remove(index);
        } else {
            index += 1;
        }
    }
}

fn push(out: &mut Vec<(usize, CameraUser)>, node: usize, user: CameraUser) {
    if !out.contains(&(node, user)) {
        out.push((node, user));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Step {
    pub changed: bool,
    pub due: Option<Instant>,
}

pub(crate) fn settle(
    now: Instant,
    hold: Duration,
    nodes: &mut [Node],
    uses: &[(usize, CameraUser)],
    published: &mut Vec<CameraUser>,
) -> Step {
    let mut due: Option<Instant> = None;
    let mut wake_at = |at: Instant| {
        if at > now {
            due = Some(due.map_or(at, |due| due.min(at)));
        }
    };
    for (index, node) in nodes.iter_mut().enumerate() {
        let mine = || {
            uses.iter()
                .filter(move |(node, _)| *node == index)
        };
        if mine().next().is_none() {
            node.held_since = None;
            node.users.clear();
            continue;
        }
        wake_at(*node.held_since.get_or_insert(now) + hold);
        node.users
            .retain(|(user, _)| mine().any(|(_, used)| used == user));
        for (_, user) in mine() {
            if !node
                .users
                .iter()
                .any(|(known, _)| known == user)
            {
                node.users.push((*user, now));
            }
        }
        for (_, since) in &node.users {
            wake_at(*since + hold);
        }
    }
    let lasted = |since: Instant| since + hold <= now;
    let shown = |node: usize, user: &CameraUser| {
        let node = &nodes[node];
        if !node.held_since.is_some_and(lasted) {
            return false;
        }
        let established = |(_, since): &&(CameraUser, Instant)| lasted(*since);
        match node
            .users
            .iter()
            .find(|(known, _)| known == user)
        {
            Some(entry) if established(&entry) => true,
            Some(_) => !node
                .users
                .iter()
                .any(|entry| established(&entry)),
            None => false,
        }
    };
    let before = published.len();
    published.retain(|user| {
        uses.iter()
            .any(|(node, used)| used == user && shown(*node, used))
    });
    let mut changed = published.len() != before;
    for (node, user) in uses {
        if shown(*node, user) && !published.contains(user) {
            published.push(*user);
            changed = true;
        }
    }
    Step { changed, due }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOLD: Duration = Duration::from_secs(1);

    fn hw(name: &str) -> Node {
        Node::new(name, true)
    }

    fn virt(name: &str) -> Node {
        Node::new(name, false)
    }

    fn hold(node: usize, pid: u32) -> Hold {
        Hold { node, pid, pipewire: false, fds: 1, grew: 1 }
    }

    fn opened(mut node: Node, wake: u64) -> Node {
        node.last = Some(Last::Open { wake });
        node
    }

    fn closed(mut node: Node) -> Node {
        node.last = Some(Last::Close);
        node
    }

    fn users(nodes: &[Node], holds: &[Hold]) -> Vec<CameraUser> {
        let mut out = Vec::new();
        uses(nodes, holds, &mut out);
        out.into_iter()
            .map(|(_, user)| user)
            .collect()
    }

    use CameraUser::{PipeWire, Process, Unknown};

    #[test]
    fn hardware_holder_is_the_user() {
        assert_eq!(
            users(&[hw("video0")], &[hold(0, 10)]),
            [Process(10)]
        );
    }

    #[test]
    fn virtual_holder_alone_is_no_use() {
        assert_eq!(users(&[virt("video10")], &[hold(0, 10)]), []);
        assert_eq!(users(&[opened(virt("video10"), 3)], &[]), []);
    }

    #[test]
    fn idle_nodes_have_no_use() {
        assert_eq!(
            users(
                &[hw("video0"), closed(hw("video1")), virt("video10")],
                &[]
            ),
            []
        );
    }

    #[test]
    fn several_holders_and_a_fork() {
        let holds = [hold(0, 10), hold(0, 11), hold(1, 10)];
        let mut out = Vec::new();
        uses(&[hw("video0"), hw("video1")], &holds, &mut out);
        assert_eq!(
            out,
            [(0, Process(10)), (0, Process(11)), (1, Process(10))]
        );
    }

    #[test]
    fn mapping_only_holder_counts() {
        let mapped = Hold { fds: 0, ..hold(0, 10) };
        assert_eq!(users(&[hw("video0")], &[mapped]), [Process(10)]);
    }

    #[test]
    fn producer_with_readers_names_the_readers() {
        let nodes = [hw("video0"), virt("video10")];
        let holds = [hold(0, 10), hold(1, 10), hold(1, 20), hold(1, 21)];
        assert_eq!(users(&nodes, &holds), [Process(20), Process(21)]);
    }

    #[test]
    fn producer_without_readers_is_named() {
        let nodes = [hw("video0"), virt("video10")];
        assert_eq!(
            users(&nodes, &[hold(0, 10), hold(1, 10)]),
            [Process(10)]
        );
    }

    #[test]
    fn producer_explains_its_own_open() {
        let nodes = [opened(hw("video0"), 4), opened(virt("video10"), 4)];
        let holds = [Hold { grew: 4, ..hold(0, 10) }, Hold { grew: 4, ..hold(1, 10) }];
        assert_eq!(users(&nodes, &holds), [Process(10)]);
    }

    #[test]
    fn producer_reopening_explains_the_open() {
        let nodes = [hw("video0"), opened(virt("video10"), 9)];
        let holds = [hold(0, 10), Hold { fds: 2, grew: 9, ..hold(1, 10) }];
        assert_eq!(users(&nodes, &holds), [Process(10)]);
    }

    #[test]
    fn unreadable_reader_of_the_virtual_node_is_unknown() {
        let nodes = [hw("video0"), opened(virt("video10"), 7)];
        assert_eq!(
            users(&nodes, &[hold(0, 10), hold(1, 10)]),
            [Unknown]
        );
    }

    #[test]
    fn readable_reader_hides_the_virtual_unknown() {
        let nodes = [hw("video0"), opened(virt("video10"), 7)];
        let holds = [hold(0, 10), hold(1, 10), hold(1, 20)];
        assert_eq!(users(&nodes, &holds), [Process(20)]);
    }

    #[test]
    fn virtual_close_drops_the_unknown() {
        let nodes = [hw("video0"), closed(virt("video10"))];
        assert_eq!(
            users(&nodes, &[hold(0, 10), hold(1, 10)]),
            [Process(10)]
        );
    }

    #[test]
    fn reader_of_the_virtual_node_while_the_webcam_is_closed_is_no_use() {
        let nodes = [hw("video0"), opened(virt("video10"), 7)];
        assert_eq!(users(&nodes, &[hold(1, 10), hold(1, 20)]), []);
    }

    #[test]
    fn pipewire_holder() {
        let pw = Hold { pipewire: true, ..hold(0, 30) };
        assert_eq!(users(&[hw("video0")], &[pw]), [PipeWire]);
    }

    #[test]
    fn pipewire_reading_the_virtual_node() {
        let nodes = [hw("video0"), virt("video10")];
        let holds = [hold(0, 10), hold(1, 10), Hold { pipewire: true, ..hold(1, 30) }];
        assert_eq!(users(&nodes, &holds), [PipeWire]);
    }

    #[test]
    fn unreadable_holder_through_the_last_event() {
        assert_eq!(users(&[opened(hw("video0"), 2)], &[]), [Unknown]);
        assert_eq!(users(&[closed(hw("video0"))], &[]), []);
    }

    #[test]
    fn readable_holder_hides_the_hardware_unknown() {
        assert_eq!(
            users(&[opened(hw("video0"), 2)], &[hold(0, 10)]),
            [Process(10)]
        );
    }

    #[test]
    fn gone_nodes_count_no_more() {
        let mut gone = opened(hw("video0"), 2);
        gone.gone = true;
        assert_eq!(users(&[gone], &[hold(0, 10)]), []);
        let mut gone_virtual = opened(virt("video10"), 2);
        gone_virtual.gone = true;
        let nodes = [hw("video0"), gone_virtual];
        assert_eq!(
            users(&nodes, &[hold(0, 10), hold(1, 10), hold(1, 20)]),
            [Process(10)]
        );
    }

    fn merged(nodes: &[Node], holds: &[Hold], roots: &[(u32, u32)]) -> Vec<CameraUser> {
        let mut holds = holds.to_vec();
        merge_groups(&mut holds, roots);
        users(nodes, &holds)
    }

    #[test]
    fn producer_child_that_maps_the_virtual_node_is_no_reader() {
        let nodes = [hw("video0"), virt("video10")];
        let worker = Hold { fds: 0, ..hold(1, 11) };
        let holds = [hold(0, 10), hold(1, 10), worker, hold(1, 20)];
        let roots = [(10, 10), (11, 10), (20, 20)];
        assert_eq!(merged(&nodes, &holds, &roots), [Process(20)]);
        assert_eq!(users(&nodes, &holds), [Process(11), Process(20)]);
        assert_eq!(merged(&nodes, &holds[..3], &roots), [Process(10)]);
    }

    #[test]
    fn child_that_maps_the_webcam_makes_its_parent_a_producer() {
        let nodes = [hw("video0"), virt("video10")];
        let child = Hold { fds: 0, ..hold(0, 11) };
        let holds = [child, hold(1, 10), hold(1, 20)];
        let roots = [(11, 10), (10, 10), (20, 20)];
        assert_eq!(merged(&nodes, &holds, &roots), [Process(20)]);
    }

    #[test]
    fn group_adds_up_fds_and_takes_the_root_flag() {
        let mut holds = vec![
            Hold { pipewire: true, ..hold(0, 30) },
            Hold { fds: 2, ..hold(0, 31) },
            hold(0, 40),
        ];
        merge_groups(&mut holds, &[(30, 30), (31, 30), (40, 40)]);
        assert_eq!(
            holds,
            [Hold { pipewire: true, fds: 3, ..hold(0, 30) }, hold(0, 40)]
        );
    }

    fn settled(
        now: Instant,
        nodes: &mut [Node],
        holds: &[Hold],
        published: &mut Vec<CameraUser>,
    ) -> Step {
        let mut out = Vec::new();
        uses(nodes, holds, &mut out);
        settle(now, HOLD, nodes, &out, published)
    }

    #[test]
    fn short_probe_never_shows() {
        let t0 = Instant::now();
        let mut nodes = [hw("video0")];
        let mut published = Vec::new();
        let step = settled(t0, &mut nodes, &[hold(0, 10)], &mut published);
        assert_eq!(
            step,
            Step { changed: false, due: Some(t0 + HOLD) }
        );
        let step = settled(t0 + HOLD / 2, &mut nodes, &[], &mut published);
        assert_eq!(step, Step { changed: false, due: None });
        assert_eq!(nodes[0].held_since, None);
        assert!(published.is_empty());
    }

    #[test]
    fn held_node_shows_after_the_hold_and_hides_at_once() {
        let t0 = Instant::now();
        let mut nodes = [hw("video0")];
        let mut published = Vec::new();
        settled(t0, &mut nodes, &[hold(0, 10)], &mut published);
        let step = settled(
            t0 + HOLD,
            &mut nodes,
            &[hold(0, 10)],
            &mut published,
        );
        assert_eq!(step, Step { changed: true, due: None });
        assert_eq!(published, [Process(10)]);
        let step = settled(t0 + HOLD * 5, &mut nodes, &[], &mut published);
        assert_eq!(step, Step { changed: true, due: None });
        assert!(published.is_empty());
    }

    #[test]
    fn hold_runs_per_node_across_a_change_of_user() {
        let t0 = Instant::now();
        let mut nodes = [opened(hw("video0"), 1)];
        let mut published = Vec::new();
        settled(t0, &mut nodes, &[], &mut published);
        let step = settled(
            t0 + HOLD,
            &mut nodes,
            &[hold(0, 10)],
            &mut published,
        );
        assert!(step.changed);
        assert_eq!(published, [Process(10)]);
    }

    #[test]
    fn producer_tail_keeps_the_dot() {
        let t0 = Instant::now();
        let mut nodes = [hw("video0"), virt("video10")];
        let mut published = Vec::new();
        let reading = [hold(0, 10), hold(1, 10), hold(1, 20)];
        settled(t0, &mut nodes, &reading, &mut published);
        settled(t0 + HOLD, &mut nodes, &reading, &mut published);
        assert_eq!(published, [Process(20)]);
        let tail = [hold(0, 10), hold(1, 10)];
        let step = settled(t0 + HOLD * 2, &mut nodes, &tail, &mut published);
        assert_eq!(
            step,
            Step { changed: true, due: Some(t0 + HOLD * 3) }
        );
        assert_eq!(published, [Process(10)]);
    }

    #[test]
    fn second_app_probing_during_a_use_stays_hidden() {
        let t0 = Instant::now();
        let mut nodes = [hw("video0")];
        let mut published = Vec::new();
        settled(t0, &mut nodes, &[hold(0, 10)], &mut published);
        settled(
            t0 + HOLD,
            &mut nodes,
            &[hold(0, 10)],
            &mut published,
        );
        let probe = [hold(0, 10), hold(0, 11)];
        let step = settled(t0 + HOLD * 2, &mut nodes, &probe, &mut published);
        assert_eq!(
            step,
            Step { changed: false, due: Some(t0 + HOLD * 3) }
        );
        assert_eq!(published, [Process(10)]);
        settled(
            t0 + HOLD * 2 + HOLD / 4,
            &mut nodes,
            &[hold(0, 10)],
            &mut published,
        );
        assert_eq!(published, [Process(10)]);
        settled(t0 + HOLD * 4, &mut nodes, &probe, &mut published);
        settled(t0 + HOLD * 5, &mut nodes, &probe, &mut published);
        assert_eq!(published, [Process(10), Process(11)]);
    }

    #[test]
    fn new_user_shows_at_once_when_it_is_the_only_one() {
        let t0 = Instant::now();
        let mut nodes = [hw("video0")];
        let mut published = Vec::new();
        settled(t0, &mut nodes, &[hold(0, 10)], &mut published);
        settled(
            t0 + HOLD,
            &mut nodes,
            &[hold(0, 10)],
            &mut published,
        );
        settled(
            t0 + HOLD * 2,
            &mut nodes,
            &[hold(0, 11)],
            &mut published,
        );
        assert_eq!(published, [Process(11)]);
    }

    #[test]
    fn published_order_follows_the_start_of_uses() {
        let t0 = Instant::now();
        let mut nodes = [hw("video0"), hw("video1")];
        let mut published = Vec::new();
        settled(t0, &mut nodes, &[hold(1, 20)], &mut published);
        settled(
            t0 + HOLD,
            &mut nodes,
            &[hold(1, 20), hold(0, 10)],
            &mut published,
        );
        assert_eq!(published, [Process(20)]);
        settled(
            t0 + HOLD * 2,
            &mut nodes,
            &[hold(1, 20), hold(0, 10)],
            &mut published,
        );
        assert_eq!(published, [Process(20), Process(10)]);
    }
}
