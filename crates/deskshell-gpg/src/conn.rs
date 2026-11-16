//! One proxied connection's Assuan lines, adapted from yubikey-touch-detector 1.15.0.

use std::time::Duration;

use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::task::AbortHandle;

use crate::assuan::fields;
use crate::keys::{Keygrip, Keys};
use crate::sys;
use crate::waits::{Block, Event, OpId, Waits, next_op};

const PINENTRY_POLL: Duration = Duration::from_millis(100);

pub struct Conn<'a> {
    keys: &'a Keys,
    waits: &'a Waits,
    signing: Option<Keygrip>,
    decrypt: Option<Keygrip>,
    op: Option<OpId>,
    inquiry: bool,
    pinentries: Vec<AbortHandle>,
}

impl<'a> Conn<'a> {
    pub fn new(keys: &'a Keys, waits: &'a Waits) -> Self {
        Self {
            keys,
            waits,
            signing: None,
            decrypt: None,
            op: None,
            inquiry: false,
            pinentries: Vec::new(),
        }
    }

    /// libassuan matches commands without regard to case, so this does too.
    pub fn client_line(&mut self, line: &[u8]) {
        let mut fields = fields(line);
        let Some(command) = fields.next() else {
            return;
        };
        let is = |name: &[u8]| command.eq_ignore_ascii_case(name);
        if is(b"SIGKEY") {
            if let Some(grip) = fields.last() {
                self.signing = Keygrip::parse(grip);
            }
        } else if is(b"SETKEY") {
            if let Some(grip) = fields.last() {
                self.decrypt = Keygrip::parse(grip);
            }
        } else if is(b"PKSIGN") {
            self.start(self.signing);
        } else if is(b"PKDECRYPT") {
            self.start(self.decrypt);
        } else if is(b"RESET") {
            self.signing = None;
            self.decrypt = None;
        } else if (is(b"END") || is(b"CAN")) && self.inquiry {
            self.inquiry = false;
            if let Some(op) = self.op {
                self.waits
                    .send(Event::Unblock(op, Block::Inquiry));
            }
        }
    }

    pub fn server_line(&mut self, line: &[u8]) {
        let Some(op) = self.op else {
            return;
        };
        let mut fields = fields(line);
        match fields.next() {
            Some(b"INQUIRE") => {
                self.inquiry = true;
                self.waits
                    .send(Event::Block(op, Block::Inquiry));
                if fields.next() == Some(b"PINENTRY_LAUNCHED")
                    && let Some(pid) = fields.next()
                {
                    self.block_pinentry(op, pid);
                }
            }
            Some(b"S") => {
                if fields.next() == Some(b"PINENTRY_LAUNCHED")
                    && let Some(pid) = fields.next()
                {
                    self.block_pinentry(op, pid);
                }
            }
            Some(b"OK" | b"ERR") => self.finish(),
            _ => {}
        }
    }

    fn start(&mut self, key: Option<Keygrip>) {
        self.finish();
        if key.is_some_and(|grip| self.keys.is_card_key(&grip)) {
            let op = next_op();
            self.waits.send(Event::Start(op));
            self.op = Some(op);
        }
    }

    fn finish(&mut self) {
        self.inquiry = false;
        for pinentry in self.pinentries.drain(..) {
            pinentry.abort();
        }
        if let Some(op) = self.op.take() {
            self.waits.send(Event::Finish(op));
        }
    }

    fn block_pinentry(&mut self, op: OpId, field: &[u8]) {
        let digits = field
            .split(|&b| b == b':')
            .next()
            .unwrap_or(field);
        let Some(pid) = std::str::from_utf8(digits)
            .ok()
            .and_then(|text| text.parse::<i32>().ok())
            .filter(|pid| *pid > 0)
        else {
            return;
        };
        let block = Block::Pinentry(pid);
        self.waits.send(Event::Block(op, block));
        let waits = self.waits.clone();
        let task = tokio::spawn(async move {
            process_exit(pid).await;
            waits.send(Event::Unblock(op, block));
        });
        self.pinentries
            .push(task.abort_handle());
    }
}

impl Drop for Conn<'_> {
    fn drop(&mut self) {
        self.finish();
    }
}

