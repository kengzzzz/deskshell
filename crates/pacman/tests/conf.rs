//! `pacman.conf` as pacman reads it.

mod common;

use std::fs;
use std::path::PathBuf;

use common::TempDir;
use deskshell_pacman::{PacmanConf, Repo};

fn repo(name: &str, servers: &[&str]) -> Repo {
    Repo {
        name: name.to_owned(),
        servers: servers
            .iter()
            .map(|server| (*server).to_owned())
            .collect(),
    }
}

#[test]
fn includes_variables_and_order() {
    let dir = TempDir::new("conf");
    let d = dir.path.display();
    fs::create_dir_all(dir.path.join("mirrors.d")).unwrap();
    fs::write(
        dir.path.join("mirrors.d/10-first"),
        "# a comment\nServer = https://one.example/$arch_v4/$repo/  # trailing\n",
    )
    .unwrap();
    fs::write(
        dir.path.join("mirrors.d/20-second"),
        "Server = https://two.example/$repo/os/$arch\nServer = https://bad.example/$basearch/$repo\n",
    )
    .unwrap();
    fs::write(
        dir.path.join("mirrors.d/.hidden"),
        "Server = https://hidden.example/$repo\n",
    )
    .unwrap();
    fs::write(
        dir.path.join("mirrorlist"),
        "Server = https://arch.example/$repo/os/$arch\n",
    )
    .unwrap();
    fs::write(
        dir.path.join("options.conf"),
        "IgnoreGroup = gnome*\n",
    )
    .unwrap();
    fs::write(
        dir.path.join("pacman.conf"),
        format!(
            "[options]\n\
             HoldPkg = pacman glibc\n\
             Architecture = x86_64 x86_64_v3 x86_64_v4\n\
             DBPath = {d}/db/\n\
             IgnorePkg = linux* nvidia-?? \n\
             IgnorePkg = foo\n\
             Include = {d}/options.conf\n\
             CheckSpace\n\
             SigLevel = Required DatabaseOptional\n\
             \n\
             [zeta-v4]\n\
             Include = {d}/mirrors.d/*\n\
             \n\
             [core]\n\
             Include = {d}/mirrorlist\n\
             [alpha]\n\
             SigLevel = Optional\n\
             Server = file:///srv/$repo\n"
        ),
    )
    .unwrap();
    let conf = PacmanConf::load(&dir.path.join("pacman.conf"), "aarch64").unwrap();
    assert_eq!(
        conf,
        PacmanConf {
            db_path: PathBuf::from(format!("{d}/db/")),
            arch: String::from("x86_64"),
            ignore_pkg: vec![
                String::from("linux*"),
                String::from("nvidia-??"),
                String::from("foo")
            ],
            ignore_group: vec![String::from("gnome*")],
            repos: vec![
                repo(
                    "zeta-v4",
                    &[
                        "https://one.example/x86_64_v4/zeta-v4",
                        "https://two.example/zeta-v4/os/x86_64"
                    ]
                ),
                repo("core", &["https://arch.example/core/os/x86_64"]),
                repo("alpha", &["file:///srv/alpha"]),
            ],
        }
    );
    assert!(conf.ignored("linux-zen", ""));
    assert!(conf.ignored("nvidia-dk", ""));
    assert!(!conf.ignored("nvidia-dkms", ""));
    assert!(!conf.ignored("foobar", ""));
    assert!(conf.ignored("bar", "xorg\ngnome-extra"));
    assert!(!conf.ignored("bar", "xorg"));
}

#[test]
fn auto_and_defaults() {
    let dir = TempDir::new("conf-auto");
    let path = dir.path.join("pacman.conf");
    fs::write(
        &path,
        "[options]\nArchitecture = auto\n[core]\nServer = https://m.example/$repo/os/$arch\n",
    )
    .unwrap();
    let conf = PacmanConf::load(&path, "x86_64").unwrap();
    assert_eq!(conf.arch, "x86_64");
    assert_eq!(conf.db_path, PathBuf::from("/var/lib/pacman/"));
    assert_eq!(
        conf.repos,
        [repo("core", &["https://m.example/core/os/x86_64"])]
    );

    fs::write(
        &path,
        "[options]\n[core]\nServer = https://m.example/$arch\n",
    )
    .unwrap();
    let conf = PacmanConf::load(&path, "riscv64").unwrap();
    assert_eq!(conf.arch, "riscv64", "no Architecture is auto");
}

#[test]
fn failures() {
    let dir = TempDir::new("conf-bad");
    let path = dir.path.join("pacman.conf");
    assert!(
        PacmanConf::load(&path, "x86_64")
            .unwrap_err()
            .contains("cannot read")
    );
    fs::write(
        &path,
        format!(
            "[core]\nInclude = {}/none-*\n",
            dir.path.display()
        ),
    )
    .unwrap();
    assert!(
        PacmanConf::load(&path, "x86_64")
            .unwrap_err()
            .contains("names no file")
    );
    fs::write(&path, "Server = https://m.example\n").unwrap();
    assert!(
        PacmanConf::load(&path, "x86_64")
            .unwrap_err()
            .contains("outside a section")
    );
    fs::write(
        &path,
        format!("[options]\nInclude = {}\n", path.display()),
    )
    .unwrap();
    assert!(
        PacmanConf::load(&path, "x86_64")
            .unwrap_err()
            .contains("too deep")
    );
}
