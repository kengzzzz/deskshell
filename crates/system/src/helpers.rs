//! Pure helpers the bar, the Hub and the update popover share.

use crate::model::Package;

pub const FLAG_THAI: &str = "\u{1F1F9}\u{1F1ED}";
pub const FLAG_US: &str = "\u{1F1FA}\u{1F1F8}";
pub const FLAG_OTHER: &str = "\u{1F3F3}\u{FE0F}";

pub fn keyboard_flag(layout: &str) -> &'static str {
    if contains_ignore_ascii_case(layout, "thai") || layout.eq_ignore_ascii_case("th") {
        FLAG_THAI
    } else if contains_ignore_ascii_case(layout, "english (us)")
        || layout.eq_ignore_ascii_case("us")
    {
        FLAG_US
    } else {
        FLAG_OTHER
    }
}

fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

pub fn hdr_enabled(color_management_preset: &str) -> bool {
    color_management_preset == "hdr"
}

const REBOOT_NAMES: [&str; 5] = ["systemd", "systemd-libs", "glibc", "amd-ucode", "intel-ucode"];
const LINUX_EXTRAS: [&str; 4] = ["-headers", "-docs", "-api-headers", "-tools"];

pub fn is_reboot_package(name: &str) -> bool {
    if REBOOT_NAMES.contains(&name) || name.starts_with("nvidia") {
        return true;
    }
    let kernel = name == "linux"
        || name
            .strip_prefix("linux-")
            .is_some_and(|rest| {
                !rest.is_empty()
                    && rest
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
            });
    kernel
        && !LINUX_EXTRAS
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

pub fn reboot_packages(packages: &[Package]) -> impl Iterator<Item = &Package> {
    packages
        .iter()
        .filter(|package| package.reboot)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UpdateCounts {
    pub total: u32,
    pub official: u32,
    pub aur: u32,
}

pub fn update_counts(packages: &[Package]) -> UpdateCounts {
    let aur = packages
        .iter()
        .filter(|package| package.aur)
        .count();
    let total = packages.len();
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    UpdateCounts { total: count(total), official: count(total - aur), aur: count(aur) }
}

pub fn split_updates(packages: &[Package]) -> (Vec<&Package>, Vec<&Package>) {
    packages
        .iter()
        .partition(|package| !package.aur)
}

pub fn install_updates_argv() -> Vec<String> {
    vec![
        "kitty".to_owned(),
        "--class".to_owned(),
        "deskshell-floating".to_owned(),
        "-e".to_owned(),
        "deskshell".to_owned(),
        "update-terminal".to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_flags() {
        assert_eq!(keyboard_flag("Thai"), FLAG_THAI);
        assert_eq!(keyboard_flag("Thai (Pattachote)"), FLAG_THAI);
        assert_eq!(keyboard_flag("TH"), FLAG_THAI);
        assert_eq!(keyboard_flag("English (US)"), FLAG_US);
        assert_eq!(
            keyboard_flag("english (us, intl., with dead keys)"),
            FLAG_OTHER
        );
        assert_eq!(keyboard_flag("English (US, intl.)"), FLAG_OTHER);
        assert_eq!(keyboard_flag("us"), FLAG_US);
        assert_eq!(keyboard_flag("German"), FLAG_OTHER);
        assert_eq!(keyboard_flag(""), FLAG_OTHER);
        assert_eq!(keyboard_flag("thx"), FLAG_OTHER);
    }

    #[test]
    fn hdr() {
        assert!(hdr_enabled("hdr"));
        assert!(!hdr_enabled("srgb"));
        assert!(!hdr_enabled("HDR"));
        assert!(!hdr_enabled(""));
    }

    #[test]
    fn reboot_rule() {
        for name in [
            "systemd",
            "systemd-libs",
            "glibc",
            "amd-ucode",
            "intel-ucode",
            "nvidia",
            "nvidia-utils",
            "nvidia-open-dkms",
            "linux",
            "linux-zen",
            "linux-lts",
            "linux-cachyos-eevdf",
            "linux-firmware",
            "linux-firmware-nvidia",
        ] {
            assert!(is_reboot_package(name), "{name}");
        }
        for name in [
            "systemd-sysvcompat",
            "glibc-locales",
            "lib32-nvidia-utils",
            "linux-headers",
            "linux-zen-headers",
            "linux-api-headers",
            "linux-docs",
            "linux-tools",
            "linux-",
            "linuxconsole",
            "util-linux",
            "linux-zen+git",
            "firefox",
        ] {
            assert!(!is_reboot_package(name), "{name}");
        }
    }

    fn package(name: &str, aur: bool) -> Package {
        Package {
            name: name.to_owned(),
            aur,
            reboot: is_reboot_package(name),
            ..Package::default()
        }
    }

    #[test]
    fn counts_and_split() {
        let packages = [package("glibc", false), package("firefox", false), package("paru", true)];
        assert_eq!(
            update_counts(&packages),
            UpdateCounts { total: 3, official: 2, aur: 1 }
        );
        let (official, aur) = split_updates(&packages);
        assert_eq!(
            official
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["glibc", "firefox"]
        );
        assert_eq!(aur.len(), 1);
        assert_eq!(
            reboot_packages(&packages)
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["glibc"]
        );
        assert_eq!(update_counts(&[]), UpdateCounts::default());
    }

    #[test]
    fn install_argv() {
        assert_eq!(
            install_updates_argv(),
            ["kitty", "--class", "deskshell-floating", "-e", "deskshell", "update-terminal"]
        );
    }
}
