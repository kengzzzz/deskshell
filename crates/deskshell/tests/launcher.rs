//! The launcher's logic and effects. Effects are values, nothing runs.

mod common;

use std::assert_matches;
use std::path::PathBuf;
use std::sync::Arc;

use deskshell::panel::launcher::{self, Item, Launcher, Message, Usage, calc};
use deskshell::panel::{Effect, Env};
use deskshell::panels::Panel;
use deskshell::screenshot;
use deskshell_core::launcher::query::Mode;
use deskshell_ui::Theme;
use iced::Point;

use common::{TENANT, Temp};

struct Harness {
    theme: Theme,
    sources: deskshell::panel::Sources,
    launcher: Launcher,
    effects: Vec<Effect>,
}

impl Harness {
    fn new() -> Self {
        Self::with_usage(Usage::default())
    }

    fn with_usage(usage: Usage) -> Self {
        let mut harness = Self {
            theme: Theme::default(),
            sources: common::sources(None, usage),
            launcher: Launcher::default(),
            effects: Vec::new(),
        };
        let env = harness.sources.env(&harness.theme);
        let _ = harness.launcher.open(&env);
        harness
    }

    fn env(&self) -> Env<'_> {
        self.sources.env(&self.theme)
    }

    fn send(&mut self, message: Message) -> &[Effect] {
        self.effects.clear();
        let env = self.sources.env(&self.theme);
        let _ = self
            .launcher
            .update(message, &env, &mut self.effects);
        &self.effects
    }

    fn input(&mut self, text: &str) {
        let _ = self.send(Message::Input(text.to_owned()));
    }

    fn items(&self) -> Vec<Item> {
        self.launcher.items().cloned().collect()
    }

    fn app_ids(&self) -> Vec<String> {
        self.launcher
            .items()
            .filter_map(|item| match item {
                Item::App(index) => self
                    .launcher
                    .app(*index)
                    .map(|entry| entry.id.clone()),
                _ => None,
            })
            .collect()
    }

    fn activate(&mut self, alternate: bool, pick: impl Fn(&Item) -> bool) -> Vec<Effect> {
        let index = self
            .launcher
            .items()
            .position(pick)
            .expect("a matching row");
        let moves = index as i32 - self.launcher.selected() as i32;
        let _ = self.send(Message::Move(moves));
        assert_eq!(self.launcher.selected(), index);
        self.send(Message::Activate { alternate })
            .to_vec()
    }

    fn activate_app(&mut self, id: &str, alternate: bool) -> Vec<Effect> {
        let index = self
            .launcher
            .items()
            .position(|item| matches!(item, Item::App(app) if self.launcher.app(*app).is_some_and(|entry| entry.id == id)))
            .expect("the app row");
        let moves = index as i32 - self.launcher.selected() as i32;
        let _ = self.send(Message::Move(moves));
        self.send(Message::Activate { alternate })
            .to_vec()
    }
}

fn spawn(argv: &[&str], cwd: Option<&str>) -> Effect {
    Effect::Spawn {
        argv: argv
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect(),
        cwd: cwd.map(PathBuf::from),
    }
}

#[test]
fn modes_and_stepping_back() {
    let mut harness = Harness::new();
    assert_eq!(harness.launcher.mode(), Mode::Apps);
    assert_eq!(
        harness.app_ids().len(),
        6,
        "NoDisplay entries are not listed"
    );

    harness.input(">");
    assert_eq!(harness.launcher.mode(), Mode::Commands);
    assert_eq!(
        harness.launcher.text(),
        "",
        "the prefix leaves the field"
    );
    let _ = harness.send(Message::StepBack);
    assert_eq!(
        harness.launcher.mode(),
        Mode::Apps,
        "Backspace on an empty field steps out"
    );

    harness.input("@");
    assert_eq!(harness.launcher.mode(), Mode::Filters);
    assert_eq!(harness.items().len(), 8, "every filter");
    harness.input("k ");
    assert_eq!(
        harness.launcher.text(),
        "",
        "a typed key and space pick the filter"
    );
    harness.input("shell");
    assert_eq!(harness.app_ids(), ["foot"]);
    harness.input("");
    let _ = harness.send(Message::StepBack);
    assert_eq!(
        harness.launcher.mode(),
        Mode::Filters,
        "the first step drops the key"
    );
    assert_eq!(harness.items().len(), 8);
    let _ = harness.send(Message::StepBack);
    assert_eq!(harness.launcher.mode(), Mode::Apps);

    let _ = harness.send(Message::SetMode(Mode::Filters));
    let effects = harness.activate(false, |item| *item == Item::Filter('t'));
    assert!(
        effects.is_empty(),
        "picking a filter stays in the launcher"
    );
    assert_eq!(
        harness.app_ids(),
        ["htop"],
        "@t lists terminal apps"
    );

    let _ = harness.send(Message::SetMode(Mode::Bangs));
    assert_eq!(harness.launcher.mode(), Mode::Bangs);
    let _ = harness.send(Message::SetMode(Mode::Apps));
    assert_eq!(harness.launcher.mode(), Mode::Apps);
}

