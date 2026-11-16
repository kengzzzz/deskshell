//! The StatusNotifierHost: follows the watcher, loads items and keeps the open menu.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, MissedTickBehavior};
use tracing::{debug, warn};
use zbus::fdo::RequestNameFlags;
use zbus::message::Type as MessageType;
use zbus::names::BusName;
use zbus::{Connection, Message, ObjectPath, OwnedMatchRule, Variant};

use crate::bus::{
    Calls, DBUS_NAME, DBUS_PATH, DEFAULT_ITEM_PATH, Dest, ForwardTask, ITEM_INTERFACE,
    MENU_INTERFACE, PROPERTIES, WATCHER_NAME, WATCHER_PATH, error_text, forward, host_rules,
    owner_change, unknown_method,
};
use crate::menu;
use crate::model::{
    Category, ClickAction, Command, Icon, Item, Menu, Pixmap, State, Status, Tooltip, WatcherState,
};
use crate::pixmap::{RawPixmap, convert};
use crate::service::Error;
use crate::view::{click_action, insert_sorted};
use crate::wire::{ChangedInterface, ItemProps, Layout, MenuProps, Prop, Typed};

#[derive(Debug, Clone, Copy)]
enum Then {
    WatcherOwner,
    RegisterHost { watcher: u64 },
    WatcherItems { watcher: u64 },
    Owner { item: u64 },
    GetAll { item: u64 },
    Get { item: u64, prop: Prop },
    ItemCall { item: u64, method: &'static str },
    MenuProps { menu: u64 },
    AboutToShow { menu: u64, id: i32 },
    Layout { menu: u64, parent: i32, opens: Option<i32> },
    Event { menu: u64, event: &'static str },
}

impl Then {
    fn watcher(self) -> Option<u64> {
        match self {
            Self::RegisterHost { watcher } | Self::WatcherItems { watcher } => Some(watcher),
            _ => None,
        }
    }

    fn item(self) -> Option<u64> {
        match self {
            Self::Owner { item }
            | Self::GetAll { item }
            | Self::Get { item, .. }
            | Self::ItemCall { item, .. } => Some(item),
            _ => None,
        }
    }

    fn menu(self) -> Option<u64> {
        match self {
            Self::MenuProps { menu }
            | Self::AboutToShow { menu, .. }
            | Self::Layout { menu, .. }
            | Self::Event { menu, .. } => Some(menu),
            _ => None,
        }
    }
}

#[derive(Default)]
struct Raw {
    theme_path: String,
    theme_path_seen: bool,
    icon_name: String,
    icon_pixmaps: Arc<[Pixmap]>,
    overlay_name: String,
    overlay_pixmaps: Arc<[Pixmap]>,
    attention_name: String,
    attention_pixmaps: Arc<[Pixmap]>,
    tooltip_name: String,
    tooltip_pixmaps: Arc<[Pixmap]>,
}

struct Tracked {
    id: u64,
    address: String,
    path: String,
    owner: Option<Arc<str>>,
    key: String,
    ready: bool,
    raw: Raw,
}

struct Session {
    id: u64,
    item: u64,
    key: String,
    owner: Arc<str>,
    path: String,
    icon_theme_path: Vec<String>,
    opened: bool,
    open: Vec<(i32, bool)>,
}

pub(crate) struct Host {
    calls: Calls<Then>,
    rules: Vec<OwnedMatchRule>,
    me: Arc<str>,
    name: String,
    state: watch::Sender<State>,
    watcher: Option<Arc<str>>,
    watcher_gen: u64,
    items: Vec<Tracked>,
    next_item: u64,
    menu: Option<Session>,
    next_menu: u64,
    messages: mpsc::UnboundedReceiver<Message>,
    forwarder: Option<ForwardTask>,
}

fn host_name() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!(
        "org.kde.StatusNotifierHost-{}-{n}",
        std::process::id()
    )
}

fn set<T: PartialEq>(slot: &mut T, value: T) -> bool {
    if *slot == value {
        false
    } else {
        *slot = value;
        true
    }
}

fn modify<T: Clone>(slot: &mut Arc<T>, change: impl FnOnce(&mut T) -> bool) -> bool {
    if let Some(value) = Arc::get_mut(slot) {
        return change(value);
    }
    let mut copy = T::clone(slot);
    let changed = change(&mut copy);
    if changed {
        *slot = Arc::new(copy);
    }
    changed
}

fn set_str(slot: &mut String, value: &str) -> bool {
    if slot == value {
        false
    } else {
        value.clone_into(slot);
        true
    }
}

fn set_pixmaps(slot: &mut Arc<[Pixmap]>, raw: &[RawPixmap<'_>]) -> bool {
    let pixmaps = convert(raw);
    if **slot == *pixmaps {
        false
    } else {
        *slot = pixmaps;
        true
    }
}

fn set_icon(slot: &mut Icon, name: &str, pixmaps: &Arc<[Pixmap]>, theme_path: &str) -> bool {
    let same = match slot {
        Icon::Name { name: have, theme_path: have_path } => {
            !name.is_empty() && have == name && have_path == theme_path
        }
        Icon::Pixmaps(have) => name.is_empty() && Arc::ptr_eq(have, pixmaps),
        Icon::None => name.is_empty() && pixmaps.is_empty(),
    };
    if same {
        return false;
    }
    *slot = if !name.is_empty() {
        Icon::Name { name: name.to_owned(), theme_path: theme_path.to_owned() }
    } else if !pixmaps.is_empty() {
        Icon::Pixmaps(Arc::clone(pixmaps))
    } else {
        Icon::None
    };
    true
}

// Apps send `/NO_DBUSMENU` for no menu.
fn menu_path(path: &ObjectPath<'_>) -> Option<String> {
    let path = path.as_str();
    (!path.is_empty() && path != "/NO_DBUSMENU").then(|| path.to_owned())
}

fn apply(raw: &mut Raw, item: &mut Item, props: &ItemProps<'_>) -> bool {
    let mut changed = false;
    if let Some(id) = props.id {
        changed |= set_str(&mut item.id, id);
    }
    if let Some(title) = props.title {
        changed |= set_str(&mut item.title, title);
    }
    if let Some(text) = props.status {
        match Status::parse(text) {
            Some(status) => changed |= set(&mut item.status, status),
            None => debug!(
                item = item.key,
                text, "ignoring a nonconformant status"
            ),
        }
    }
    if let Some(text) = props.category {
        match Category::parse(text) {
            Some(category) => changed |= set(&mut item.category, category),
            None => debug!(
                item = item.key,
                text, "ignoring a nonconformant category"
            ),
        }
    }
    let mut icons = false;
    raw.theme_path_seen |= props.icon_theme_path_seen;
    if let Some(path) = props.icon_theme_path {
        icons |= set_str(&mut raw.theme_path, path);
    }
    if let Some(name) = props.icon_name {
        icons |= set_str(&mut raw.icon_name, name);
    }
    if let Some(pixmaps) = &props.icon_pixmap {
        icons |= set_pixmaps(&mut raw.icon_pixmaps, pixmaps);
    }
    if let Some(name) = props.overlay_icon_name {
        icons |= set_str(&mut raw.overlay_name, name);
    }
    if let Some(pixmaps) = &props.overlay_icon_pixmap {
        icons |= set_pixmaps(&mut raw.overlay_pixmaps, pixmaps);
    }
    if let Some(name) = props.attention_icon_name {
        icons |= set_str(&mut raw.attention_name, name);
    }
    if let Some(pixmaps) = &props.attention_icon_pixmap {
        icons |= set_pixmaps(&mut raw.attention_pixmaps, pixmaps);
    }
    if let Some((name, pixmaps, title, description)) = &props.tool_tip {
        icons |= set_str(&mut raw.tooltip_name, name);
        icons |= set_pixmaps(&mut raw.tooltip_pixmaps, pixmaps);
        changed |= set_str(&mut item.tooltip.title, title);
        changed |= set_str(&mut item.tooltip.description, description);
    }
    if let Some(path) = &props.menu {
        changed |= set(&mut item.menu, menu_path(path));
    }
    if let Some(only_menu) = props.item_is_menu {
        changed |= set(&mut item.item_is_menu, only_menu);
    }
    if icons {
        let theme = &raw.theme_path;
        changed |= set_icon(
            &mut item.icon,
            &raw.icon_name,
            &raw.icon_pixmaps,
            theme,
        );
        changed |= set_icon(
            &mut item.overlay,
            &raw.overlay_name,
            &raw.overlay_pixmaps,
            theme,
        );
        changed |= set_icon(
            &mut item.attention,
            &raw.attention_name,
            &raw.attention_pixmaps,
            theme,
        );
        changed |= set_icon(
            &mut item.tooltip.icon,
            &raw.tooltip_name,
            &raw.tooltip_pixmaps,
            theme,
        );
    }
    changed
}

fn new_item(key: &str) -> Item {
    Item {
        key: key.to_owned(),
        id: String::new(),
        title: String::new(),
        status: Status::default(),
        category: Category::default(),
        icon: Icon::None,
        attention: Icon::None,
        overlay: Icon::None,
        tooltip: Tooltip::default(),
        item_is_menu: false,
        menu: None,
    }
}

// Seconds since the epoch, cut to 32 bits as Qt hosts send it.
fn timestamp() -> u32 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    u32::try_from(secs & u64::from(u32::MAX)).unwrap_or(u32::MAX)
}

fn strings(value: &Variant<'_>) -> Option<Vec<String>> {
    match value {
        Variant::Variant(inner) => strings(inner),
        Variant::Array(array) => Some(
            array
                .inner()
                .iter()
                .filter_map(|v| match v {
                    Variant::Str(s) => Some(s.as_str().to_owned()),
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    }
}

async fn send_event(calls: &mut Calls<Then>, session: &Session, id: i32, event: &'static str) {
    let body = (id, event, Variant::I32(0), timestamp());
    calls
        .call(
            Dest::Peer(Arc::clone(&session.owner)),
            &session.path,
            MENU_INTERFACE,
            "Event",
            &body,
            Then::Event { menu: session.id, event },
        )
        .await;
}

async fn get_layout(calls: &mut Calls<Then>, session: &Session, parent: i32, opens: Option<i32>) {
    let no_names: &[&str] = &[];
    calls
        .call(
            Dest::Peer(Arc::clone(&session.owner)),
            &session.path,
            MENU_INTERFACE,
            "GetLayout",
            &(parent, -1i32, no_names),
            Then::Layout { menu: session.id, parent, opens },
        )
        .await;
}

impl Host {
    pub(crate) async fn start(
        conn: &Connection,
        timeout: Duration,
        state: watch::Sender<State>,
    ) -> Result<Self, Error> {
        let me: Arc<str> = conn
            .unique_name()
            .map(|name| Arc::from(name.as_str()))
            .ok_or(Error::NotBus)?;
        let (messages, forwarder) = forward(conn);
        let rules = host_rules()?;
        let proxy = zbus::fdo::DBusProxy::new(conn).await?;
        let mut added = Vec::with_capacity(rules.len());
        for rule in rules {
            if let Err(error) = proxy
                .add_match_rule(rule.clone().into_inner())
                .await
            {
                remove_rules(conn, &added).await;
                return Err(zbus::Error::from(error).into());
            }
            added.push(rule);
        }
        let name = host_name();
        if let Err(error) = conn
            .request_name_with_flags(name.as_str(), RequestNameFlags::DO_NOT_QUEUE)
            .await
        {
            remove_rules(conn, &added).await;
            return Err(Error::HostName(name, error));
        }
        let mut host = Self {
            calls: Calls::new(conn.clone(), timeout),
            rules: added,
            me,
            name,
            state,
            watcher: None,
            watcher_gen: 0,
            items: Vec::new(),
            next_item: 0,
            menu: None,
            next_menu: 0,
            messages,
            forwarder: Some(forwarder),
        };
        host.calls
            .call(
                Dest::Bus,
                DBUS_PATH,
                DBUS_NAME,
                "GetNameOwner",
                &(WATCHER_NAME,),
                Then::WatcherOwner,
            )
            .await;
        Ok(host)
    }

    pub(crate) async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<Command>,
        mut stop: oneshot::Receiver<()>,
    ) {
        let period = (self.calls.timeout() / 4).max(Duration::from_millis(10));
        let mut sweep = tokio::time::interval(period);
        sweep.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                _ = &mut stop => break,
                Some(command) = commands.recv() => self.command(command).await,
                Some(message) = self.messages.recv() => self.message(&message).await,
                _ = sweep.tick() => self.sweep().await,
            }
        }
        self.close_menu(true).await;
        self.forwarder = None;
        let conn = self.calls.conn().clone();
        if let Err(error) = conn
            .release_name(self.name.as_str())
            .await
        {
            warn!(%error, name = self.name, "cannot release the host name");
        }
        remove_rules(&conn, &self.rules).await;
    }

    async fn sweep(&mut self) {
        for then in self.calls.expired(Instant::now()) {
            let error = zbus::Error::Failure("no reply in time".to_owned());
            self.reply(then, Err(error)).await;
        }
    }

    fn tracked(&self, id: u64) -> Option<usize> {
        self.items
            .iter()
            .position(|t| t.id == id)
    }

    fn ready_by_key(&self, key: &str) -> Option<usize> {
        self.items
            .iter()
            .position(|t| t.ready && t.key == key)
    }

    fn ready_at(&self, sender: &str, path: &str) -> Option<usize> {
        self.items
            .iter()
            .position(|t| t.ready && t.path == path && t.owner.as_deref() == Some(sender))
    }

    async fn message(&mut self, message: &Message) {
        match message.message_type() {
            MessageType::MethodReturn | MessageType::Error => {
                if let Some((then, result)) = self.calls.reply(message) {
                    self.reply(then, result).await;
                }
            }
            MessageType::Signal => self.signal(message).await,
            MessageType::MethodCall => {}
        }
    }

    async fn signal(&mut self, message: &Message) {
        let header = message.header();
        let (Some(interface), Some(member), Some(sender)) = (
            header.interface(),
            header.member(),
            header.sender(),
        ) else {
            return;
        };
        let path = header.path().map_or("", |p| p.as_str());
        let sender = sender.as_str();
        match interface.as_str() {
            DBUS_NAME => {
                let body = message.body();
                if let Some((name, new)) = owner_change(message, &body) {
                    self.owner_changed(name, new).await;
                }
            }
            WATCHER_NAME if self.watcher.as_deref() == Some(sender) && path == WATCHER_PATH => {
                let body = message.body();
                let Ok(address) = body.deserialize::<&str>() else {
                    return;
                };
                match member.as_str() {
                    "StatusNotifierItemRegistered" => self.add_address(address).await,
                    "StatusNotifierItemUnregistered" => {
                        if let Some(i) = self
                            .items
                            .iter()
                            .position(|t| t.address == address)
                        {
                            self.remove_tracked(i);
                        }
                    }
                    _ => {}
                }
            }
            ITEM_INTERFACE => {
                if let Some(i) = self.ready_at(sender, path) {
                    self.item_signal(i, member.as_str(), message)
                        .await;
                }
            }
            MENU_INTERFACE => {
                if self
                    .menu
                    .as_ref()
                    .is_some_and(|s| &*s.owner == sender && s.path == path)
                {
                    self.menu_signal(member.as_str(), message)
                        .await;
                }
            }
            PROPERTIES if member.as_str() == "PropertiesChanged" => {
                self.properties_changed(sender, path, message)
                    .await;
            }
            _ => {}
        }
    }

    async fn reply(&mut self, then: Then, result: zbus::Result<Message>) {
        match then {
            Then::WatcherOwner => {
                let owner = match &result {
                    Ok(reply) => reply
                        .body()
                        .deserialize::<&str>()
                        .ok()
                        .map(str::to_owned),
                    Err(_) => None,
                };
                self.set_watcher(owner.as_deref()).await;
            }
            Then::RegisterHost { .. } => {
                if let Err(error) = result {
                    warn!(
                        error = error_text(&error),
                        "the watcher refused the host"
                    );
                }
            }
            Then::WatcherItems { watcher } => {
                if watcher != self.watcher_gen {
                    return;
                }
                match result {
                    Ok(reply) => {
                        let body = reply.body();
                        match body.deserialize::<Typed<Vec<&str>>>() {
                            Ok(Typed(Some(addresses))) => {
                                for address in addresses {
                                    self.add_address(address).await;
                                }
                            }
                            _ => warn!("the watcher's RegisteredStatusNotifierItems is not a list"),
                        }
                    }
                    Err(error) => {
                        warn!(
                            error = error_text(&error),
                            "cannot read the watcher's items"
                        );
                    }
                }
            }
            Then::Owner { item } => match result {
                Ok(reply) => match reply.body().deserialize::<&str>() {
                    Ok(owner) => self.resolved(item, owner).await,
                    Err(_) => self.forget_tracked(item),
                },
                Err(error) => {
                    debug!(
                        error = error_text(&error),
                        "an item's name has no owner"
                    );
                    self.forget_tracked(item);
                }
            },
            Then::GetAll { item } => self.loaded(item, result),
            Then::Get { item, prop } => {
                let Ok(reply) = result else {
                    return;
                };
                let body = reply.body();
                match ItemProps::from_get(prop, &body) {
                    Ok(props) => self.update(item, &props).await,
                    Err(error) => debug!(%error, prop = prop.name(), "cannot read a property"),
                }
            }
            Then::ItemCall { method, .. } => match result {
                Err(error) if unknown_method(&error) => {
                    debug!(method, "the item has no such method");
                }
                Err(error) => warn!(
                    method,
                    error = error_text(&error),
                    "an item call failed"
                ),
                Ok(_) => {}
            },
            Then::MenuProps { menu } => {
                if let Ok(reply) = result {
                    let body = reply.body();
                    if let Ok(props) = body.deserialize::<HashMap<&str, Variant<'_>>>()
                        && let Some(path) = props
                            .get("IconThemePath")
                            .and_then(strings)
                    {
                        self.set_menu_theme_path(menu, path);
                    }
                }
            }
            Then::AboutToShow { menu, id } => {
                self.about_to_show(menu, id, result)
                    .await
            }
            Then::Layout { menu, parent, opens } => {
                self.layout(menu, parent, opens, result)
                    .await;
            }
            Then::Event { event, .. } => {
                if let Err(error) = result {
                    warn!(
                        event,
                        error = error_text(&error),
                        "a menu event failed"
                    );
                }
            }
        }
    }

    async fn owner_changed(&mut self, name: &str, new: Option<&str>) {
        if name == WATCHER_NAME {
            self.set_watcher(new).await;
            return;
        }
        if new.is_none() && name.starts_with(':') {
            while let Some(i) = self
                .items
                .iter()
                .position(|t| t.owner.as_deref() == Some(name))
            {
                self.remove_tracked(i);
            }
        }
    }

    async fn set_watcher(&mut self, owner: Option<&str>) {
        if self.watcher.as_deref() == owner {
            return;
        }
        self.drop_items();
        self.watcher_gen += 1;
        self.calls
            .forget(|then| then.watcher().is_some());
        self.watcher = owner.map(Arc::from);
        let watcher = match owner {
            None => WatcherState::Absent,
            Some(owner) if owner == &*self.me => WatcherState::Own,
            Some(_) => WatcherState::Foreign,
        };
        self.state
            .send_if_modified(|state| set(&mut state.watcher, watcher));
        let Some(owner) = self.watcher.clone() else {
            return;
        };
        debug!(owner = &*owner, "registering with the watcher");
        let generation = self.watcher_gen;
        self.calls
            .call(
                Dest::Peer(Arc::clone(&owner)),
                WATCHER_PATH,
                WATCHER_NAME,
                "RegisterStatusNotifierHost",
                &(self.name.as_str(),),
                Then::RegisterHost { watcher: generation },
            )
            .await;
        self.calls
            .call(
                Dest::Peer(owner),
                WATCHER_PATH,
                PROPERTIES,
                "Get",
                &(WATCHER_NAME, "RegisteredStatusNotifierItems"),
                Then::WatcherItems { watcher: generation },
            )
            .await;
    }

    fn drop_items(&mut self) {
        self.menu = None;
        self.items.clear();
        self.calls
            .forget(|then| then.item().is_some() || then.menu().is_some());
        self.state.send_if_modified(|state| {
            let changed = !state.items.is_empty() || state.menu.is_some();
            state.items.clear();
            state.menu = None;
            changed
        });
    }

    async fn add_address(&mut self, address: &str) {
        if self
            .items
            .iter()
            .any(|t| t.address == address)
        {
            debug!(address, "ignoring a duplicate item");
            return;
        }
        let (service, path) = match address.find('/') {
            Some(at) => address.split_at(at),
            None => (address, DEFAULT_ITEM_PATH),
        };
        if BusName::try_from(service).is_err() || ObjectPath::try_from(path).is_err() {
            warn!(address, "ignoring an item with a bad address");
            return;
        }
        self.next_item += 1;
        let id = self.next_item;
        self.items.push(Tracked {
            id,
            address: address.to_owned(),
            path: path.to_owned(),
            owner: None,
            key: String::new(),
            ready: false,
            raw: Raw::default(),
        });
        if service.starts_with(':') {
            self.resolved(id, service).await;
        } else {
            self.calls
                .call(
                    Dest::Bus,
                    DBUS_PATH,
                    DBUS_NAME,
                    "GetNameOwner",
                    &(service,),
                    Then::Owner { item: id },
                )
                .await;
        }
    }

    async fn resolved(&mut self, id: u64, owner: &str) {
        let Some(i) = self.tracked(id) else {
            return;
        };
        let key = format!("{owner}{}", self.items[i].path);
        if self
            .items
            .iter()
            .any(|t| t.id != id && t.key == key)
        {
            debug!(key, "the watcher lists one item twice");
            self.items.remove(i);
            return;
        }
        let owner: Arc<str> = Arc::from(owner);
        let Self { items, calls, .. } = self;
        let tracked = &mut items[i];
        tracked.owner = Some(Arc::clone(&owner));
        tracked.key = key;
        calls
            .call(
                Dest::Peer(owner),
                &tracked.path,
                PROPERTIES,
                "GetAll",
                &(ITEM_INTERFACE,),
                Then::GetAll { item: id },
            )
            .await;
    }

    fn loaded(&mut self, id: u64, result: zbus::Result<Message>) {
        let Some(i) = self.tracked(id) else {
            return;
        };
        let tracked = &mut self.items[i];
        if tracked.ready {
            return;
        }
        let reply = match result {
            Ok(reply) => reply,
            Err(error) => {
                warn!(
                    item = tracked.key,
                    error = error_text(&error),
                    "cannot load a tray item"
                );
                return;
            }
        };
        let body = reply.body();
        let props = match body.deserialize::<ItemProps<'_>>() {
            Ok(props) => props,
            Err(error) => {
                warn!(item = tracked.key, %error, "cannot read a tray item's properties");
                return;
            }
        };
        let mut item = new_item(&tracked.key);
        apply(&mut tracked.raw, &mut item, &props);
        tracked.ready = true;
        debug!(
            item = tracked.key,
            id = item.id,
            "tray item ready"
        );
        self.state
            .send_modify(|state| insert_sorted(&mut state.items, Arc::new(item)));
    }

