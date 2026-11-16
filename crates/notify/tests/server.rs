//! The notification server over a private bus.

mod common;

use std::assert_matches;
use std::collections::HashMap;
use std::future::poll_fn;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use common::{Harness, PrivateBus, Sig, no_hints, within};
use deskshell_notify::{
    BUS_NAME, Command, Config, Effect, Image, Rect, SERVER_VERSION, Service, TimerKind, Urgency,
};
use zbus::export::futures_core::Stream;
use zbus::fdo::{DBusProxy, RequestNameFlags};
use zbus::message::Type as MessageType;
use zbus::{MessageStream, Variant};

const QUIET: Duration = Duration::from_millis(200);

#[tokio::test]
async fn capabilities_and_server_information() {
    let h = Harness::start().await;
    let capabilities = h
        .proxy
        .get_capabilities()
        .await
        .unwrap();
    assert_eq!(
        capabilities,
        [
            "persistence",
            "body",
            "body-markup",
            "body-hyperlinks",
            "actions",
            "icon-static",
            "inline-reply"
        ]
    );
    let info = h
        .proxy
        .get_server_information()
        .await
        .unwrap();
    assert_eq!(
        info,
        (
            "deskshell".into(),
            "deskshell".into(),
            SERVER_VERSION.into(),
            "1.3".into()
        )
    );
}

#[tokio::test]
async fn ids_start_at_one_and_increase() {
    let h = Harness::start().await;
    assert_eq!(h.notify("Chat", "one").await, 1);
    assert_eq!(h.notify("Chat", "two").await, 2);
    assert_eq!(h.notify("Mail", "three").await, 3);
    let state = h.state.borrow();
    let ids: Vec<_> = state
        .history
        .iter()
        .map(|e| e.server_id)
        .collect();
    assert_eq!(ids, [3, 2, 1], "newest first");
    let first = &state.history[2];
    assert_eq!(first.uid, format!("{}-1", first.received_at));
    assert!(first.live);
}

#[tokio::test]
async fn fallbacks_for_app_name_and_summary() {
    let h = Harness::start().await;
    h.notify("", "").await;
    h.notify("Chat", "").await;
    let state = h.state.borrow();
    assert_eq!(state.history[1].app_name, "Notification");
    assert_eq!(state.history[1].summary, "Notification");
    assert_eq!(state.history[0].summary, "Chat");
}

#[tokio::test]
async fn replace_in_place_keeps_position_and_received_at() {
    let h = Harness::start().await;
    h.notify("Chat", "a").await;
    h.notify("Chat", "b").await;
    h.notify("Chat", "c").await;
    let before = Arc::clone(&h.state.borrow().history[1]);
    tokio::time::sleep(Duration::from_millis(5)).await;

    let hints = HashMap::from([("urgency", Variant::U8(2))]);
    let id = h
        .notify_full("Chat", 2, "b2", &["default", "Open"], &hints)
        .await;
    assert_eq!(id, 2);

    let state = h.state.borrow().clone();
    let summaries: Vec<_> = state
        .history
        .iter()
        .map(|e| e.summary.as_str())
        .collect();
    assert_eq!(summaries, ["c", "b2", "a"]);
    let popups: Vec<_> = state
        .popups
        .iter()
        .map(|e| e.summary.as_str())
        .collect();
    assert_eq!(popups, ["c", "b2", "a"]);
    let after = &state.history[1];
    assert_eq!(after.uid, before.uid);
    assert_eq!(after.received_at, before.received_at);
    assert_eq!(after.urgency, Urgency::Critical);
    assert_eq!(after.actions.len(), 1);
    assert!(Arc::ptr_eq(after, &state.popups[1]));
}

#[tokio::test]
async fn a_replace_with_an_odd_action_list_keeps_the_old_actions() {
    let h = Harness::start().await;
    let actions = ["default", "Open", "inline-reply", "Answer"];
    let id = h
        .notify_full("Chat", 0, "a", &actions, &no_hints())
        .await;
    h.notify_full("Chat", id, "b", &["dangling"], &no_hints())
        .await;
    let entry = Arc::clone(&h.state.borrow().history[0]);
    assert_eq!(entry.summary, "b");
    assert_eq!(entry.actions.len(), 1);
    assert_eq!(entry.inline_reply.as_deref(), Some("Answer"));

    h.notify_full("Chat", id, "c", &["default", "Open"], &no_hints())
        .await;
    assert_eq!(
        h.state.borrow().history[0].inline_reply,
        None,
        "recomputed on every Notify"
    );
}

