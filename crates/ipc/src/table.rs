//! The call table: every target and function the control socket serves.

use std::fmt;
use std::time::Duration;

/// Past `deskshell-lock.service`'s `TimeoutStartSec` of 15 s.
pub const LOCK_WAIT: Duration = Duration::from_secs(20);
/// Past [`LOCK_WAIT`].
pub const LOCK_HANDLER_TIMEOUT: Duration = Duration::from_secs(22);
/// The install job holds a cancel up to 5 s for a runner connection, then waits up to 5 s
/// for the runner's answer.
pub const UPDATES_CANCEL_HANDLER_TIMEOUT: Duration = Duration::from_secs(12);
pub const SHARE_PICK_HANDLER_TIMEOUT: Duration = Duration::from_secs(300);

macro_rules! panel {
    ($name:literal, $variant:ident) => {
        Target {
            name: $name,
            functions: &[
                void("toggle", |_| Ok(Command::$variant(Panel::Toggle))),
                void("show", |_| Ok(Command::$variant(Panel::Show))),
                void("hide", |_| Ok(Command::$variant(Panel::Hide))),
            ],
        }
    };
}

pub static TARGETS: &[Target] = &[
    Target {
        name: "lock",
        functions: &[
            void("lock", |_| Ok(Command::Lock(Lock::Lock))),
            Function {
                name: "isLocked",
                params: &[],
                returns: ReturnType::Bool,
                build: |_| Ok(Command::Lock(Lock::IsLocked)),
            },
        ],
    },
    Target {
        name: "sharepicker",
        functions: &[Function {
            name: "pick",
            params: &[
                Param { name: "windowList", ty: ArgType::String },
                Param { name: "purpose", ty: ArgType::String },
            ],
            returns: ReturnType::String,
            build: |a| {
                Ok(Command::SharePicker(SharePicker::Pick {
                    window_list: a.text(0),
                    purpose: a.text(1),
                }))
            },
        }],
    },
    Target {
        name: "screenshot",
        functions: &[
            Function {
                name: "area",
                params: &[],
                returns: ReturnType::String,
                build: |_| Ok(Command::Screenshot(Screenshot::Area)),
            },
            Function {
                name: "full",
                params: &[],
                returns: ReturnType::String,
                build: |_| Ok(Command::Screenshot(Screenshot::Full)),
            },
        ],
    },
    Target {
        name: "media",
        functions: &[
            void("toggle", |_| Ok(Command::Media(Media::Toggle))),
            void("show", |_| Ok(Command::Media(Media::Show))),
            void("hide", |_| Ok(Command::Media(Media::Hide))),
            void("playPause", |_| {
                Ok(Command::Media(Media::PlayPause))
            }),
            void("pause", |_| Ok(Command::Media(Media::Pause))),
            void("next", |_| Ok(Command::Media(Media::Next))),
            void("previous", |_| {
                Ok(Command::Media(Media::Previous))
            }),
        ],
    },
    panel!("overview", Overview),
    panel!("launcher", Launcher),
    Target {
        name: "clipboard",
        functions: &[
            void("toggle", |_| {
                Ok(Command::Clipboard(Panel::Toggle))
            }),
            void("show", |_| Ok(Command::Clipboard(Panel::Show))),
            void("hide", |_| Ok(Command::Clipboard(Panel::Hide))),
            Function {
                name: "copyEpoch",
                params: &[],
                returns: ReturnType::String,
                build: |_| Ok(Command::ClipboardCopyEpoch),
            },
        ],
    },
    panel!("powermenu", PowerMenu),
    panel!("keybindings", Keybindings),
    panel!("calendar", Calendar),
    panel!("quicksettings", QuickSettings),
    Target {
        name: "hub",
        functions: &[
            void("toggle", |_| Ok(Command::Hub(Hub::Toggle))),
            void("show", |_| Ok(Command::Hub(Hub::Show))),
            void("hide", |_| Ok(Command::Hub(Hub::Hide))),
            void("toggleKeepAwake", |_| {
                Ok(Command::Hub(Hub::ToggleKeepAwake))
            }),
        ],
    },
    Target {
        name: "notifications",
        functions: &[
            void("toggle", |_| {
                Ok(Command::Notifications(Notifications::Toggle))
            }),
            void("show", |_| {
                Ok(Command::Notifications(Notifications::Show))
            }),
            void("hide", |_| {
                Ok(Command::Notifications(Notifications::Hide))
            }),
            void("toggleDnd", |_| {
                Ok(Command::Notifications(Notifications::ToggleDnd))
            }),
            void("clear", |_| {
                Ok(Command::Notifications(Notifications::Clear))
            }),
            Function {
                name: "count",
                params: &[],
                returns: ReturnType::String,
                build: |_| Ok(Command::Notifications(Notifications::Count)),
            },
        ],
    },
    Target {
        name: "status",
        functions: &[void("refreshUpdates", |_| {
            Ok(Command::Status(Status::RefreshUpdates))
        })],
    },
    Target {
        name: "updates",
        functions: &[
            void("start", |_| Ok(Command::Updates(Updates::Start))),
            void("cancel", |_| {
                Ok(Command::Updates(Updates::Cancel))
            }),
            void("toggle", |_| {
                Ok(Command::Updates(Updates::Toggle))
            }),
        ],
    },
    Target {
        name: "windows",
        functions: &[void("refresh", |_| {
            Ok(Command::Windows(Windows::Refresh))
        })],
    },
    Target {
        name: "display",
        functions: &[
            void("toggleHdr", |_| {
                Ok(Command::Display(Display::ToggleHdr))
            }),
            void("toggleVrr", |_| {
                Ok(Command::Display(Display::ToggleVrr))
            }),
            void("toggleNightLight", |_| {
                Ok(Command::Display(Display::ToggleNightLight))
            }),
        ],
    },
    Target {
        name: "cpupower",
        functions: &[
            void("cycle", |_| {
                Ok(Command::CpuPower(CpuPower::Cycle))
            }),
            void("details", |_| {
                Ok(Command::CpuPower(CpuPower::Details))
            }),
            Function {
                name: "set",
                params: &[Param { name: "mode", ty: ArgType::String }],
                returns: ReturnType::Void,
                build: |a| {
                    let mode = a.raw[0];
                    CpuPowerMode::parse(mode)
                        .map(|mode| Command::CpuPower(CpuPower::Set(mode)))
                        .ok_or_else(|| CallError::InvalidArgument {
                            param: "mode",
                            value: mode.to_owned(),
                            allowed: "eco, balanced, performance",
                        })
                },
            },
        ],
    },
];

