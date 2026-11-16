//! Made-up launcher fixtures: entries, bangs and icons drawn into a temp dir.

#![allow(
    dead_code,
    reason = "shared by several test binaries, each using part of it"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use deskshell::panel::Sources;
use deskshell::panel::launcher::{Bangs, Config, Shared, Usage};
use deskshell_apps::entry::{self, Parsed};
use deskshell_apps::{Dirs, IconTheme, Index, Locale, Snapshot};
use deskshell_core::launcher::bangs;
use image::{Rgba, RgbaImage};

pub const TENANT: &str = "/opt/sample/bin/runner";

pub const ENTRIES: [(&str, &str); 7] = [
    (
        "foot",
        "[Desktop Entry]\nType=Application\nName=Foot\nGenericName=Terminal\nComment=A fast terminal\n\
         Exec=foot\nIcon=foot\nKeywords=shell;prompt;\nCategories=System;TerminalEmulator;\nStartupWMClass=foot\n",
    ),
    (
        "htop",
        "[Desktop Entry]\nType=Application\nName=Top\nComment=Process viewer\nExec=htop\nTerminal=true\n\
         Path=/srv/work\nIcon=htop\nKeywords=process;cpu;\nCategories=System;Monitor;\n",
    ),
    (
        "files",
        "[Desktop Entry]\nType=Application\nName=Files\nGenericName=File Manager\nExec=files %U\nIcon=files\n\
         Categories=System;FileManager;\n",
    ),
    (
        "calc",
        "[Desktop Entry]\nType=Application\nName=เครื่องคิดเลข\nGenericName=Calculator\nComment=คำนวณตัวเลข\n\
         Exec=calc\nIcon=calc\nCategories=Utility;\n",
    ),
    (
        "game",
        "[Desktop Entry]\nType=Application\nName=Game\nExec=/opt/sample/bin/runner --run game\n\
         StartupWMClass=GameClass\nIcon=game\nCategories=Game;\n",
    ),
    (
        "browser",
        "[Desktop Entry]\nType=Application\nName=Browser\nComment=Browse the web\nExec=browser %u\nIcon=browser\n\
         Categories=Network;WebBrowser;\n",
    ),
    (
        "helper",
        "[Desktop Entry]\nType=Application\nName=Helper\nExec=helper\nNoDisplay=true\n",
    ),
];

pub const BANGS: &str = r#"[
    {"t": "yt", "s": "YouTube", "d": "www.youtube.com", "c": "Multimedia", "sc": "Video", "r": 90},
    {"t": "w", "s": "Wikipedia", "d": "en.wikipedia.org", "c": "Research", "sc": "Reference", "r": 50},
    {"t": "gh", "s": "GitHub", "d": "github.com", "c": "Tech", "r": 40}
]"#;

pub fn index() -> Index {
    let locale = Locale::parse("en_US.UTF-8");
    Index::from_parsed(
        ENTRIES
            .iter()
            .map(|(id, text)| -> Parsed { entry::parse(id, text, &locale) }),
    )
}

pub fn config() -> Config {
    Config {
        shell: String::from("/bin/zsh"),
        home: Some(PathBuf::from("/home/tester")),
        tenant_runner: Some(String::from(TENANT)),
        qalc: None,
    }
}

pub fn sources(icons: Option<Arc<IconTheme>>, usage: Usage) -> Sources {
    Sources {
        apps: Snapshot { index: Arc::new(index()), icons, scans: 1, icon_loads: 1 },
        bangs: Bangs { list: Arc::from(bangs::parse(BANGS).unwrap()), fetching: false },
        launcher: Shared { config: config(), usage },
        clipboard: deskshell::panel::clipboard::Config::default(),
    }
}

pub struct Temp(pub PathBuf);

impl Temp {
    pub fn new(name: &str) -> Self {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("launcher-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    /// Socket paths must stay under 108 bytes.
    pub fn short(name: &str) -> Self {
        let path = PathBuf::from(format!(
            "/tmp/dsh-launcher-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn icon_theme(root: &Path) -> IconTheme {
    let theme = root.join("icons/hicolor");
    let apps = theme.join("48x48/apps");
    let scalable = theme.join("scalable/apps");
    fs::create_dir_all(&apps).unwrap();
    fs::create_dir_all(&scalable).unwrap();
    fs::write(
        theme.join("index.theme"),
        "[Icon Theme]\nName=Hicolor\nDirectories=48x48/apps,scalable/apps\n\n[48x48/apps]\nSize=48\n\
         Type=Threshold\n\n[scalable/apps]\nSize=48\nType=Scalable\nMinSize=1\nMaxSize=512\n",
    )
    .unwrap();
    for (name, color) in [
        ("foot", [0x3d, 0x8b, 0xd9]),
        ("files", [0xe8, 0xa8, 0x38]),
        ("calc", [0x5a, 0xb5, 0x6e]),
        ("browser", [0xd9, 0x5a, 0x4a]),
    ] {
        disc(color)
            .save(apps.join(format!("{name}.png")))
            .unwrap();
    }
    fs::write(
        scalable.join("game.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48"><rect x="4" y="10" width="40" height="28" rx="10" fill="#8f6ee8"/><circle cx="16" cy="24" r="4" fill="#ffffff"/><circle cx="32" cy="24" r="4" fill="#ffffff"/></svg>"##,
    )
    .unwrap();
    fs::write(
        scalable.join("application-x-executable.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48"><rect x="6" y="6" width="36" height="36" rx="8" fill="#7a8194"/><path d="M16 24h16M24 16v16" stroke="#ffffff" stroke-width="4"/></svg>"##,
    )
    .unwrap();
    IconTheme::load(
        "hicolor",
        &Dirs { applications: Vec::new(), icons: vec![root.join("icons")], pixmaps: Vec::new() },
    )
}

fn disc(color: [u8; 3]) -> RgbaImage {
    RgbaImage::from_fn(64, 64, |x, y| {
        let (dx, dy) = (x as f32 - 31.5, y as f32 - 31.5);
        let distance = (dx * dx + dy * dy).sqrt();
        if distance > 30.0 {
            Rgba([0, 0, 0, 0])
        } else if distance > 22.0 {
            Rgba([color[0] / 2 + 120, color[1] / 2 + 120, color[2] / 2 + 120, 255])
        } else {
            Rgba([color[0], color[1], color[2], 255])
        }
    })
}