async fn process_exit(pid: i32) {
    if let Ok(pidfd) = sys::pidfd_open(pid)
        // SAFETY: the AsyncFd owns the OwnedFd, which stays open and keeps its number
        // until the AsyncFd drops it.
        && let Ok(pidfd) = unsafe { AsyncFd::register_with_interest(pidfd, Interest::READABLE) }
        && pidfd.readable().await.is_ok()
    {
        return;
    }
    while sys::process_exists(pid) {
        tokio::time::sleep(PINENTRY_POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::process::{Child, Command};

    use tokio::sync::mpsc::UnboundedReceiver;
    use tokio::sync::mpsc::error::TryRecvError;

    use super::*;
    use crate::test_dir::{TempDir, forking};

    const CARD: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const SOFT: &str = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

    struct Fixture {
        home: TempDir,
        waits: Waits,
        events: RefCell<UnboundedReceiver<Event>>,
    }

    impl Fixture {
        fn new() -> Self {
            let home = TempDir::new("conn");
            let dir = home.path().join("private-keys-v1.d");
            fs::create_dir(&dir).unwrap();
            fs::write(
                dir.join(format!("{CARD}.key")),
                "Key: (shadowed-private-key (ecc))\n",
            )
            .unwrap();
            fs::write(
                dir.join(format!("{SOFT}.key")),
                "Key: (private-key (ecc))\n",
            )
            .unwrap();
            let (waits, events) = Waits::channel();
            Self { home, waits, events: RefCell::new(events) }
        }

        fn keys(&self) -> Keys {
            Keys::new(self.home.path())
        }

        fn drain(&self) -> Vec<Event> {
            let mut out = Vec::new();
            while let Ok(event) = self.events.borrow_mut().try_recv() {
                out.push(event);
            }
            out
        }
    }

    fn started(events: &[Event]) -> OpId {
        match events {
            [Event::Start(op)] => *op,
            other => panic!("expected one start, got {other:?}"),
        }
    }

    #[test]
    fn a_software_key_never_counts() {
        let f = Fixture::new();
        let keys = f.keys();
        let mut conn = Conn::new(&keys, &f.waits);
        conn.client_line(format!("SIGKEY {CARD}").as_bytes());
        conn.client_line(format!("SIGKEY {SOFT}").as_bytes());
        conn.client_line(b"PKSIGN");
        conn.client_line(b"SETKEY not-a-keygrip");
        conn.client_line(b"PKDECRYPT");
        conn.server_line(b"INQUIRE CIPHERTEXT");
        conn.server_line(b"OK");
        drop(conn);
        assert_eq!(f.drain(), []);
    }

    #[test]
    fn setkey_with_options_and_lowercase() {
        let f = Fixture::new();
        let keys = f.keys();
        let mut conn = Conn::new(&keys, &f.waits);
        conn.client_line(format!("SETKEY --force {}", CARD.to_lowercase()).as_bytes());
        conn.client_line(b"pkdecrypt");
        let op = started(&f.drain());
        conn.server_line(b"OK");
        assert_eq!(f.drain(), [Event::Finish(op)]);
    }

    #[test]
    fn the_signing_key_stays_across_operations() {
        let f = Fixture::new();
        let keys = f.keys();
        let mut conn = Conn::new(&keys, &f.waits);
        conn.client_line(format!("SIGKEY {CARD}").as_bytes());
        conn.client_line(b"SIGKEY");
        conn.client_line(b"PKSIGN");
        let first = started(&f.drain());
        conn.server_line(b"OK");
        assert_eq!(f.drain(), [Event::Finish(first)]);
        conn.client_line(b"PKSIGN");
        let second = started(&f.drain());
        assert_ne!(first, second);
        conn.server_line(b"ERR 83886179 Operation cancelled");
        assert_eq!(f.drain(), [Event::Finish(second)]);

        conn.client_line(b"RESET");
        conn.client_line(b"PKSIGN");
        assert_eq!(f.drain(), [], "RESET clears the keys");
    }

    #[test]
    fn a_new_operation_finishes_the_current_one() {
        let f = Fixture::new();
        let keys = f.keys();
        let mut conn = Conn::new(&keys, &f.waits);
        conn.client_line(format!("SIGKEY {CARD}").as_bytes());
        conn.client_line(b"PKSIGN");
        let first = started(&f.drain());
        conn.client_line(b"PKSIGN");
        match f.drain()[..] {
            [Event::Finish(done), Event::Start(next)] => {
                assert_eq!(done, first);
                assert_ne!(next, first);
            }
            ref other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_inquiry_blocks_until_end_or_can() {
        let f = Fixture::new();
        let keys = f.keys();
        let mut conn = Conn::new(&keys, &f.waits);
        conn.client_line(format!("SETKEY {CARD}").as_bytes());
        conn.client_line(b"END");
        conn.client_line(b"PKDECRYPT");
        let op = started(&f.drain());
        conn.server_line(b"INQUIRE CIPHERTEXT");
        conn.client_line(b"D (enc-val (ecdh (s #00#)))");
        assert_eq!(f.drain(), [Event::Block(op, Block::Inquiry)]);
        conn.client_line(b"END");
        conn.client_line(b"END");
        assert_eq!(
            f.drain(),
            [Event::Unblock(op, Block::Inquiry)],
            "one unblock per inquiry"
        );
        conn.server_line(b"INQUIRE CIPHERTEXT");
        conn.client_line(b"CAN");
        assert_eq!(
            f.drain(),
            [Event::Block(op, Block::Inquiry), Event::Unblock(op, Block::Inquiry)]
        );
        conn.server_line(b"S PROGRESS learncard k 0 0");
        conn.server_line(b"# comment");
        conn.server_line(b"D data");
        assert_eq!(f.drain(), []);
    }

    #[test]
    fn server_lines_without_an_operation_do_nothing() {
        let f = Fixture::new();
        let keys = f.keys();
        let mut conn = Conn::new(&keys, &f.waits);
        conn.server_line(b"INQUIRE PINENTRY_LAUNCHED 1");
        conn.client_line(b"END");
        conn.server_line(b"S PINENTRY_LAUNCHED 1");
        conn.server_line(b"OK");
        assert_eq!(f.drain(), []);
    }

    #[test]
    fn closing_mid_operation_finishes_it() {
        let f = Fixture::new();
        let keys = f.keys();
        let mut conn = Conn::new(&keys, &f.waits);
        conn.client_line(format!("SIGKEY {CARD}").as_bytes());
        conn.client_line(b"PKSIGN");
        let op = started(&f.drain());
        drop(conn);
        assert_eq!(f.drain(), [Event::Finish(op)]);
    }

    fn sleeper() -> Child {
        let _fork = forking();
        Command::new("sleep")
            .arg("0.3")
            .spawn()
            .unwrap()
    }

    #[expect(
        clippy::await_holding_refcell_ref,
        reason = "the fixture runs on one thread and nothing else borrows the receiver"
    )]
    async fn next_event(f: &Fixture) -> Event {
        tokio::time::timeout(
            Duration::from_secs(5),
            f.events.borrow_mut().recv(),
        )
        .await
        .unwrap()
        .unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_pinentry_status_blocks_until_the_process_exits() {
        let f = Fixture::new();
        let keys = f.keys();
        let mut conn = Conn::new(&keys, &f.waits);
        let mut pinentry = sleeper();
        let pid = pinentry.id() as i32;
        conn.client_line(format!("SIGKEY {CARD}").as_bytes());
        conn.client_line(b"PKSIGN");
        let op = started(&f.drain());
        conn.server_line(format!("S PINENTRY_LAUNCHED {pid}:curses 1.3.1 /dev/pts/1 -").as_bytes());
        assert_eq!(
            f.drain(),
            [Event::Block(op, Block::Pinentry(pid))]
        );
        tokio::task::yield_now().await;
        assert_eq!(
            f.events.borrow_mut().try_recv(),
            Err(TryRecvError::Empty)
        );

        pinentry.wait().unwrap();
        assert_eq!(
            next_event(&f).await,
            Event::Unblock(op, Block::Pinentry(pid))
        );
        conn.server_line(b"OK");
        assert_eq!(f.drain(), [Event::Finish(op)]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_pinentry_inquiry_blocks_past_the_inquiry() {
        let f = Fixture::new();
        let keys = f.keys();
        let mut conn = Conn::new(&keys, &f.waits);
        let mut pinentry = sleeper();
        let pid = pinentry.id() as i32;
        conn.client_line(format!("SIGKEY {CARD}").as_bytes());
        conn.client_line(b"PKSIGN");
        let op = started(&f.drain());
        conn.server_line(format!("INQUIRE PINENTRY_LAUNCHED {pid} curses 1.3.1 - -").as_bytes());
        conn.client_line(b"END");
        assert_eq!(
            f.drain(),
            [
                Event::Block(op, Block::Inquiry),
                Event::Block(op, Block::Pinentry(pid)),
                Event::Unblock(op, Block::Inquiry),
            ]
        );
        pinentry.wait().unwrap();
        assert_eq!(
            next_event(&f).await,
            Event::Unblock(op, Block::Pinentry(pid))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_pinentry_that_is_already_gone_or_bogus() {
        let f = Fixture::new();
        let keys = f.keys();
        let mut conn = Conn::new(&keys, &f.waits);
        let mut gone = {
            let _fork = forking();
            Command::new("true").spawn().unwrap()
        };
        let pid = gone.id() as i32;
        gone.wait().unwrap();
        conn.client_line(format!("SIGKEY {CARD}").as_bytes());
        conn.client_line(b"PKSIGN");
        let op = started(&f.drain());
        conn.server_line(b"S PINENTRY_LAUNCHED 0");
        conn.server_line(b"S PINENTRY_LAUNCHED -5");
        conn.server_line(b"S PINENTRY_LAUNCHED abc");
        conn.server_line(b"S PINENTRY_LAUNCHED");
        assert_eq!(f.drain(), []);
        conn.server_line(format!("S PINENTRY_LAUNCHED {pid}").as_bytes());
        assert_eq!(
            next_event(&f).await,
            Event::Block(op, Block::Pinentry(pid))
        );
        assert_eq!(
            next_event(&f).await,
            Event::Unblock(op, Block::Pinentry(pid))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finishing_drops_the_pinentry_watch() {
        let f = Fixture::new();
        let keys = f.keys();
        let mut conn = Conn::new(&keys, &f.waits);
        let mut pinentry = sleeper();
        let pid = pinentry.id() as i32;
        conn.client_line(format!("SIGKEY {CARD}").as_bytes());
        conn.client_line(b"PKSIGN");
        let op = started(&f.drain());
        conn.server_line(format!("S PINENTRY_LAUNCHED {pid}").as_bytes());
        conn.server_line(b"ERR 1");
        assert_eq!(
            f.drain(),
            [Event::Block(op, Block::Pinentry(pid)), Event::Finish(op)]
        );
        pinentry.wait().unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(f.drain(), [], "the watch was aborted");
    }
}
