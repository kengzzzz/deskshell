//! Which panel is open: IPC commands and toolkit events in, surface actions out.

use deskshell_ipc::{self as ipc, Command};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Panel {
    Overview,
    Clipboard,
    PowerMenu,
    Keybindings,
    Launcher,
    Calendar,
    Media,
    Hub,
    Updates,
    SharePicker,
}

impl Panel {
    pub const ALL: [Self; 10] = [
        Self::Overview,
        Self::Clipboard,
        Self::PowerMenu,
        Self::Keybindings,
        Self::Launcher,
        Self::Calendar,
        Self::Media,
        Self::Hub,
        Self::Updates,
        Self::SharePicker,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Clipboard => "Clipboard",
            Self::PowerMenu => "Power menu",
            Self::Keybindings => "Keybindings",
            Self::Launcher => "Launcher",
            Self::Calendar => "Calendar",
            Self::Media => "Media",
            Self::Hub => "Hub",
            Self::Updates => "Updates",
            Self::SharePicker => "Share picker",
        }
    }

    pub fn namespace(self) -> &'static str {
        match self {
            Self::Overview | Self::Clipboard | Self::PowerMenu | Self::Keybindings => {
                "deskshell-overlay"
            }
            Self::Launcher => "deskshell-launcher",
            Self::Calendar => "deskshell-calendar",
            Self::Media => "deskshell-media",
            Self::Hub => "deskshell-hub",
            Self::Updates => "deskshell-updates",
            Self::SharePicker => "deskshell-share-picker",
        }
    }

    pub fn fullscreen(self) -> bool {
        matches!(
            self,
            Self::Overview
                | Self::Clipboard
                | Self::PowerMenu
                | Self::Keybindings
                | Self::Launcher
                | Self::SharePicker
        )
    }

    pub fn focus_grab(self) -> bool {
        !self.fullscreen()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    Overview,
    Launcher,
    Clipboard,
    PowerMenu,
    Keybindings,
    Calendar,
    QuickSettings,
    Hub,
    Notifications,
    Media,
    Updates,
}