#[tokio::test]
async fn replacing_an_unknown_or_closed_id_creates_a_new_notification() {
    let mut h = Harness::start().await;
    assert_eq!(
        h.notify_full("Chat", 42, "unknown", &[], &no_hints())
            .await,
        1
    );
    h.proxy
        .close_notification(1)
        .await
        .unwrap();
    assert_eq!(h.signals.next().await, Sig::Closed(1, 3));
    assert_eq!(
        h.notify_full("Chat", 1, "closed", &[], &no_hints())
            .await,
        2
    );
    assert_eq!(h.state.borrow().history.len(), 2);
}

#[tokio::test]
async fn close_notification_emits_reason_3() {
    let mut h = Harness::start().await;
    let id = h.notify("Chat", "hello").await;
    let address = Arc::as_ptr(&h.state.borrow().history[0]);
    h.proxy
        .close_notification(id)
        .await
        .unwrap();
    assert_eq!(h.signals.next().await, Sig::Closed(id, 3));

    let state = h.state.borrow().clone();
    assert!(state.popups.is_empty(), "the popup goes away");
    assert_eq!(state.history.len(), 1, "the history entry stays");
    assert!(!state.history[0].live);
    assert_eq!(
        Arc::as_ptr(&state.history[0]),
        address,
        "flipped in place, not copied"
    );

    h.proxy
        .close_notification(id)
        .await
        .unwrap();
    h.proxy
        .close_notification(99)
        .await
        .unwrap();
    h.signals.none_within(QUIET).await;
}

#[tokio::test]
async fn dismiss_emits_reason_2() {
    let mut h = Harness::start().await;
    let keep = h.notify("Chat", "keep").await;
    let gone = h.notify("Chat", "gone").await;
    h.service
        .send(Command::Dismiss(h.uid(gone)));
    assert_eq!(h.signals.next().await, Sig::Closed(gone, 2));
    h.wait(|s| s.history.len() == 1).await;
    let state = h.state.borrow().clone();
    assert_eq!(state.history[0].server_id, keep);
    assert_eq!(state.popups.len(), 1);
    assert_eq!(state.popups[0].server_id, keep);
}

#[tokio::test]
async fn action_invoked_only_for_live_entries() {
    let mut h = Harness::start().await;
    let id = h
        .notify_full(
            "Chat",
            0,
            "hi",
            &["default", "", "mark", "Mark read"],
            &no_hints(),
        )
        .await;
    let sentinel = h.notify("Chat", "sentinel").await;
    let uid = h.uid(id);

    h.service
        .send(Command::Invoke(uid.clone(), "missing".into()));
    h.service
        .send(Command::Invoke(uid.clone(), "mark".into()));
    assert_eq!(
        h.signals.next().await,
        Sig::Invoked(id, "mark".into())
    );
    assert_eq!(
        h.signals.next().await,
        Sig::Closed(id, 2),
        "not resident, so it closes"
    );
    h.wait(|s| s.popups.len() == 1).await;
    {
        let state = h.state.borrow();
        let entry = state
            .history
            .iter()
            .find(|e| e.server_id == id)
            .unwrap();
        assert!(
            !entry.live,
            "the entry stays in history, no longer live"
        );
    }

    h.service
        .send(Command::Invoke(uid, "default".into()));
    h.service
        .send(Command::Dismiss(h.uid(sentinel)));
    assert_eq!(
        h.signals.next().await,
        Sig::Closed(sentinel, 2),
        "nothing for a closed entry"
    );
}

#[tokio::test]
async fn resident_notifications_stay_live_after_invoke() {
    let mut h = Harness::start().await;
    let hints = HashMap::from([("resident", Variant::Bool(true))]);
    let id = h
        .notify_full("Player", 0, "song", &["default", "Play"], &hints)
        .await;
    let uid = h.uid(id);
    h.service
        .send(Command::Invoke(uid.clone(), "default".into()));
    assert_eq!(
        h.signals.next().await,
        Sig::Invoked(id, "default".into())
    );
    h.service
        .send(Command::Invoke(uid, "default".into()));
    assert_eq!(
        h.signals.next().await,
        Sig::Invoked(id, "default".into())
    );
    assert!(h.state.borrow().history[0].live);
}

