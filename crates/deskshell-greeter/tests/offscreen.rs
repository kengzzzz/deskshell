//! Renders the greeter's views with no compositor to `target/tmp/offscreen/greeter/*.png`.

use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use deskshell_auth::faillock::{Locked, Status};
use deskshell_greeter::clock::Clock;
use deskshell_greeter::flow::{Event, Flow, Session, Timer};
use deskshell_greeter::greetd::{AuthMessageType, ErrorType, Kind, Reply, Response};
use deskshell_greeter::view::{self, Login, Scene, Texts, Ui};
use deskshell_ui::{Theme, fonts};
use iced::advanced::graphics::text::font_system;
use iced::advanced::renderer::{self, Headless};
use iced::advanced::{clipboard, mouse};
use iced::window::{self, RedrawRequest};
use iced::{Color, Element, Event as IcedEvent, Pixels, Size};
use iced_test::futures::futures::executor::block_on;
use iced_test::runtime::user_interface::{self, UserInterface};

const FRAME: Duration = Duration::from_micros(16_667);
const SIZE: Size = Size::new(1920.0, 1080.0);
/// One full touch pulse, so the ring is back at its rest size.
const PULSE: Duration = Duration::from_millis(1400);

struct Surface {
    renderer: iced::Renderer,
    cache: user_interface::Cache,
}

impl Surface {
    fn new() -> Self {
        let mut system = font_system()
            .write()
            .expect("font system");
        for font in fonts::all() {
            system.load_font(Cow::Borrowed(font));
        }
        drop(system);
        let renderer = block_on(iced::Renderer::new(
            fonts::PLEX,
            Pixels(16.0),
            Some("tiny-skia"),
        ))
        .expect("a tiny-skia renderer");
        Self { renderer, cache: user_interface::Cache::new() }
    }

