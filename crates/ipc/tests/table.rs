//! The call table against an independent list of every function.

use std::assert_matches;

use deskshell_ipc::{
    ArgType, CallError, Command, CpuPower, CpuPowerMode, Display, Hub, Lock, Media, Notifications,
    Panel, ReturnType, Screenshot, SharePicker, Status, TARGETS, Updates, Windows,
};

fn panel(target: &'static str, variant: fn(Panel) -> Command) -> Vec<Case> {
    vec![
        case(
            target,
            "toggle",
            &[],
            variant(Panel::Toggle),
            ReturnType::Void,
        ),
        case(
            target,
            "show",
            &[],
            variant(Panel::Show),
            ReturnType::Void,
        ),
        case(
            target,
            "hide",
            &[],
            variant(Panel::Hide),
            ReturnType::Void,
        ),
    ]
}

struct Case {
    target: &'static str,
    function: &'static str,
    args: &'static [&'static str],
    command: Command,
    returns: ReturnType,
}

fn case(
    target: &'static str,
    function: &'static str,
    args: &'static [&'static str],
    command: Command,
    returns: ReturnType,
) -> Case {
    Case { target, function, args, command, returns }
}

/// Every function, written out independently of the table.
fn spec() -> Vec<Case> {
    use ReturnType::{Bool, String, Void};
    let mut cases = vec![
        case(
            "lock",
            "lock",
            &[],
            Command::Lock(Lock::Lock),
            Void,
        ),
        case(
            "lock",
            "isLocked",
            &[],
            Command::Lock(Lock::IsLocked),
            Bool,
        ),
        case(
            "sharepicker",
            "pick",
            &["[0]window", "share"],
            Command::SharePicker(SharePicker::Pick {
                window_list: "[0]window".into(),
                purpose: "share".into(),
            }),
            String,
        ),
        case(
            "screenshot",
            "area",
            &[],
            Command::Screenshot(Screenshot::Area),
            String,
        ),
        case(
            "screenshot",
            "full",
            &[],
            Command::Screenshot(Screenshot::Full),
            String,
        ),
        case(
            "media",
            "toggle",
            &[],
            Command::Media(Media::Toggle),
            Void,
        ),
        case(
            "media",
            "show",
            &[],
            Command::Media(Media::Show),
            Void,
        ),
        case(
            "media",
            "hide",
            &[],
            Command::Media(Media::Hide),
            Void,
        ),
        case(
            "media",
            "playPause",
            &[],
            Command::Media(Media::PlayPause),
            Void,
        ),
        case(
            "media",
            "pause",
            &[],
            Command::Media(Media::Pause),
            Void,
        ),
        case(
            "media",
            "next",
            &[],
            Command::Media(Media::Next),
            Void,
        ),
        case(
            "media",
            "previous",
            &[],
            Command::Media(Media::Previous),
            Void,
        ),
    ];
    cases.extend(panel("overview", Command::Overview));
    cases.extend(panel("launcher", Command::Launcher));
    cases.extend(panel("clipboard", Command::Clipboard));
    cases.push(case(
        "clipboard",
        "copyEpoch",
        &[],
        Command::ClipboardCopyEpoch,
        String,
    ));
    cases.extend(panel("powermenu", Command::PowerMenu));
    cases.extend(panel("keybindings", Command::Keybindings));
    cases.extend(panel("calendar", Command::Calendar));
    cases.extend(panel("quicksettings", Command::QuickSettings));
    cases.extend([
        case(
            "hub",
            "toggle",
            &[],
            Command::Hub(Hub::Toggle),
            Void,
        ),
        case("hub", "show", &[], Command::Hub(Hub::Show), Void),
        case("hub", "hide", &[], Command::Hub(Hub::Hide), Void),
        case(
            "hub",
            "toggleKeepAwake",
            &[],
            Command::Hub(Hub::ToggleKeepAwake),
            Void,
        ),
        case(
            "notifications",
            "toggle",
            &[],
            Command::Notifications(Notifications::Toggle),
            Void,
        ),
        case(
            "notifications",
            "show",
            &[],
            Command::Notifications(Notifications::Show),
            Void,
        ),
        case(
            "notifications",
            "hide",
            &[],
            Command::Notifications(Notifications::Hide),
            Void,
        ),
        case(
            "notifications",
            "toggleDnd",
            &[],
            Command::Notifications(Notifications::ToggleDnd),
            Void,
        ),
        case(
            "notifications",
            "clear",
            &[],
            Command::Notifications(Notifications::Clear),
            Void,
        ),
        case(
            "notifications",
            "count",
            &[],
            Command::Notifications(Notifications::Count),
            String,
        ),
        case(
            "status",
            "refreshUpdates",
            &[],
            Command::Status(Status::RefreshUpdates),
            Void,
        ),
        case(
            "updates",
            "start",
            &[],
            Command::Updates(Updates::Start),
            Void,
        ),
        case(
            "updates",
            "cancel",
            &[],
            Command::Updates(Updates::Cancel),
            Void,
        ),
        case(
            "updates",
            "toggle",
            &[],
            Command::Updates(Updates::Toggle),
            Void,
        ),
        case(
            "windows",
            "refresh",
            &[],
            Command::Windows(Windows::Refresh),
            Void,
        ),
        case(
            "display",
            "toggleHdr",
            &[],
            Command::Display(Display::ToggleHdr),
            Void,
        ),
        case(
            "display",
            "toggleVrr",
            &[],
            Command::Display(Display::ToggleVrr),
            Void,
        ),
        case(
            "display",
            "toggleNightLight",
            &[],
            Command::Display(Display::ToggleNightLight),
            Void,
        ),
        case(
            "cpupower",
            "cycle",
            &[],
            Command::CpuPower(CpuPower::Cycle),
            Void,
        ),
        case(
            "cpupower",
            "details",
            &[],
            Command::CpuPower(CpuPower::Details),
            Void,
        ),
        case(
            "cpupower",
            "set",
            &["balanced"],
            Command::CpuPower(CpuPower::Set(CpuPowerMode::Balanced)),
            Void,
        ),
    ]);
    cases
}