#[tokio::test]
async fn notification_replied() {
    let mut h = Harness::start().await;
    let plain = h
        .notify_full(
            "Chat",
            0,
            "no reply",
            &["default", "Open"],
            &no_hints(),
        )
        .await;
    let id = h
        .notify_full(
            "Chat",
            0,
            "reply",
            &["inline-reply", "Type a reply", "default", ""],
            &no_hints(),
        )
        .await;
    {
        let state = h.state.borrow();
        let entry = &state.history[0];
        assert_eq!(
            entry.inline_reply.as_deref(),
            Some("Type a reply")
        );
        assert_eq!(
            entry.actions.len(),
            1,
            "inline-reply is not an action"
        );
        assert_eq!(entry.actions[0].label, "Open");
    }
    let uid = h.uid(id);

    h.service
        .send(Command::Reply(h.uid(plain), "ignored".into()));
    h.service
        .send(Command::Reply(uid.clone(), "   ".into()));
    h.service.send(Command::Reply(
        uid.clone(),
        "  on my way  ".into(),
    ));
    assert_eq!(
        h.signals.next().await,
        Sig::Replied(id, "on my way".into())
    );
    assert_eq!(h.signals.next().await, Sig::Closed(id, 2));

    h.service
        .send(Command::Reply(uid, "again".into()));
    h.service
        .send(Command::Dismiss(h.uid(plain)));
    assert_eq!(h.signals.next().await, Sig::Closed(plain, 2));
    assert!(
        h.effects.try_recv().is_err(),
        "a reply starts no focus grant"
    );
}

#[tokio::test]
async fn transient_skips_history() {
    let mut h = Harness::start().await;
    let hints = HashMap::from([("transient", Variant::Bool(true))]);
    let id = h
        .notify_full("Volume", 0, "50%", &[], &hints)
        .await;
    {
        let state = h.state.borrow();
        assert!(state.history.is_empty());
        assert_eq!(state.popups.len(), 1);
    }
    h.proxy
        .close_notification(id)
        .await
        .unwrap();
    assert_eq!(h.signals.next().await, Sig::Closed(id, 3));
    assert!(h.state.borrow().popups.is_empty());
}

#[tokio::test]
async fn history_and_popup_caps() {
    let h = Harness::start().await;
    for i in 1..=105 {
        h.notify("Chat", &i.to_string()).await;
    }
    let state = h.state.borrow();
    assert_eq!(state.history.len(), 100);
    assert_eq!(state.history[0].server_id, 105);
    assert_eq!(state.history[99].server_id, 6);
    assert_eq!(state.popups.len(), 20);
    assert_eq!(state.popups[0].server_id, 105);
    assert_eq!(state.popups[19].server_id, 86);
}

#[tokio::test]
async fn an_entry_pushed_out_of_history_without_a_popup_expires() {
    let mut h = Harness::start().await;
    for i in 1..=100 {
        h.notify("Chat", &i.to_string()).await;
    }
    assert!(
        h.state
            .borrow()
            .history
            .iter()
            .all(|e| e.live),
        "out of popups, still in history"
    );
    h.notify("Chat", "101").await;
    assert_eq!(h.signals.next().await, Sig::Closed(1, 1));
    h.proxy
        .close_notification(2)
        .await
        .unwrap();
    assert_eq!(
        h.signals.next().await,
        Sig::Closed(2, 3),
        "only the entry that left expires"
    );
}

#[tokio::test]
async fn an_entry_out_of_history_stays_live_while_its_popup_shows() {
    let mut h = Harness::start().await;
    let first = h.notify("Chat", "first").await;
    for i in 0..100 {
        let id = h.notify("Chat", &i.to_string()).await;
        h.service
            .send(Command::HidePopup(h.uid(id)));
    }
    h.wait(|s| s.popups.len() == 1 && s.popups[0].server_id == first)
        .await;
    assert!(
        h.state
            .borrow()
            .history
            .iter()
            .all(|e| e.server_id != first)
    );

    let transient = HashMap::from([("transient", Variant::Bool(true))]);
    let marker = h
        .notify_full("Volume", 0, "marker", &[], &transient)
        .await;
    h.proxy
        .close_notification(marker)
        .await
        .unwrap();
    assert_eq!(
        h.signals.next().await,
        Sig::Closed(marker, 3),
        "nothing expired while the popup shows"
    );

    let uid = h.state.borrow().popups[0].uid.clone();
    h.service.send(Command::HidePopup(uid));
    assert_eq!(h.signals.next().await, Sig::Closed(first, 1));
}