#[test]
fn bang_flow() {
    let mut harness = Harness::new();
    harness.input("!");
    assert_eq!(
        harness.items(),
        [Item::Bang(0), Item::Bang(1), Item::Bang(2)],
        "suggested bangs by popularity"
    );
    harness.input("yt cats");
    assert_eq!(
        harness.items()[0],
        Item::BangSearch { bang: 0, query: String::from("cats") },
        "the yt cats shortcut comes first"
    );
    let effects = harness.activate(false, |item| {
        matches!(item, Item::BangSearch { .. })
    });
    assert_eq!(
        effects,
        [
            Effect::CountBang(String::from("yt")),
            Effect::Close,
            spawn(
                &["deskshell", "launch", "--", "xdg-open", "https://duckduckgo.com/?q=!yt%20cats"],
                None
            ),
        ]
    );

    harness.input("wiki");
    let effects = harness.activate(false, |item| *item == Item::Bang(1));
    assert!(effects.is_empty());
    assert_eq!(harness.launcher.text(), "");
    assert_eq!(
        harness.items(),
        [Item::BangSearch { bang: 1, query: String::new() }]
    );
    harness.input("rust");
    let effects = harness.activate(false, |_| true);
    assert_eq!(
        effects[2],
        spawn(
            &["deskshell", "launch", "--", "xdg-open", "https://duckduckgo.com/?q=!w%20rust"],
            None
        )
    );
    harness.input("");
    let _ = harness.send(Message::StepBack);
    assert_eq!(
        harness.launcher.mode(),
        Mode::Bangs,
        "the first step drops the bang"
    );
    let _ = harness.send(Message::StepBack);
    assert_eq!(harness.launcher.mode(), Mode::Apps);

    harness.input("!");
    harness.input("zzzz");
    assert_eq!(
        harness.items(),
        [Item::Web(String::from("!zzzz"))]
    );
}