const fn void(name: &'static str, build: Build) -> Function {
    Function { name, params: &[], returns: ReturnType::Void, build }
}

type Build = fn(&Args<'_>) -> Result<Command, CallError>;

#[derive(Debug)]
pub struct Target {
    pub name: &'static str,
    pub functions: &'static [Function],
}

impl Target {
    pub fn find(name: &str) -> Option<&'static Target> {
        TARGETS.iter().find(|t| t.name == name)
    }

    pub fn function(&self, name: &str) -> Option<&'static Function> {
        self.functions
            .iter()
            .find(|f| f.name == name)
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "target {}", self.name)?;
        for function in self.functions {
            write!(f, "\n  {function}")?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct Function {
    pub name: &'static str,
    pub params: &'static [Param],
    pub returns: ReturnType,
    build: Build,
}

impl fmt::Display for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "function {}(", self.name)?;
        for (i, param) in self.params.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{}: {}", param.name, param.ty)?;
        }
        write!(f, "): {}", self.returns)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Param {
    pub name: &'static str,
    pub ty: ArgType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgType {
    String,
}

impl fmt::Display for ArgType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::String => "string",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnType {
    Void,
    Bool,
    String,
}

impl fmt::Display for ReturnType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Void => "void",
            Self::Bool => "bool",
            Self::String => "string",
        })
    }
}

struct Args<'a> {
    raw: &'a [&'a str],
}

impl Args<'_> {
    fn text(&self, index: usize) -> String {
        self.raw[index].to_owned()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Lock(Lock),
    SharePicker(SharePicker),
    Screenshot(Screenshot),
    Media(Media),
    Overview(Panel),
    Launcher(Panel),
    Clipboard(Panel),
    /// The clipboard's requested clear epoch as decimal text, for the copy marker of
    /// `deskshell clipboard copy --png`. It reads only.
    ClipboardCopyEpoch,
    PowerMenu(Panel),
    Keybindings(Panel),
    Calendar(Panel),
    QuickSettings(Panel),
    Hub(Hub),
    Notifications(Notifications),
    Status(Status),
    Updates(Updates),
    Windows(Windows),
    Display(Display),
    CpuPower(CpuPower),
}

