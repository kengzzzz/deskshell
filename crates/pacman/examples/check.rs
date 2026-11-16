//! Runs the native update check by hand, to compare it with `checkupdates` and `paru -Qua`.
//!
//! `check CACHE_DIR [PACMAN_CONF]` prints `name old -> new`, official first, then the AUR
//! after a `--- aur` line. `check servers [PACMAN_CONF]` prints each repo's expanded servers.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use deskshell_pacman::{AUR_INFO, Config, PACMAN_CONF, PacmanConf, check};

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let Some(first) = args.next() else {
        eprintln!("usage: check CACHE_DIR [PACMAN_CONF] | check servers [PACMAN_CONF]");
        return ExitCode::from(2);
    };
    let conf = args
        .next()
        .map_or_else(|| PathBuf::from(PACMAN_CONF), PathBuf::from);
    if first == "servers" {
        return servers(&conf);
    }
    run(first, conf)
}

fn servers(conf: &std::path::Path) -> ExitCode {
    match PacmanConf::load(conf, std::env::consts::ARCH) {
        Ok(conf) => {
            for repo in &conf.repos {
                println!("[{}]", repo.name);
                for server in &repo.servers {
                    println!("{server}");
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cache: OsString, conf: PathBuf) -> ExitCode {
    let config = Config {
        conf,
        sync_dir: PathBuf::from(cache),
        aur: AUR_INFO.to_owned(),
        machine: std::env::consts::ARCH.to_owned(),
        deadline: deskshell_pacman::DEADLINE,
        limits: deskshell_pacman::Limits::default(),
        env_proxy: true,
    };
    match check(&config) {
        Ok(report) => {
            let mut aur = false;
            for update in &report.updates {
                if update.aur && !aur {
                    println!("--- aur");
                    aur = true;
                }
                println!("{} {} -> {}", update.name, update.old, update.new);
            }
            if let Some(error) = report.aur_error {
                eprintln!("the AUR failed: {error}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
