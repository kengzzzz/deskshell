//! The launcher's command mode actions, then the power menu's.

use deskshell_core::launcher::rank::ActionText;

use super::super::Effect;
use super::super::power;
use crate::panels::Panel;
use crate::screenshot::flow::Mode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Run {
    Show(Panel),
    ToggleDnd,
    ClearNotifications,
    ToggleKeepAwake,
    Deskshell(&'static [&'static str]),
    Screenshot(Mode),
    Snapshot(&'static [&'static str]),
    Power(power::Action),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub symbol: &'static str,
    pub bind: &'static str,
    pub keywords: &'static str,
    pub run: Run,
}

const fn action(
    id: &'static str,
    name: &'static str,
    description: &'static str,
    symbol: &'static str,
    bind: &'static str,
    keywords: &'static str,
    run: Run,
) -> Action {
    Action { id, name, description, symbol, bind, keywords, run }
}

pub const SHELL: [Action; 12] = [
    action(
        "clipboard",
        "Clipboard",
        "Open clipboard history",
        "content_paste",
        "Open clipboard manager",
        "paste history",
        Run::Show(Panel::Clipboard),
    ),
    action(
        "hub",
        "Notifications",
        "Open the hub",
        "notifications",
        "Open notification center",
        "hub quick settings controls",
        Run::Show(Panel::Hub),
    ),
    action(
        "keybindings",
        "Keybindings",
        "Show keybinding cheat sheet",
        "keyboard",
        "Show keybindings cheat sheet",
        "shortcuts keys help",
        Run::Show(Panel::Keybindings),
    ),
    action(
        "overview",
        "Overview",
        "Show window overview",
        "view_carousel",
        "Open window overview",
        "windows workspaces",
        Run::Show(Panel::Overview),
    ),
    action(
        "screenshot-area",
        "Screenshot Area",
        "Capture a selected region",
        "screenshot_region",
        "Take area screenshot",
        "capture region",
        Run::Screenshot(Mode::Area),
    ),
    action(
        "screenshot-full",
        "Screenshot Full",
        "Capture the whole screen",
        "screenshot_monitor",
        "Take full screenshot",
        "capture screen",
        Run::Screenshot(Mode::Full),
    ),
    action(
        "color-picker",
        "Color Picker",
        "Pick a color and copy it",
        "colorize",
        "",
        "colour hyprpicker",
        Run::Snapshot(&["hyprpicker", "-a"]),
    ),
    action(
        "dnd",
        "Do Not Disturb",
        "Toggle notification popups",
        "do_not_disturb_on",
        "",
        "dnd silence mute",
        Run::ToggleDnd,
    ),
    action(
        "clear-notifications",
        "Clear Notifications",
        "Dismiss all notifications",
        "clear_all",
        "",
        "dismiss",
        Run::ClearNotifications,
    ),
    action(
        "keep-awake",
        "Keep Awake",
        "Currently off",
        "coffee",
        "",
        "caffeine idle inhibit",
        Run::ToggleKeepAwake,
    ),
    action(
        "restart-shell",
        "Restart deskshell",
        "Reload the shell",
        "refresh",
        "Restart deskshell",
        "reload shell deskshell",
        Run::Deskshell(&["restart"]),
    ),
    action(
        "powermenu",
        "Power Menu",
        "Open the power menu",
        "power_settings_new",
        "Open power menu",
        "session",
        Run::Show(Panel::PowerMenu),
    ),
];

pub fn all() -> impl Iterator<Item = Action> {
    SHELL.into_iter().chain(
        power::Action::ALL
            .into_iter()
            .map(|power| Action {
                id: power.id(),
                name: power.label(),
                description: power.description(),
                symbol: power.symbol(),
                bind: if power == power::Action::Lock { "Lock screen" } else { "" },
                keywords: if power == power::Action::PowerOff { "shutdown" } else { "" },
                run: Run::Power(power),
            }),
    )
}

pub fn description(action: &Action, keep_awake: bool) -> &'static str {
    match action.run {
        Run::ToggleKeepAwake if keep_awake => "Currently on",
        _ => action.description,
    }
}

pub fn texts(actions: &[Action]) -> Vec<ActionText> {
    actions
        .iter()
        .map(|action| ActionText::new(action.name, action.keywords))
        .collect()
}

pub fn run(action: &Action, effects: &mut Vec<Effect>) {
    effects.push(Effect::Close);
    effects.push(match action.run {
        Run::Show(panel) => Effect::Show(panel),
        Run::ToggleDnd => Effect::ToggleDnd,
        Run::ClearNotifications => Effect::ClearNotifications,
        Run::ToggleKeepAwake => Effect::ToggleKeepAwake,
        Run::Screenshot(mode) => Effect::Screenshot(mode),
        Run::Deskshell(args) => Effect::Spawn {
            argv: std::iter::once(super::super::binary())
                .chain(args.iter().copied())
                .map(String::from)
                .collect(),
            cwd: None,
        },
        Run::Snapshot(argv) => Effect::SpawnSettled(
            argv.iter()
                .map(|arg| (*arg).to_owned())
                .collect(),
        ),
        Run::Power(power) => Effect::Spawn { argv: power.command(), cwd: None },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_runs_deskshell_restart() {
        let restart = all()
            .find(|action| action.id == "restart-shell")
            .unwrap();
        let mut effects = Vec::new();
        run(&restart, &mut effects);
        assert_eq!(
            effects,
            [
                Effect::Close,
                Effect::Spawn {
                    argv: vec![String::from("deskshell"), String::from("restart")],
                    cwd: None
                }
            ]
        );
    }

    #[test]
    fn color_picker_waits_for_the_launcher_to_close() {
        let picker = all()
            .find(|action| action.id == "color-picker")
            .unwrap();
        let mut effects = Vec::new();
        run(&picker, &mut effects);
        assert_eq!(
            effects,
            [
                Effect::Close,
                Effect::SpawnSettled(vec![
                    String::from("hyprpicker"),
                    String::from("-a")
                ])
            ]
        );
    }

    #[test]
    fn keep_awake_says_whether_it_is_on() {
        let keep_awake = all()
            .find(|action| action.id == "keep-awake")
            .unwrap();
        assert_eq!(description(&keep_awake, false), "Currently off");
        assert_eq!(description(&keep_awake, true), "Currently on");
        let dnd = all()
            .find(|action| action.id == "dnd")
            .unwrap();
        assert_eq!(
            description(&dnd, true),
            "Toggle notification popups"
        );
    }
}