#[test]
fn every_spec_function_parses() {
    for c in spec() {
        let command = Command::parse(c.target, c.function, c.args)
            .unwrap_or_else(|err| panic!("{} {}: {err}", c.target, c.function));
        assert_eq!(command, c.command, "{} {}", c.target, c.function);
        assert_eq!(
            command.returns(),
            c.returns,
            "{} {}",
            c.target,
            c.function
        );
    }
}

#[test]
fn table_holds_exactly_the_spec() {
    let spec = spec();
    let table: Vec<(&str, &str)> = TARGETS
        .iter()
        .flat_map(|t| {
            t.functions
                .iter()
                .map(move |f| (t.name, f.name))
        })
        .collect();
    let expected: Vec<(&str, &str)> = spec
        .iter()
        .map(|c| (c.target, c.function))
        .collect();
    assert_eq!(table, expected);
}

#[test]
fn table_agrees_with_commands() {
    for target in TARGETS {
        for function in target.functions {
            let name = format!("{} {}", target.name, function.name);
            let args: Vec<&str> = function
                .params
                .iter()
                .map(|p| match (p.ty, p.name) {
                    (ArgType::String, "mode") => "eco",
                    (ArgType::String, _) => "text",
                })
                .collect();
            let command = Command::parse(target.name, function.name, &args)
                .unwrap_or_else(|err| panic!("{name}: {err}"));
            assert_eq!(command.returns(), function.returns, "{name}");

            let mut extra = args.clone();
            extra.push("extra");
            assert_matches!(
                Command::parse(target.name, function.name, &extra),
                Err(CallError::ArgCount { .. }),
                "{name}"
            );
            if !args.is_empty() {
                assert_matches!(
                    Command::parse(target.name, function.name, &args[1..]),
                    Err(CallError::ArgCount { .. }),
                    "{name}"
                );
            }
        }
    }
}

fn error(target: &str, function: &str, args: &[&str]) -> CallError {
    Command::parse(target, function, args).unwrap_err()
}

#[test]
fn errors_use_the_qs_messages() {
    assert_eq!(
        error("nope", "toggle", &[]).to_string(),
        "Target not found."
    );
    assert_eq!(
        error("launcher", "nope", &[]).to_string(),
        "Function not found."
    );
    assert_eq!(
        error("launcher", "toggle", &["a"]).to_string(),
        "Too many arguments provided (0 required but 1 were provided.)"
    );
    assert_eq!(
        error("sharepicker", "pick", &["a", "b", "c"]).to_string(),
        "Too many arguments provided (2 required but 3 were provided.)"
    );
    assert_eq!(
        error("sharepicker", "pick", &["a"]).to_string(),
        "Too few arguments provided (2 required but 1 were provided.)"
    );
    assert_matches!(
        error("sharepicker", "open", &["a", "b", "c"]),
        CallError::FunctionNotFound,
        "the FIFO call is gone"
    );
}

#[test]
fn target_is_checked_before_function_and_count() {
    assert_matches!(error("", "", &["a"]), CallError::TargetNotFound);
    assert_matches!(
        error("lock", "", &["a"]),
        CallError::FunctionNotFound
    );
    assert_matches!(
        error("sharepicker", "pick", &["x"]),
        CallError::ArgCount { .. }
    );
}

#[test]
fn report_adds_the_function_definition() {
    assert_eq!(
        error("nope", "toggle", &[]).report(),
        "Target not found."
    );
    assert_eq!(
        error("sharepicker", "pick", &["a"]).report(),
        "Too few arguments provided (2 required but 1 were provided.)\n\
         Function definition: function pick(windowList: string, purpose: string): string"
    );
    assert_eq!(
        error("lock", "isLocked", &["a"]).report(),
        "Too many arguments provided (0 required but 1 were provided.)\n\
         Function definition: function isLocked(): bool"
    );
}

#[test]
fn show_uses_the_qs_layout() {
    let show: String = TARGETS
        .iter()
        .map(|t| format!("{t}\n"))
        .collect();
    assert!(show.starts_with(
        "target lock
  function lock(): void
  function isLocked(): bool
target sharepicker
  function pick(windowList: string, purpose: string): string
target screenshot
  function area(): string
  function full(): string
target media
  function toggle(): void
"
    ));
    assert!(show.ends_with(
        "target windows
  function refresh(): void
target display
  function toggleHdr(): void
  function toggleVrr(): void
  function toggleNightLight(): void
target cpupower
  function cycle(): void
  function details(): void
  function set(mode: string): void
"
    ));
}

#[test]
fn cpupower_set_takes_the_three_modes() {
    for (word, mode) in [
        ("eco", CpuPowerMode::Eco),
        ("balanced", CpuPowerMode::Balanced),
        ("performance", CpuPowerMode::Performance),
    ] {
        assert_eq!(
            Command::parse("cpupower", "set", &[word]).unwrap(),
            Command::CpuPower(CpuPower::Set(mode))
        );
    }
    for word in ["turbo", "Eco", "", "custom"] {
        let error = error("cpupower", "set", &[word]);
        assert_matches!(
            &error,
            CallError::InvalidArgument { value, .. } if value == word,
            "{word:?}"
        );
        assert_eq!(
            error.report(),
            format!("Invalid mode: {word:?} (one of eco, balanced, performance).")
        );
    }
}