#[tokio::test]
async fn a_transient_notification_expires_when_its_popup_is_hidden() {
    let mut h = Harness::start().await;
    let transient = HashMap::from([("transient", Variant::Bool(true))]);
    let kept = h.notify("Chat", "kept").await;
    let id = h
        .notify_full("Volume", 0, "50%", &[], &transient)
        .await;
    h.service
        .send(Command::HidePopup(h.uid(kept)));
    h.service
        .send(Command::HidePopup(h.uid(id)));
    assert_eq!(h.signals.next().await, Sig::Closed(id, 1));
    h.wait(|s| s.popups.is_empty()).await;
    assert!(
        h.state.borrow().history[0].live,
        "a hidden popup in history stays live"
    );

    let other = h
        .notify_full("Volume", 0, "60%", &[], &transient)
        .await;
    h.service.send(Command::ClearPopups);
    assert_eq!(h.signals.next().await, Sig::Closed(other, 1));
}

#[tokio::test]
async fn a_transient_notification_expires_when_dnd_clears_the_popups() {
    let mut h = Harness::start().await;
    let transient = HashMap::from([("transient", Variant::Bool(true))]);
    let kept = h.notify("Chat", "kept").await;
    let id = h
        .notify_full("Volume", 0, "50%", &[], &transient)
        .await;
    h.service.send(Command::ToggleDnd);
    assert_eq!(h.signals.next().await, Sig::Closed(id, 1));
    h.wait(|s| s.dnd).await;
    h.proxy
        .close_notification(kept)
        .await
        .unwrap();
    assert_eq!(
        h.signals.next().await,
        Sig::Closed(kept, 3),
        "the history entry stayed live"
    );
}