    fn frame(
        &mut self,
        view: Element<'_, Ui>,
        now: Instant,
        render: Option<&Path>,
    ) -> RedrawRequest {
        let cache = std::mem::replace(&mut self.cache, user_interface::Cache::new());
        let mut ui = UserInterface::build(view, SIZE, cache, &mut self.renderer);
        let mut messages = Vec::new();
        let (state, _) = ui.update(
            &[IcedEvent::Window(window::Event::RedrawRequested(now))],
            mouse::Cursor::Unavailable,
            &mut self.renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        let redraw = match state {
            user_interface::State::Updated { redraw_request, .. } => redraw_request,
            user_interface::State::Outdated => RedrawRequest::NextFrame,
        };
        if let Some(path) = render {
            ui.draw(
                &mut self.renderer,
                &iced::Theme::Dark,
                &renderer::Style { text_color: Color::WHITE },
                mouse::Cursor::Unavailable,
            );
            let (width, height) = (SIZE.width as u32, SIZE.height as u32);
            let rgba = self
                .renderer
                .screenshot(Size::new(width, height), 1.0, view::BACKGROUND);
            image::save_buffer(
                path,
                &rgba,
                width,
                height,
                image::ColorType::Rgba8,
            )
            .expect("write PNG");
        }
        self.cache = ui.into_cache();
        redraw
    }
}

struct State {
    flow: Result<Flow, String>,
    texts: Texts,
    run: Duration,
}

fn session() -> Session {
    Session {
        name: String::from("Hyprland"),
        command: vec![String::from("Hyprland")],
        env: Vec::new(),
    }
}

fn users(names: &[&str], selected: Option<&str>) -> Flow {
    let names: Vec<String> = names
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    Flow::new(names.clone(), Some(names), session(), selected)
}

fn reply(kind: Kind, response: Response) -> Event {
    Event::Greetd(Reply { attempt: 1, kind, result: Ok(response) })
}

fn submitted(flow: &mut Flow) {
    for c in "123456".chars() {
        flow.handle(Event::Type(c));
    }
    flow.handle(Event::Submit);
    let check = flow
        .pending_key_check()
        .expect("a key check");
    flow.handle(Event::KeyChecked { check, present: true });
}

fn prompted(flow: &mut Flow) {
    submitted(flow);
    flow.handle(reply(
        Kind::CreateSession,
        Response::AuthMessage {
            auth_message_type: AuthMessageType::Secret,
            auth_message: String::from("Please enter the PIN: "),
        },
    ));
}

fn status(attempts_left: u32, locked: Option<Locked>) -> Event {
    Event::Faillock {
        user: String::from("alice"),
        status: Status {
            known: true,
            failures: 3 - attempts_left,
            attempts_left: Some(attempts_left),
            locked,
        },
    }
}

fn state(name: &str) -> State {
    let mut flow = users(&["alice"], None);
    let mut run = Duration::from_millis(600);
    match name {
        "idle" => {}
        "multi" => flow = users(&["alice", "tester", "bob"], Some("tester")),
        "typing" => {
            for c in "123456".chars() {
                flow.handle(Event::Type(c));
            }
        }
        "checking" => submitted(&mut flow),
        "wrong" => {
            prompted(&mut flow);
            flow.handle(reply(
                Kind::PostAuthMessageResponse,
                Response::Error {
                    error_type: ErrorType::AuthError,
                    description: String::from("Authentication failure"),
                },
            ));
            flow.handle(status(2, None));
        }
        "locked" => {
            flow.handle(status(0, Some(Locked::Until(4_000_000_000))));
        }
        "nokey" => {
            flow.handle(Event::PollKey);
            flow.handle(Event::KeyChecked { check: 1, present: false });
        }
        "unregistered" => {
            flow = Flow::new(
                vec![String::from("alice")],
                Some(Vec::new()),
                session(),
                None,
            );
        }
        "touch" => {
            prompted(&mut flow);
            flow.handle(reply(
                Kind::PostAuthMessageResponse,
                Response::AuthMessage {
                    auth_message_type: AuthMessageType::Info,
                    auth_message: String::from("Please touch the FIDO authenticator."),
                },
            ));
            run = PULSE;
        }
        "stalled" => {
            prompted(&mut flow);
            flow.handle(reply(
                Kind::PostAuthMessageResponse,
                Response::AuthMessage {
                    auth_message_type: AuthMessageType::Info,
                    auth_message: String::from("Please touch the FIDO authenticator."),
                },
            ));
            flow.handle(Event::Timer(Timer::Watchdog(1)));
        }
        "starting" => {
            prompted(&mut flow);
            flow.handle(reply(
                Kind::PostAuthMessageResponse,
                Response::Success,
            ));
        }
        "broken" => {
            return State {
                flow: Err(String::from(
                    "/etc/deskshell/greeter.toml is missing",
                )),
                texts: Texts::default(),
                run,
            };
        }
        other => panic!("no state {other}"),
    }
    let mut texts = Texts::new(&flow, 600, 1000);
    if name == "locked" {
        texts.locked_placeholder = String::from("Locked until 07:15");
        texts.locked_status = String::from("Too many failed attempts · try again in 10 min");
    }
    State { flow: Ok(flow), texts, run }
}

fn scene<'a>(theme: &'a Theme, clock: &'a Clock, state: &'a State) -> Scene<'a> {
    let login = match &state.flow {
        Ok(flow) => Login::Ready { flow, texts: &state.texts, avatars: &[] },
        Err(reason) => Login::Broken(reason),
    };
    Scene { theme, login, host: "deskshell-vm", clock, wallpaper: None, hovered: None }
}

fn out_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("offscreen/greeter");
    fs::create_dir_all(&dir).expect("output dir");
    dir
}

#[test]
fn renders_every_view() {
    let theme = Theme::default();
    let clock = Clock { date: String::from("Friday, 13 November"), time: String::from("07:05") };
    let dir = out_dir();
    for name in [
        "idle",
        "multi",
        "typing",
        "checking",
        "wrong",
        "locked",
        "nokey",
        "unregistered",
        "touch",
        "stalled",
        "starting",
        "broken",
    ] {
        let state = state(name);
        let mut surface = Surface::new();
        let start = Instant::now();
        let mut now = start;
        while now - start < state.run {
            surface.frame(
                view::view(scene(&theme, &clock, &state)),
                now,
                None,
            );
            now += FRAME;
        }
        let path = dir.join(format!("{name}.png"));
        surface.frame(
            view::view(scene(&theme, &clock, &state)),
            now,
            Some(&path),
        );
        if name != "touch" {
            assert_eq!(
                surface.frame(
                    view::view(scene(&theme, &clock, &state)),
                    now + FRAME,
                    None
                ),
                RedrawRequest::Wait,
                "{name} rests after the fade"
            );
        }
    }
    println!("greeter renders in {}", dir.display());
}
