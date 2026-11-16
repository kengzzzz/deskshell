# deskshell

A desktop shell for Hyprland, written in Rust with [iced](https://github.com/iced-rs/iced).

It is one person's daily desktop, built for Arch Linux, Hyprland and a YubiKey. It works, but expect rough edges and opinions baked in.

## What is in it

| Binary | Does |
|---|---|
| `deskshell` | The bar, the panels (launcher, hub, calendar, clipboard history, media, notifications, overview, keybindings, updates, screen share picker), toasts, the polkit agent and the keyring prompt. It also starts the session (`deskshell session`). |
| `deskshell-lock` | The session lock. It runs on its own, so a shell crash never unlocks or blocks it. |
| `deskshell-pam` | A small PAM worker, one per unlock attempt, so a pending touch can always be cancelled. |
| `deskshell-greeter` | A greetd greeter. |
| `deskshell-gpg` | A proxy on gpg-agent's socket that tells the shell when GPG waits for a key touch. |
| `deskshell-bridge` | One-way bridges to a second user who runs apps (such as games) in the same Wayland session: a filtered Wayland socket, app launches, desktop shortcuts, notifications and tray. |

Services talk to the system over protocols (D-Bus, Wayland, the PulseAudio protocol, Hyprland IPC) instead of linking C libraries. The release binaries link only glibc, the unwinder and, for `deskshell-pam`, libpam.

## Build

The toolchain is pinned in `rust-toolchain.toml`.

```sh
cargo build --release
```

On Arch, `packaging/PKGBUILD` builds the checked out commit:

```sh
cd packaging && makepkg -si
systemctl --user enable --now deskshell.service
```

The greetd files land in `/usr/share/deskshell/greetd` and stay inactive until you copy them into `/etc`.

## Configure

Copy `crates/ui/theme.example.toml` to `~/.config/deskshell/theme.toml` and keep only the keys you change. The shell reloads it on save.

## Test

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Tests that need a compositor, PAM or greetd run in disposable KVM guests. See [`dev/vm/README.md`](dev/vm/README.md).

## License

MIT or Apache-2.0, at your option. The bundled fonts keep their own licenses (`crates/ui/assets/fonts`). `vendor/iced_wgpu` is a patched copy of iced_wgpu under MIT (see `vendor/README.md`).