#[tokio::test]
async fn a_transient_notification_under_dnd_expires_after_the_reply() {
    let mut h = Harness::start().await;
    h.service.send(Command::ToggleDnd);
    h.wait(|s| s.dnd).await;
    let mut all = MessageStream::from(&h.client);
    let transient = HashMap::from([("transient", Variant::Bool(true))]);
    let id = h
        .notify_full("Volume", 0, "50%", &[], &transient)
        .await;
    assert_eq!(h.signals.next().await, Sig::Closed(id, 1));
    {
        let state = h.state.borrow();
        assert!(state.history.is_empty() && state.popups.is_empty());
    }

    let mut replied = false;
    loop {
        let message = within(common::next_message(&mut all)).await;
        let header = message.header();
        match header.message_type() {
            MessageType::MethodReturn => replied = true,
            MessageType::Signal
                if header
                    .member()
                    .is_some_and(|m| m == "NotificationClosed") =>
            {
                assert!(
                    replied,
                    "the reply with the id comes before the close"
                );
                break;
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn dnd_suppresses_popups_and_turning_it_on_clears_them() {
    let mut h = Harness::start().await;
    h.notify("Chat", "before").await;
    assert_eq!(h.state.borrow().popups.len(), 1);

    h.service.send(Command::ToggleDnd);
    h.wait(|s| s.dnd).await;
    assert!(h.state.borrow().popups.is_empty());

    h.notify("Chat", "during").await;
    {
        let state = h.state.borrow();
        assert_eq!(state.history.len(), 2);
        assert!(state.popups.is_empty());
        assert!(
            state.history.iter().all(|e| e.live),
            "hiding popups closes nothing"
        );
    }

    h.service.send(Command::ToggleDnd);
    h.wait(|s| !s.dnd).await;
    assert!(h.state.borrow().popups.is_empty());
    h.notify("Chat", "after").await;
    assert_eq!(h.state.borrow().popups.len(), 1);
}

#[tokio::test]
async fn hide_popup_and_clear_popups_keep_notifications_live() {
    let mut h = Harness::start().await;
    let a = h.notify("Chat", "a").await;
    h.notify("Chat", "b").await;
    h.notify("Chat", "c").await;
    h.service
        .send(Command::HidePopup(h.uid(a)));
    h.wait(|s| s.popups.len() == 2).await;
    h.service.send(Command::ClearPopups);
    h.wait(|s| s.popups.is_empty()).await;
    let state = h.state.borrow();
    assert_eq!(state.history.len(), 3);
    assert!(state.history.iter().all(|e| e.live));
}

#[tokio::test]
async fn clear_dismisses_every_live_notification() {
    let mut h = Harness::start().await;
    let transient = HashMap::from([("transient", Variant::Bool(true))]);
    let a = h.notify("Chat", "a").await;
    let b = h
        .notify_full("Volume", 0, "b", &[], &transient)
        .await;
    let c = h.notify("Mail", "c").await;
    h.proxy
        .close_notification(c)
        .await
        .unwrap();
    assert_eq!(h.signals.next().await, Sig::Closed(c, 3));

    h.service.send(Command::Clear);
    let mut closed = vec![h.signals.next().await, h.signals.next().await];
    closed.sort_by_key(|s| match s {
        Sig::Closed(id, _) => *id,
        _ => 0,
    });
    assert_eq!(closed, [Sig::Closed(a, 2), Sig::Closed(b, 2)]);
    h.wait(|s| s.history.is_empty() && s.popups.is_empty())
        .await;
    h.signals.none_within(QUIET).await;
}

fn rgba(width: i32, height: i32) -> Variant<'static> {
    let data: Vec<u8> = (0..width * height * 4)
        .map(|i| i as u8)
        .collect();
    Variant::from((width, height, width * 4, true, 8, 4, data))
}

#[tokio::test]
async fn image_pixels_are_shared_between_history_and_popup() {
    let h = Harness::start().await;
    let hints =
        HashMap::from([("image-data", rgba(16, 8)), ("image-path", Variant::from("unused"))]);
    h.notify_full("Chat", 0, "avatar", &[], &hints)
        .await;

    let state = h.state.borrow().clone();
    let (Some(Image::Pixels(history)), Some(Image::Pixels(popup))) =
        (&state.history[0].image, &state.popups[0].image)
    else {
        panic!("expected pixels in both lists");
    };
    assert!(Arc::ptr_eq(history, popup));
    assert_eq!(
        (history.width, history.height, history.rowstride),
        (16, 8, 64)
    );
    assert!(history.has_alpha);
    assert_eq!(history.data.len(), 16 * 8 * 4);
    assert_eq!(history.data[5], 5);
}

#[tokio::test]
async fn image_path_is_the_fallback() {
    let h = Harness::start().await;
    let hints = HashMap::from([(
        "image_path",
        Variant::from("file:///tmp/avatar.png"),
    )]);
    h.notify_full("Chat", 0, "avatar", &[], &hints)
        .await;
    let bad =
        HashMap::from([("image-data", Variant::from("garbage")), ("urgency", Variant::I32(0))]);
    h.notify_full("Chat", 0, "odd hints", &[], &bad)
        .await;

    let state = h.state.borrow();
    assert_eq!(
        state.history[1].image,
        Some(Image::Path("file:///tmp/avatar.png".into()))
    );
    assert_eq!(state.history[0].image, None);
    assert_eq!(
        state.history[0].urgency,
        Urgency::Low,
        "a wrong hint type never fails Notify"
    );
}

#[tokio::test]
async fn a_second_server_waits_for_the_name() {
    let h = Harness::start().await;
    let first_owner = h
        .server
        .unique_name()
        .unwrap()
        .to_string();

    let second = h.bus.connect().await;
    let (waiting, _state, _effects) = Service::start(&second, Config::default())
        .await
        .expect("a second server waits instead of failing");

    let dbus = DBusProxy::new(&h.client).await.unwrap();
    let owner = dbus
        .get_name_owner(BUS_NAME.try_into().unwrap())
        .await
        .unwrap();
    assert_eq!(
        owner.as_str(),
        first_owner,
        "the first server keeps the name"
    );

    let third = h.bus.connect().await;
    let replace = RequestNameFlags::REPLACE_EXISTING | RequestNameFlags::DO_NOT_QUEUE;
    let taken = third
        .request_name_with_flags(BUS_NAME, replace)
        .await;
    assert_matches!(
        taken,
        Err(zbus::Error::NameTaken),
        "the first server allows no replacement"
    );

    assert_eq!(h.notify("Chat", "still here").await, 1);

    let queued = dbus
        .list_queued_owners(BUS_NAME.try_into().unwrap())
        .await
        .unwrap();
    let second_name = second
        .unique_name()
        .unwrap()
        .to_string();
    assert!(
        queued
            .iter()
            .any(|name| name.as_str() == second_name)
    );
    within(waiting.shutdown()).await;
    let queued = dbus
        .list_queued_owners(BUS_NAME.try_into().unwrap())
        .await
        .unwrap();
    assert!(
        queued
            .iter()
            .all(|name| name.as_str() != second_name),
        "a shutdown leaves the queue"
    );
}

#[tokio::test]
async fn takes_the_name_when_the_owner_leaves() {
    let bus = PrivateBus::start();
    let holder = bus.connect().await;
    holder
        .request_name(BUS_NAME)
        .await
        .unwrap();
    let holder_name = holder
        .unique_name()
        .unwrap()
        .to_string();

    let server = bus.connect().await;
    let (service, state, _effects) = within(Service::start(&server, Config::default()))
        .await
        .expect("start waits for the name instead of failing");
    let server_name = server
        .unique_name()
        .unwrap()
        .to_string();

    let client = bus.connect().await;
    let dbus = DBusProxy::new(&client).await.unwrap();
    let owner = || async {
        dbus.get_name_owner(BUS_NAME.try_into().unwrap())
            .await
            .unwrap()
            .to_string()
    };
    assert_eq!(owner().await, holder_name);
    let mut changes = dbus
        .receive_name_owner_changed_with_args(&[(0, BUS_NAME)])
        .await
        .unwrap();

    within(holder.close()).await.unwrap();
    let change = within(poll_fn(|cx| Pin::new(&mut changes).poll_next(cx)))
        .await
        .expect("an owner change");
    let args = change.args().unwrap();
    assert_eq!(
        args.new_owner()
            .as_ref()
            .map(|name| name.as_str()),
        Some(server_name.as_str())
    );
    assert_eq!(owner().await, server_name);

    let proxy = common::NotificationsProxy::new(&client)
        .await
        .unwrap();
    let id = proxy
        .notify(
            "Chat",
            0,
            "",
            "after the handover",
            "body",
            &[],
            &no_hints(),
            -1,
        )
        .await
        .unwrap();
    assert_eq!(id, 1);
    assert_eq!(
        state.borrow().history[0].summary,
        "after the handover"
    );
    within(service.shutdown()).await;
}

#[tokio::test]
async fn shutdown_gives_up_the_name() {
    let bus = PrivateBus::start();
    let h = Harness::on_bus(bus, Config::default()).await;
    within(h.service.shutdown()).await;
    let next = h.bus.connect().await;
    let (service, _state, _effects) = Service::start(&next, Config::default())
        .await
        .unwrap();
    within(service.shutdown()).await;
}

#[tokio::test]
async fn invoke_starts_the_focus_grant() {
    let mut h = Harness::start().await;
    let hints = HashMap::from([("desktop-entry", Variant::from("org.example.Chat"))]);
    let id = h
        .notify_full("Chat Client", 0, "ping", &["default", ""], &hints)
        .await;
    h.service
        .send(Command::Urgent { address: 0xabc, class: "org.example.Chat".into() });
    h.service
        .send(Command::Invoke(h.uid(id), "default".into()));
    assert_eq!(
        h.signals.next().await,
        Sig::Invoked(id, "default".into())
    );
    assert!(
        h.effects.try_recv().is_err(),
        "urgent before the invoke has no grant"
    );

    h.service
        .send(Command::Urgent { address: 0xabc, class: "browser".into() });
    h.service
        .send(Command::Urgent { address: 0xabc, class: "org.example.Chat".into() });
    let effects: Vec<Effect> = vec![
        within(h.effects.recv()).await.unwrap(),
        within(h.effects.recv()).await.unwrap(),
        within(h.effects.recv()).await.unwrap(),
    ];
    assert_eq!(
        effects[0],
        Effect::Dispatch(r#"hl.dsp.focus({ window = "address:0xabc" })"#.into())
    );
    assert_eq!(
        effects[1],
        Effect::Dispatch(r#"hl.dsp.layout("fit_into_view")"#.into())
    );
    let Effect::Schedule { timer, after } = effects[2] else { panic!("expected a timer") };
    assert_eq!(
        (timer.kind, timer.address, after),
        (
            TimerKind::Settle,
            0xabc,
            Duration::from_millis(250)
        )
    );

    h.service
        .send(Command::FocusTimer { timer, window: None, monitors: Vec::new() });
    assert_eq!(
        within(h.effects.recv()).await.unwrap(),
        Effect::RefreshWindows
    );
    let Effect::Schedule { timer, .. } = within(h.effects.recv()).await.unwrap() else {
        panic!("expected a timer")
    };
    assert_eq!(timer.kind, TimerKind::Move);
    let window = Rect { x: 0, y: 0, width: 100, height: 100 };
    let monitor = Rect { x: 0, y: 0, width: 1920, height: 1080 };
    h.service
        .send(Command::FocusTimer { timer, window: Some(window), monitors: vec![monitor] });
    assert_eq!(
        within(h.effects.recv()).await.unwrap(),
        Effect::Dispatch("hl.dsp.cursor.move({ x = 50, y = 50 })".into())
    );
}