    async fn update(&mut self, id: u64, props: &ItemProps<'_>) {
        let Some(i) = self.tracked(id) else {
            return;
        };
        let tracked = &mut self.items[i];
        if !tracked.ready {
            return;
        }
        let open_path = self
            .menu
            .as_ref()
            .filter(|session| session.item == id)
            .map(|session| session.path.as_str());
        let mut moved = false;
        self.state.send_if_modified(|state| {
            let Some(slot) = state
                .items
                .iter_mut()
                .find(|item| item.key == tracked.key)
            else {
                return false;
            };
            modify(slot, |item| {
                let changed = apply(&mut tracked.raw, item, props);
                moved = open_path.is_some_and(|path| item.menu.as_deref() != Some(path));
                changed
            })
        });
        if moved {
            debug!("the item's menu moved, closing it");
            self.close_menu(true).await;
        }
    }

    fn forget_tracked(&mut self, id: u64) {
        if let Some(i) = self.tracked(id) {
            self.remove_tracked(i);
        }
    }

    fn remove_tracked(&mut self, i: usize) {
        let tracked = self.items.remove(i);
        let id = tracked.id;
        self.calls
            .forget(|then| then.item() == Some(id));
        let menu = self
            .menu
            .take_if(|session| session.item == id);
        if let Some(session) = &menu {
            let menu_id = session.id;
            self.calls
                .forget(|then| then.menu() == Some(menu_id));
        }
        if !tracked.ready && menu.is_none() {
            return;
        }
        debug!(item = tracked.key, "tray item gone");
        self.state.send_if_modified(|state| {
            let before = state.items.len();
            state
                .items
                .retain(|item| item.key != tracked.key);
            let mut changed = state.items.len() != before;
            if menu.is_some() {
                changed |= state.menu.take().is_some();
            }
            changed
        });
    }