impl Target {
    pub const ALL: [Self; 11] = [
        Self::Overview,
        Self::Launcher,
        Self::Clipboard,
        Self::PowerMenu,
        Self::Keybindings,
        Self::Calendar,
        Self::QuickSettings,
        Self::Hub,
        Self::Notifications,
        Self::Media,
        Self::Updates,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Launcher => "launcher",
            Self::Clipboard => "clipboard",
            Self::PowerMenu => "powermenu",
            Self::Keybindings => "keybindings",
            Self::Calendar => "calendar",
            Self::QuickSettings => "quicksettings",
            Self::Hub => "hub",
            Self::Notifications => "notifications",
            Self::Media => "media",
            Self::Updates => "updates",
        }
    }

    pub fn panel(self) -> Panel {
        match self {
            Self::Overview => Panel::Overview,
            Self::Launcher => Panel::Launcher,
            Self::Clipboard => Panel::Clipboard,
            Self::PowerMenu => Panel::PowerMenu,
            Self::Keybindings => Panel::Keybindings,
            Self::Calendar => Panel::Calendar,
            Self::QuickSettings | Self::Hub | Self::Notifications => Panel::Hub,
            Self::Media => Panel::Media,
            Self::Updates => Panel::Updates,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Toggle,
    Show,
    Hide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// The handler timeout already sent the client an error.
    Skip,
    Panel(Target, Op),
    RefreshWindows,
    Drop,
    ToggleDnd,
    ClearNotifications,
    ToggleKeepAwake,
    MediaControl(ipc::Media),
    RefreshUpdates,
    Lock,
    IsLocked,
    NotificationCount,
    ClipboardCopyEpoch,
    UpdatesStart,
    UpdatesCancel,
    Display(ipc::Display),
    CpuPower(ipc::CpuPower),
}

pub fn route(cancelled: bool, command: &Command) -> Route {
    if cancelled {
        return Route::Skip;
    }
    match *command {
        Command::Overview(op) => panel(Target::Overview, op),
        Command::Launcher(op) => panel(Target::Launcher, op),
        Command::Clipboard(op) => panel(Target::Clipboard, op),
        Command::PowerMenu(op) => panel(Target::PowerMenu, op),
        Command::Keybindings(op) => panel(Target::Keybindings, op),
        Command::Calendar(op) => panel(Target::Calendar, op),
        Command::QuickSettings(op) => panel(Target::QuickSettings, op),
        Command::Hub(ipc::Hub::Toggle) => Route::Panel(Target::Hub, Op::Toggle),
        Command::Hub(ipc::Hub::Show) => Route::Panel(Target::Hub, Op::Show),
        Command::Hub(ipc::Hub::Hide) => Route::Panel(Target::Hub, Op::Hide),
        Command::Notifications(ipc::Notifications::Toggle) => {
            Route::Panel(Target::Notifications, Op::Toggle)
        }
        Command::Notifications(ipc::Notifications::Show) => {
            Route::Panel(Target::Notifications, Op::Show)
        }
        Command::Notifications(ipc::Notifications::Hide) => {
            Route::Panel(Target::Notifications, Op::Hide)
        }
        Command::Media(ipc::Media::Toggle) => Route::Panel(Target::Media, Op::Toggle),
        Command::Media(ipc::Media::Show) => Route::Panel(Target::Media, Op::Show),
        Command::Media(ipc::Media::Hide) => Route::Panel(Target::Media, Op::Hide),
        Command::Updates(ipc::Updates::Toggle) => Route::Panel(Target::Updates, Op::Toggle),
        Command::Windows(ipc::Windows::Refresh) => Route::RefreshWindows,
        Command::Notifications(ipc::Notifications::ToggleDnd) => Route::ToggleDnd,
        Command::Notifications(ipc::Notifications::Clear) => Route::ClearNotifications,
        Command::Hub(ipc::Hub::ToggleKeepAwake) => Route::ToggleKeepAwake,
        Command::Media(
            media @ (ipc::Media::PlayPause
            | ipc::Media::Pause
            | ipc::Media::Next
            | ipc::Media::Previous),
        ) => Route::MediaControl(media),
        Command::Status(ipc::Status::RefreshUpdates) => Route::RefreshUpdates,
        Command::Lock(ipc::Lock::Lock) => Route::Lock,
        Command::Lock(ipc::Lock::IsLocked) => Route::IsLocked,
        Command::Notifications(ipc::Notifications::Count) => Route::NotificationCount,
        Command::ClipboardCopyEpoch => Route::ClipboardCopyEpoch,
        Command::Updates(ipc::Updates::Start) => Route::UpdatesStart,
        Command::Updates(ipc::Updates::Cancel) => Route::UpdatesCancel,
        Command::Display(call) => Route::Display(call),
        Command::CpuPower(call) => Route::CpuPower(call),
        _ => Route::Drop,
    }
}

fn panel(target: Target, op: ipc::Panel) -> Route {
    Route::Panel(
        target,
        match op {
            ipc::Panel::Toggle => Op::Toggle,
            ipc::Panel::Show => Op::Show,
            ipc::Panel::Hide => Op::Hide,
        },
    )
}

pub fn choose_output<'a>(
    focused: Option<&str>,
    outputs: impl IntoIterator<Item = (u32, Option<&'a str>)>,
) -> Option<u32> {
    let focused = focused?;
    outputs
        .into_iter()
        .find(|(_, name)| *name == Some(focused))
        .map(|(id, _)| id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Open<Id> {
    pub panel: Panel,
    pub id: Id,
    pub output: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actions<Id> {
    pub close: Option<Id>,
    pub open: Option<Open<Id>>,
}

impl<Id> Actions<Id> {
    const NONE: Self = Self { close: None, open: None };
}

#[derive(Debug)]
pub struct Manager<Id> {
    open: Option<Open<Id>>,
}

impl<Id> Default for Manager<Id> {
    fn default() -> Self {
        Self { open: None }
    }
}

impl<Id: Copy + Eq> Manager<Id> {
    pub fn current(&self) -> Option<&Open<Id>> {
        self.open.as_ref()
    }

    pub fn panel_of(&self, id: Id) -> Option<Panel> {
        self.open
            .filter(|open| open.id == id)
            .map(|open| open.panel)
    }

    pub fn command(
        &mut self,
        panel: Panel,
        op: Op,
        output: Option<u32>,
        new_id: impl FnOnce() -> Id,
    ) -> Actions<Id> {
        let is_open = self
            .open
            .is_some_and(|open| open.panel == panel);
        match (op, is_open) {
            (Op::Toggle | Op::Hide, true) => {
                Actions { close: self.open.take().map(|open| open.id), open: None }
            }
            (Op::Show, true) | (Op::Hide, false) => Actions::NONE,
            (Op::Toggle | Op::Show, false) => {
                let close = self.open.take().map(|open| open.id);
                let open = Open { panel, id: new_id(), output };
                self.open = Some(open);
                Actions { close, open: Some(open) }
            }
        }
    }

    pub fn dismiss(&mut self, id: Id) -> Option<Id> {
        self.panel_of(id)?;
        self.open.take().map(|open| open.id)
    }

    /// Late close events from a panel that was already replaced change nothing.
    pub fn closed(&mut self, id: Id) -> bool {
        self.dismiss(id).is_some()
    }

    pub fn output_removed(&mut self, output: u32) -> Option<Id> {
        if self
            .open
            .is_some_and(|open| open.output == Some(output))
        {
            return self.open.take().map(|open| open.id);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ids(u32);

    impl Ids {
        fn next(&mut self) -> impl FnOnce() -> u32 + '_ {
            move || {
                self.0 += 1;
                self.0
            }
        }
    }

    fn open(panel: Panel, id: u32, output: Option<u32>) -> Option<Open<u32>> {
        Some(Open { panel, id, output })
    }

    #[test]
    fn opening_a_panel_closes_the_open_one() {
        let mut ids = Ids(0);
        let mut manager = Manager::default();
        assert_eq!(
            manager.command(Panel::Hub, Op::Toggle, Some(7), ids.next()),
            Actions { close: None, open: open(Panel::Hub, 1, Some(7)) }
        );
        assert_eq!(
            manager.command(Panel::Calendar, Op::Show, None, ids.next()),
            Actions { close: Some(1), open: open(Panel::Calendar, 2, None) }
        );
        assert_eq!(
            manager.command(Panel::Launcher, Op::Toggle, None, ids.next()),
            Actions { close: Some(2), open: open(Panel::Launcher, 3, None) }
        );
        assert_eq!(
            manager.current().map(|open| open.panel),
            Some(Panel::Launcher)
        );
    }

    #[test]
    fn toggle_show_and_hide() {
        let mut ids = Ids(0);
        let mut manager = Manager::default();
        assert_eq!(
            manager.command(Panel::Media, Op::Hide, None, ids.next()),
            Actions::NONE,
            "hide on a closed panel"
        );
        assert_eq!(
            manager
                .command(Panel::Media, Op::Toggle, None, ids.next())
                .open,
            open(Panel::Media, 1, None)
        );
        assert_eq!(
            manager.command(Panel::Media, Op::Show, None, ids.next()),
            Actions::NONE,
            "show on the open panel"
        );
        assert_eq!(
            manager.command(Panel::Hub, Op::Hide, None, ids.next()),
            Actions::NONE,
            "hide of another panel"
        );
        assert_eq!(
            manager.command(Panel::Media, Op::Toggle, None, ids.next()),
            Actions { close: Some(1), open: None }
        );
        assert!(manager.current().is_none());
        assert_eq!(
            manager
                .command(Panel::Media, Op::Show, None, ids.next())
                .open,
            open(Panel::Media, 2, None)
        );
        assert_eq!(
            manager.command(Panel::Media, Op::Hide, None, ids.next()),
            Actions { close: Some(2), open: None }
        );
        assert!(manager.current().is_none());
    }

    #[test]
    fn hub_quicksettings_and_notifications_drive_the_hub() {
        let commands = [
            (
                Command::Hub(ipc::Hub::Toggle),
                Target::Hub,
                Op::Toggle,
            ),
            (
                Command::QuickSettings(ipc::Panel::Show),
                Target::QuickSettings,
                Op::Show,
            ),
            (
                Command::Notifications(ipc::Notifications::Hide),
                Target::Notifications,
                Op::Hide,
            ),
        ];
        for (command, target, op) in commands {
            assert_eq!(route(false, &command), Route::Panel(target, op));
            assert_eq!(target.panel(), Panel::Hub);
        }

        let mut ids = Ids(0);
        let mut manager = Manager::default();
        let notifications = Target::Notifications.panel();
        let quicksettings = Target::QuickSettings.panel();
        assert!(
            manager
                .command(notifications, Op::Show, None, ids.next())
                .open
                .is_some()
        );
        assert_eq!(
            manager.command(quicksettings, Op::Toggle, None, ids.next()),
            Actions { close: Some(1), open: None }
        );
    }

    #[test]
    fn other_functions_of_panel_targets_are_dropped() {
        assert_eq!(
            route(false, &Command::Screenshot(ipc::Screenshot::Area)),
            Route::Drop
        );
        assert_eq!(
            route(false, &Command::Updates(ipc::Updates::Toggle)),
            Route::Panel(Target::Updates, Op::Toggle)
        );
        assert_eq!(
            route(false, &Command::Windows(ipc::Windows::Refresh)),
            Route::RefreshWindows
        );
    }

    #[test]
    fn every_panel_target_routes_to_its_panel() {
        for target in Target::ALL {
            let command = Command::parse(target.name(), "toggle", &[]).unwrap();
            assert_eq!(
                route(false, &command),
                Route::Panel(target, Op::Toggle)
            );
        }
        let namespaces: Vec<&str> = Panel::ALL
            .iter()
            .map(|panel| panel.namespace())
            .collect();
        assert!(
            namespaces
                .iter()
                .all(|namespace| namespace.starts_with("deskshell-"))
        );
    }

    #[test]
    fn panels_open_on_the_focused_output() {
        let outputs = [(31, Some("Virtual-1")), (44, Some("Virtual-2")), (50, None)];
        assert_eq!(
            choose_output(Some("Virtual-2"), outputs),
            Some(44)
        );
        assert_eq!(
            choose_output(Some("Virtual-1"), outputs),
            Some(31)
        );
        assert_eq!(
            choose_output(Some("DP-3"), outputs),
            None,
            "no match"
        );
        assert_eq!(choose_output(None, outputs), None, "no Hyprland");

        let mut ids = Ids(0);
        let mut manager = Manager::default();
        let output = choose_output(Some("Virtual-2"), outputs);
        assert_eq!(
            manager
                .command(Panel::Calendar, Op::Toggle, output, ids.next())
                .open,
            open(Panel::Calendar, 1, Some(44))
        );
        assert_eq!(
            manager.command(Panel::Calendar, Op::Show, Some(31), ids.next()),
            Actions::NONE,
            "an open panel stays where it is"
        );
    }

    #[test]
    fn a_panel_closed_by_the_toolkit_opens_again() {
        let mut ids = Ids(0);
        let mut manager = Manager::default();
        manager.command(Panel::Hub, Op::Toggle, Some(44), ids.next());
        assert!(manager.closed(1));
        assert!(manager.current().is_none());
        assert_eq!(
            manager
                .command(Panel::Hub, Op::Toggle, Some(31), ids.next())
                .open,
            open(Panel::Hub, 2, Some(31))
        );

        assert_eq!(manager.output_removed(44), None);
        assert_eq!(manager.output_removed(31), Some(2));
        assert!(!manager.closed(2), "already forgotten");
        assert_eq!(
            manager
                .command(Panel::Hub, Op::Toggle, None, ids.next())
                .open,
            open(Panel::Hub, 3, None)
        );
    }

    #[test]
    fn late_close_events_do_not_reset_a_newer_panel() {
        let mut ids = Ids(0);
        let mut manager = Manager::default();
        manager.command(Panel::Hub, Op::Toggle, None, ids.next());
        manager.command(Panel::Media, Op::Toggle, None, ids.next());
        assert!(!manager.closed(1));
        assert_eq!(manager.dismiss(1), None);
        assert_eq!(manager.panel_of(2), Some(Panel::Media));
        assert_eq!(manager.dismiss(2), Some(2));
        assert!(manager.current().is_none());
    }

    #[test]
    fn the_copy_epoch_is_served() {
        assert_eq!(
            route(false, &Command::ClipboardCopyEpoch),
            Route::ClipboardCopyEpoch
        );
    }

    #[test]
    fn notification_calls_go_to_the_service() {
        let dnd = Command::Notifications(ipc::Notifications::ToggleDnd);
        let clear = Command::Notifications(ipc::Notifications::Clear);
        assert_eq!(route(false, &dnd), Route::ToggleDnd);
        assert_eq!(route(false, &clear), Route::ClearNotifications);
        assert_eq!(route(true, &dnd), Route::Skip);
    }

    #[test]
    fn cancelled_requests_are_skipped() {
        for command in [
            Command::Launcher(ipc::Panel::Toggle),
            Command::Windows(ipc::Windows::Refresh),
            Command::Lock(ipc::Lock::Lock),
        ] {
            assert_eq!(route(true, &command), Route::Skip);
        }
    }

    #[test]
    fn keep_awake_media_controls_and_update_checks() {
        let keep_awake = Command::Hub(ipc::Hub::ToggleKeepAwake);
        let updates = Command::Status(ipc::Status::RefreshUpdates);
        assert_eq!(route(false, &keep_awake), Route::ToggleKeepAwake);
        assert_eq!(route(false, &updates), Route::RefreshUpdates);
        for media in
            [ipc::Media::PlayPause, ipc::Media::Pause, ipc::Media::Next, ipc::Media::Previous]
        {
            assert_eq!(
                route(false, &Command::Media(media)),
                Route::MediaControl(media)
            );
        }
    }

    #[test]
    fn lock_calls_and_the_count() {
        for (command, expected) in [
            (Command::Lock(ipc::Lock::Lock), Route::Lock),
            (
                Command::Lock(ipc::Lock::IsLocked),
                Route::IsLocked,
            ),
            (
                Command::Notifications(ipc::Notifications::Count),
                Route::NotificationCount,
            ),
        ] {
            assert_eq!(route(false, &command), expected);
        }
    }

    #[test]
    fn update_start_and_cancel() {
        for (command, expected) in [
            (
                Command::Updates(ipc::Updates::Start),
                Route::UpdatesStart,
            ),
            (
                Command::Updates(ipc::Updates::Cancel),
                Route::UpdatesCancel,
            ),
        ] {
            assert_eq!(route(false, &command), expected);
        }
    }
}