impl Command {
    pub fn parse(target: &str, function: &str, args: &[&str]) -> Result<Self, CallError> {
        let target = Target::find(target).ok_or(CallError::TargetNotFound)?;
        let function = target
            .function(function)
            .ok_or(CallError::FunctionNotFound)?;
        if args.len() != function.params.len() {
            return Err(CallError::ArgCount { function, provided: args.len() });
        }
        (function.build)(&Args { raw: args })
    }

    pub fn handler_timeout(&self) -> Option<Duration> {
        match self {
            Self::Lock(Lock::Lock) => Some(LOCK_HANDLER_TIMEOUT),
            Self::Updates(Updates::Cancel) => Some(UPDATES_CANCEL_HANDLER_TIMEOUT),
            Self::SharePicker(SharePicker::Pick { .. }) => Some(SHARE_PICK_HANDLER_TIMEOUT),
            _ => None,
        }
    }

    /// Only `sharepicker pick`: a picker whose caller is gone must close. A lock or an update
    /// cancel runs on without its caller.
    pub fn cancel_on_hangup(&self) -> bool {
        matches!(self, Self::SharePicker(SharePicker::Pick { .. }))
    }

    pub fn returns(&self) -> ReturnType {
        match self {
            Self::Lock(Lock::IsLocked) => ReturnType::Bool,
            Self::SharePicker(SharePicker::Pick { .. })
            | Self::Screenshot(Screenshot::Area | Screenshot::Full)
            | Self::Notifications(Notifications::Count)
            | Self::ClipboardCopyEpoch => ReturnType::String,
            _ => ReturnType::Void,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lock {
    Lock,
    IsLocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SharePicker {
    /// Answers `[r]/screen:NAME`, `[r]/window:ID`, `[r]/region:NAME@X,Y,W,H`, or empty for a
    /// cancel. `purpose` is `share` or `record`.
    Pick { window_list: String, purpose: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screenshot {
    /// Answers `ok`, `locked` or `busy` at once, and the flow runs in the shell.
    Area,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Media {
    Toggle,
    Show,
    Hide,
    PlayPause,
    Pause,
    Next,
    Previous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Toggle,
    Show,
    Hide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hub {
    Toggle,
    Show,
    Hide,
    ToggleKeepAwake,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notifications {
    Toggle,
    Show,
    Hide,
    ToggleDnd,
    Clear,
    /// The history length as decimal text, which the lock shows.
    Count,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    RefreshUpdates,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Updates {
    Start,
    Cancel,
    Toggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Windows {
    Refresh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    ToggleHdr,
    ToggleVrr,
    ToggleNightLight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuPower {
    Cycle,
    Details,
    Set(CpuPowerMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuPowerMode {
    Eco,
    Balanced,
    Performance,
}

impl CpuPowerMode {
    pub fn parse(mode: &str) -> Option<Self> {
        match mode {
            "eco" => Some(Self::Eco),
            "balanced" => Some(Self::Balanced),
            "performance" => Some(Self::Performance),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Unit,
    Bool(bool),
    Text(String),
}

impl Reply {
    pub fn kind(&self) -> ReturnType {
        match self {
            Self::Unit => ReturnType::Void,
            Self::Bool(_) => ReturnType::Bool,
            Self::Text(_) => ReturnType::String,
        }
    }
}

#[derive(Debug, Clone)]
pub enum CallError {
    TargetNotFound,
    FunctionNotFound,
    ArgCount {
        function: &'static Function,
        provided: usize,
    },
    /// An argument outside the values a function takes.
    InvalidArgument {
        param: &'static str,
        value: String,
        allowed: &'static str,
    },
}

impl CallError {
    pub fn definition(&self) -> Option<&'static Function> {
        match self {
            Self::TargetNotFound | Self::FunctionNotFound => None,
            Self::ArgCount { function, .. } => Some(function),
            Self::InvalidArgument { .. } => None,
        }
    }

    pub fn report(&self) -> String {
        match self.definition() {
            Some(function) => format!("{self}\nFunction definition: {function}"),
            None => self.to_string(),
        }
    }
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TargetNotFound => f.write_str("Target not found."),
            Self::FunctionNotFound => f.write_str("Function not found."),
            Self::ArgCount { function, provided } => {
                let required = function.params.len();
                let quantity = if required < *provided { "many" } else { "few" };
                write!(
                    f,
                    "Too {quantity} arguments provided ({required} required but {provided} were provided.)"
                )
            }
            Self::InvalidArgument { param, value, allowed } => {
                write!(
                    f,
                    "Invalid {param}: {value:?} (one of {allowed})."
                )
            }
        }
    }
}

impl std::error::Error for CallError {}