    async fn get(&mut self, i: usize, prop: Prop) {
        let Self { items, calls, .. } = self;
        let tracked = &items[i];
        let Some(owner) = &tracked.owner else {
            return;
        };
        calls
            .call(
                Dest::Peer(Arc::clone(owner)),
                &tracked.path,
                PROPERTIES,
                "Get",
                &(ITEM_INTERFACE, prop.name()),
                Then::Get { item: tracked.id, prop },
            )
            .await;
    }

    // `NewStatus` takes the status from the signal, so an app that signals before it changes the
    // property still shows the new status.
    async fn item_signal(&mut self, i: usize, member: &str, message: &Message) {
        let props: &[Prop] = match member {
            "NewTitle" => &[Prop::Title],
            "NewIcon" => &[Prop::IconName, Prop::IconPixmap],
            "NewOverlayIcon" => &[Prop::OverlayIconName, Prop::OverlayIconPixmap],
            "NewAttentionIcon" => &[Prop::AttentionIconName, Prop::AttentionIconPixmap],
            "NewToolTip" => &[Prop::ToolTip],
            "NewMenu" => &[Prop::Menu],
            "NewStatus" => {
                let body = message.body();
                let Ok(status) = body.deserialize::<&str>() else {
                    return;
                };
                let props = ItemProps { status: Some(status), ..ItemProps::default() };
                let id = self.items[i].id;
                self.update(id, &props).await;
                return;
            }
            _ => return,
        };
        let theme = matches!(
            member,
            "NewIcon" | "NewOverlayIcon" | "NewAttentionIcon"
        ) && self.items[i].raw.theme_path_seen;
        for &prop in props {
            self.get(i, prop).await;
        }
        if theme {
            self.get(i, Prop::IconThemePath).await;
        }
    }