#[test]
fn calculator_row_with_a_fake_qalc() {
    let temp = Temp::new("qalc");
    let fake = temp.0.join("qalc");
    std::fs::write(
        &fake,
        "#!/bin/sh\n[ \"$1\" = -t ] && [ \"$2\" = -- ] || exit 1\nprintf '  \\342\\210\\2225 for %s \\n' \"$3\"\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(calc::run(fake.clone(), String::from("2 - 7")));
    assert_eq!(
        result, "−5 for 2 - 7",
        "trimmed output, the argument after --"
    );
    let missing = runtime.block_on(calc::run(
        temp.0.join("absent"),
        String::from("1+1"),
    ));
    assert_eq!(missing, "");

    let mut harness = Harness::new();
    harness.sources.launcher.config.qalc = Some(fake);
    harness.input("2+2");
    assert_eq!(
        harness.items()[0],
        Item::Calculator { answer: String::from("4"), expression: String::from("2+2") },
        "the built-in calculator answers before qalc does"
    );
    harness.input("2 - 7");
    let _ = harness
        .send(Message::CalcDone { expression: String::from("2 - 7"), result: result.clone() });
    assert_eq!(
        harness.launcher.qalc(),
        ("2 - 7", result.as_str())
    );
    assert_eq!(
        harness.items()[0],
        Item::Calculator { answer: result, expression: String::from("2 - 7") }
    );
    let effects = harness.activate(false, |item| {
        matches!(item, Item::Calculator { .. })
    });
    assert_eq!(
        effects,
        [
            Effect::Close,
            Effect::Clipboard(deskshell::panel::clipboard::Request::CopyText(
                String::from("-5 for 2 - 7")
            ))
        ],
        "U+2212 becomes -"
    );

    harness.input("10 km");
    let _ = harness.send(Message::CalcDone {
        expression: String::from("10 km"),
        result: String::from("10 km"),
    });
    assert!(
        !matches!(harness.items()[0], Item::Calculator { .. }),
        "a qalc result that repeats the expression is not shown"
    );
}

#[test]
fn app_effects() {
    let mut harness = Harness::new();
    assert_eq!(
        harness.activate_app("foot", false),
        [
            Effect::CountLaunch(String::from("foot")),
            Effect::Close,
            spawn(&["deskshell", "launch", "--", "foot"], None),
        ]
    );
    assert_eq!(
        harness.activate_app("htop", false),
        [
            Effect::CountLaunch(String::from("htop")),
            Effect::Close,
            spawn(
                &["deskshell", "launch", "--", "kitty", "-e", "htop"],
                Some("/srv/work")
            ),
        ],
        "terminal apps run in kitty, in the entry's Path"
    );
    assert_eq!(
        harness.activate_app("htop", true)[2],
        Effect::Dispatch(String::from(
            "hl.dsp.exec_cmd([[cd '/srv/work' && 'deskshell' 'launch' '--' 'kitty' '-e' 'htop']], \
             { float = true, center = true, size = [[monitor_h*1.1 monitor_h*0.75]] })"
        )),
        "Shift+Enter floats"
    );
    assert_eq!(
        harness.activate_app("game", true)[2],
        Effect::Dispatch(format!(
            "tenant_float_launch([['deskshell' 'launch' '--' '{TENANT}' '--run' 'game']], \
             {{ [[GameClass]], [[game]] }}, [[monitor_h*1.1 monitor_h*0.75]])"
        )),
        "the tenant runner floats through tenant_float_launch"
    );
    harness
        .sources
        .launcher
        .config
        .tenant_runner = None;
    assert_matches!(
        &harness.activate_app("game", true)[2],
        Effect::Dispatch(lua) if lua.starts_with("hl.dsp.exec_cmd("),
        "without the runner a tenant app floats like any other"
    );
}

#[test]
fn action_run_and_web_effects() {
    let mut harness = Harness::new();
    harness.input(">");
    let actions = harness.items().len();
    assert_eq!(
        actions, 17,
        "the 12 actions and the 5 power tiles"
    );
    assert_eq!(
        harness.activate(false, |item| *item == Item::Action(0)),
        [Effect::Close, Effect::Show(Panel::Clipboard)],
        "a shell action opens its panel"
    );
    harness.input("dnd");
    assert_eq!(
        harness.activate(false, |item| matches!(item, Item::Action(_))),
        [Effect::Close, Effect::ToggleDnd]
    );
    harness.input("screenshot area");
    assert_eq!(
        harness.activate(false, |item| matches!(item, Item::Action(_))),
        [Effect::Close, Effect::Screenshot(screenshot::flow::Mode::Area)],
        "the native flow, in-process"
    );
    harness.input("lock");
    assert_eq!(
        harness.activate(false, |item| matches!(item, Item::Action(_))),
        [Effect::Close, spawn(&["deskshell", "lock"], None)],
        "power actions come from the power menu"
    );
    harness.input("shutdown");
    assert_eq!(
        harness.activate(false, |item| matches!(item, Item::Action(_))),
        [Effect::Close, spawn(&["systemctl", "poweroff"], None)]
    );
    harness.input("  ls -la ");
    assert_eq!(
        harness.items(),
        [Item::Run(String::from("ls -la"))]
    );
    assert_eq!(
        harness.activate(false, |_| true),
        [
            Effect::Close,
            spawn(
                &["deskshell", "launch", "--", "/bin/zsh", "-c", "ls -la"],
                Some("/home/tester")
            )
        ]
    );
    harness.input("ls -la");
    assert_eq!(
        harness.activate(true, |_| true),
        [
            Effect::Close,
            spawn(
                &["deskshell", "launch", "--", "kitty", "--hold", "/bin/zsh", "-c", "ls -la"],
                Some("/home/tester")
            )
        ],
        "Shift+Enter runs in a terminal"
    );

    let _ = harness.send(Message::SetMode(Mode::Apps));
    harness.input("rust lang");
    assert_eq!(
        harness.items().last(),
        Some(&Item::Web(String::from("rust lang")))
    );
    assert_eq!(
        harness.activate(false, |item| matches!(item, Item::Web(_))),
        [
            Effect::Close,
            spawn(
                &["deskshell", "launch", "--", "xdg-open", "https://duckduckgo.com/?q=rust%20lang"],
                None
            )
        ]
    );
}

#[test]
fn usage_counts_rank_apps() {
    let mut usage = Usage::default();
    for _ in 0..3 {
        usage.count_launch("files");
    }
    usage.count_bang("gh");
    let mut harness = Harness::with_usage(usage);
    assert_eq!(
        harness
            .app_ids()
            .first()
            .map(String::as_str),
        Some("files"),
        "launch counts break ties after the score"
    );
    harness.input("!");
    assert_eq!(
        harness.items()[0],
        Item::Bang(2),
        "used bangs come first"
    );
}

#[test]
fn hover_selects_only_after_the_pointer_moves() {
    let mut harness = Harness::new();
    assert_eq!(harness.launcher.selected(), 0);
    let _ = harness.send(Message::Hover { index: 2, position: Point::new(100.0, 200.0) });
    assert_eq!(
        harness.launcher.selected(),
        0,
        "the first report only records the position"
    );
    let _ = harness.send(Message::Hover { index: 3, position: Point::new(100.0, 200.0) });
    assert_eq!(
        harness.launcher.selected(),
        0,
        "the same position selects nothing"
    );
    let _ = harness.send(Message::Hover { index: 3, position: Point::new(101.0, 230.0) });
    assert_eq!(harness.launcher.selected(), 3);
    let _ = harness.send(Message::Move(1));
    assert_eq!(harness.launcher.selected(), 4);
    let env = harness.env();
    let mut launcher = Launcher::default();
    let _ = launcher.open(&env);
    launcher.hover(1, Point::new(5.0, 5.0));
    assert_eq!(
        launcher.selected(),
        0,
        "opening forgets the last position"
    );
}

#[test]
fn moving_wraps_and_click_activates() {
    let mut harness = Harness::new();
    let count = harness.items().len() as i32;
    let _ = harness.send(Message::Move(-1));
    assert_eq!(harness.launcher.selected(), count as usize - 1);
    let _ = harness.send(Message::Move(1));
    assert_eq!(harness.launcher.selected(), 0);
    let effects = harness
        .send(Message::Click { index: 1, alternate: false })
        .to_vec();
    assert_matches!(
        effects[..],
        [Effect::CountLaunch(_), Effect::Close, Effect::Spawn { .. }]
    );
}

#[test]
fn icons_resolve_with_the_fallback() {
    let temp = Temp::new("icons");
    let theme = Arc::new(common::icon_theme(&temp.0));
    let mut harness = Harness::new();
    harness.sources = common::sources(Some(Arc::clone(&theme)), Usage::default());
    let _ = harness.send(Message::Refresh);
    let names = harness.launcher.icon_names();
    assert!(names.contains(&String::from(launcher::FALLBACK_ICON)));
    let loaded = launcher::resolve_icons(&theme, names);
    let htop = loaded
        .iter()
        .find(|icon| icon.name == "htop")
        .unwrap();
    assert!(
        htop.path.is_none(),
        "htop has no icon in the theme"
    );
    let game = loaded
        .iter()
        .find(|icon| icon.name == "game")
        .unwrap();
    assert_matches!(
        &game.image,
        Some(Ok(image)) if image.width == launcher::ICON_PX,
        "the SVG is drawn at the icon size"
    );
    let foot = loaded
        .iter()
        .find(|icon| icon.name == "foot")
        .unwrap();
    assert_matches!(
        &foot.image,
        Some(Ok(image)) if image.width == launcher::ICON_PX,
        "a 64 px PNG shrinks to the icon size"
    );
    let generation = harness.launcher.icon_generation();
    let _ = harness.send(Message::Icons { generation, loaded });
}