    async fn properties_changed(&mut self, sender: &str, path: &str, message: &Message) {
        let body = message.body();
        let Ok((interface, _, _)) = body.deserialize::<ChangedInterface<'_>>() else {
            return;
        };
        match interface {
            ITEM_INTERFACE => {
                let Some(i) = self.ready_at(sender, path) else {
                    return;
                };
                let Ok((_, changed, invalidated)) =
                    body.deserialize::<(&str, ItemProps<'_>, Vec<&str>)>()
                else {
                    return;
                };
                let id = self.items[i].id;
                self.update(id, &changed).await;
                for name in invalidated {
                    if let (Some(i), Some(prop)) = (self.tracked(id), Prop::from_name(name)) {
                        self.get(i, prop).await;
                    }
                }
            }
            MENU_INTERFACE => {
                let Some(session) = self
                    .menu
                    .as_ref()
                    .filter(|s| &*s.owner == sender && s.path == path)
                else {
                    return;
                };
                let menu = session.id;
                let Ok((_, changed, _)) =
                    body.deserialize::<(&str, HashMap<&str, Variant<'_>>, Vec<&str>)>()
                else {
                    return;
                };
                if let Some(path) = changed
                    .get("IconThemePath")
                    .and_then(strings)
                {
                    self.set_menu_theme_path(menu, path);
                }
            }
            _ => {}
        }
    }

    async fn command(&mut self, command: Command) {
        match command {
            Command::Click { item, button, x, y } => {
                let action = {
                    let state = self.state.borrow();
                    state
                        .items
                        .iter()
                        .find(|it| it.key == item)
                        .map(|it| (click_action(it, button), it.menu.is_some()))
                };
                match action {
                    Some((ClickAction::Activate, _)) => {
                        self.item_call(&item, "Activate", &(0i32, 0i32))
                            .await;
                    }
                    Some((ClickAction::SecondaryActivate, _)) => {
                        self.item_call(&item, "SecondaryActivate", &(0i32, 0i32))
                            .await;
                    }
                    Some((ClickAction::Menu, true)) => self.open_menu(&item).await,
                    Some((ClickAction::Menu, false)) => {
                        self.item_call(&item, "ContextMenu", &(x, y))
                            .await;
                    }
                    None => {}
                }
            }
            Command::Activate { item, x, y } => {
                self.item_call(&item, "Activate", &(x, y))
                    .await
            }
            Command::SecondaryActivate { item, x, y } => {
                self.item_call(&item, "SecondaryActivate", &(x, y))
                    .await;
            }
            Command::ContextMenu { item, x, y } => {
                self.item_call(&item, "ContextMenu", &(x, y))
                    .await;
            }
            Command::Scroll { item, delta, orientation } => {
                self.item_call(&item, "Scroll", &(delta, orientation.as_str()))
                    .await;
            }
            Command::OpenMenu { item } => self.open_menu(&item).await,
            Command::OpenSubmenu(id) => self.open_submenu(id).await,
            Command::CloseSubmenu(id) => self.close_submenu(id).await,
            Command::ClickEntry(id) => self.click_entry(id).await,
            Command::CloseMenu => self.close_menu(true).await,
        }
    }

    async fn item_call<B>(&mut self, key: &str, method: &'static str, body: &B)
    where
        B: serde::Serialize + zbus::DynamicType,
    {
        let Some(i) = self.ready_by_key(key) else {
            return;
        };
        let Self { items, calls, .. } = self;
        let tracked = &items[i];
        let Some(owner) = &tracked.owner else {
            return;
        };
        calls
            .call(
                Dest::Peer(Arc::clone(owner)),
                &tracked.path,
                ITEM_INTERFACE,
                method,
                body,
                Then::ItemCall { item: tracked.id, method },
            )
            .await;
    }

    // `GetLayout` goes out after `AboutToShow(0)` whatever its answer.
    async fn open_menu(&mut self, key: &str) {
        let Some(i) = self.ready_by_key(key) else {
            return;
        };
        let path = {
            let state = self.state.borrow();
            state
                .items
                .iter()
                .find(|item| item.key == key)
                .and_then(|item| item.menu.clone())
        };
        let (Some(path), Some(owner)) = (path, self.items[i].owner.clone()) else {
            return;
        };
        self.close_menu(true).await;
        self.next_menu += 1;
        let session = Session {
            id: self.next_menu,
            item: self.items[i].id,
            key: key.to_owned(),
            owner,
            path,
            icon_theme_path: Vec::new(),
            opened: false,
            open: Vec::new(),
        };
        let dest = || Dest::Peer(Arc::clone(&session.owner));
        self.calls
            .call(
                dest(),
                &session.path,
                PROPERTIES,
                "GetAll",
                &(MENU_INTERFACE,),
                Then::MenuProps { menu: session.id },
            )
            .await;
        self.calls
            .call(
                dest(),
                &session.path,
                MENU_INTERFACE,
                "AboutToShow",
                &(0i32,),
                Then::AboutToShow { menu: session.id, id: 0 },
            )
            .await;
        self.menu = Some(session);
    }

    fn session(&self, menu: u64) -> Option<&Session> {
        self.menu
            .as_ref()
            .filter(|session| session.id == menu)
    }

    async fn about_to_show(&mut self, menu: u64, id: i32, result: zbus::Result<Message>) {
        let Self { menu: session, calls, .. } = self;
        let Some(session) = session
            .as_ref()
            .filter(|session| session.id == menu)
        else {
            return;
        };
        if id == 0 {
            if let Err(error) = &result {
                debug!(
                    error = error_text(error),
                    "AboutToShow failed, showing the menu anyway"
                );
            }
            get_layout(calls, session, 0, Some(0)).await;
            return;
        }
        if !session
            .open
            .iter()
            .any(|&(open, _)| open == id)
        {
            return;
        }
        let refetch = match &result {
            Ok(reply) => reply
                .body()
                .deserialize::<bool>()
                .unwrap_or(true),
            Err(error) => {
                debug!(
                    error = error_text(error),
                    "AboutToShow failed, refetching anyway"
                );
                true
            }
        };
        if refetch {
            get_layout(calls, session, id, Some(id)).await;
        } else {
            self.send_opened(menu, id).await;
        }
    }

    async fn layout(
        &mut self,
        menu: u64,
        parent: i32,
        opens: Option<i32>,
        result: zbus::Result<Message>,
    ) {
        if self.session(menu).is_none() {
            return;
        }
        let error = match &result {
            Ok(reply) => {
                let body = reply.body();
                match body.deserialize::<(u32, Layout<'_>)>() {
                    Ok((_, layout)) => {
                        self.apply_layout(parent, &layout);
                        None
                    }
                    Err(error) => Some(error_text(&error)),
                }
            }
            Err(error) => Some(error_text(error)),
        };
        if let Some(error) = error {
            warn!(parent, error, "cannot read a menu layout");
            let shown = self.state.borrow().menu.is_some();
            if !shown && parent == 0 {
                self.close_menu(false).await;
                return;
            }
        }
        if let Some(id) = opens {
            self.send_opened(menu, id).await;
        }
    }

    fn apply_layout(&mut self, parent: i32, layout: &Layout<'_>) {
        let Self { menu: session, state, .. } = self;
        let Some(session) = session else {
            return;
        };
        state.send_if_modified(|state| match &mut state.menu {
            None if parent == 0 => {
                let mut root = menu::build(layout);
                root.id = 0;
                state.menu = Some(Arc::new(Menu {
                    item: session.key.clone(),
                    icon_theme_path: session.icon_theme_path.clone(),
                    root,
                }));
                true
            }
            None => false,
            Some(published) => modify(published, |published| {
                match published.root.find_mut(layout.id) {
                    Some(entry) => menu::merge(entry, layout),
                    None => false,
                }
            }),
        });
    }

    async fn send_opened(&mut self, menu: u64, id: i32) {
        let Self { menu: session, calls, .. } = self;
        let Some(session) = session
            .as_mut()
            .filter(|session| session.id == menu)
        else {
            return;
        };
        if id == 0 {
            if !session.opened {
                session.opened = true;
                send_event(calls, session, 0, "opened").await;
            }
            return;
        }
        if let Some(open) = session
            .open
            .iter_mut()
            .find(|(open, sent)| *open == id && !*sent)
        {
            open.1 = true;
            send_event(calls, session, id, "opened").await;
        }
    }

    fn set_menu_theme_path(&mut self, menu: u64, path: Vec<String>) {
        let Some(session) = self
            .menu
            .as_mut()
            .filter(|session| session.id == menu)
        else {
            return;
        };
        if session.icon_theme_path == path {
            return;
        }
        session
            .icon_theme_path
            .clone_from(&path);
        self.state
            .send_if_modified(|state| match &mut state.menu {
                Some(published) => modify(published, |menu| {
                    set(&mut menu.icon_theme_path, path)
                }),
                None => false,
            });
    }

    async fn menu_signal(&mut self, member: &str, message: &Message) {
        match member {
            "LayoutUpdated" => {
                let body = message.body();
                let Ok((_, parent)) = body.deserialize::<(u32, i32)>() else {
                    return;
                };
                let Self { menu: session, calls, .. } = self;
                if let Some(session) = session {
                    get_layout(calls, session, parent, None).await;
                }
            }
            "ItemsPropertiesUpdated" => {
                let body = message.body();
                let Ok((updated, removed)) =
                    body.deserialize::<(Vec<(i32, MenuProps<'_>)>, Vec<(i32, Vec<&str>)>)>()
                else {
                    return;
                };
                self.state.send_if_modified(|state| {
                    let Some(published) = &mut state.menu else {
                        return false;
                    };
                    modify(published, |published| {
                        let mut changed = false;
                        for (id, props) in &updated {
                            if let Some(entry) = published.root.find_mut(*id) {
                                changed |= menu::apply_partial(entry, props);
                            }
                        }
                        for (id, names) in &removed {
                            if let Some(entry) = published.root.find_mut(*id) {
                                changed |= menu::reset(entry, names);
                            }
                        }
                        changed
                    })
                });
            }
            _ => {}
        }
    }

    async fn open_submenu(&mut self, id: i32) {
        let mut path = Vec::new();
        {
            let state = self.state.borrow();
            let Some(menu) = &state.menu else {
                return;
            };
            if !menu.root.path_to(id, &mut path) {
                return;
            }
        }
        let Self { menu: session, calls, .. } = self;
        let Some(session) = session else {
            return;
        };
        if session
            .open
            .iter()
            .any(|&(open, _)| open == id)
        {
            return;
        }
        while let Some((last, sent)) = session
            .open
            .pop_if(|(last, _)| !path.contains(last))
        {
            if sent {
                send_event(calls, session, last, "closed").await;
            }
        }
        session.open.push((id, false));
        calls
            .call(
                Dest::Peer(Arc::clone(&session.owner)),
                &session.path,
                MENU_INTERFACE,
                "AboutToShow",
                &(id,),
                Then::AboutToShow { menu: session.id, id },
            )
            .await;
    }

    async fn close_submenu(&mut self, id: i32) {
        let Self { menu: session, calls, .. } = self;
        let Some(session) = session else {
            return;
        };
        let Some(at) = session
            .open
            .iter()
            .position(|&(open, _)| open == id)
        else {
            return;
        };
        while session.open.len() > at {
            let Some((open, sent)) = session.open.pop() else {
                break;
            };
            if sent {
                send_event(calls, session, open, "closed").await;
            }
        }
    }

    // `clicked` goes out before the menu closes, which matters to apps that rebuild menus on
    // close.
    async fn click_entry(&mut self, id: i32) {
        let clickable = {
            let state = self.state.borrow();
            state
                .menu
                .as_ref()
                .and_then(|menu| menu.find(id))
                .is_some_and(|entry| {
                    entry.id != 0 && !entry.separator && entry.enabled && !entry.submenu
                })
        };
        if !clickable {
            return;
        }
        let Self { menu: session, calls, .. } = self;
        if let Some(session) = session {
            send_event(calls, session, id, "clicked").await;
        }
        self.close_menu(true).await;
    }

    async fn close_menu(&mut self, events: bool) {
        let Some(mut session) = self.menu.take() else {
            return;
        };
        let menu = session.id;
        self.calls
            .forget(|then| then.menu() == Some(menu));
        if events {
            while let Some((id, sent)) = session.open.pop() {
                if sent {
                    send_event(&mut self.calls, &session, id, "closed").await;
                }
            }
            if session.opened {
                send_event(&mut self.calls, &session, 0, "closed").await;
            }
        }
        self.state
            .send_if_modified(|state| state.menu.take().is_some());
    }
}

async fn remove_rules(conn: &Connection, rules: &[OwnedMatchRule]) {
    let Ok(proxy) = zbus::fdo::DBusProxy::new(conn).await else {
        return;
    };
    for rule in rules {
        if let Err(error) = proxy
            .remove_match_rule(rule.clone().into_inner())
            .await
        {
            debug!(%error, "cannot remove a match rule");
        }
    }
}
