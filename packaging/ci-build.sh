#!/usr/bin/env bash
# Builds the package as root in an archlinux:base-devel container. Used by the release workflow.
# Usage: [TARGET_CPU=znver5] packaging/ci-build.sh OUT_DIR
set -euo pipefail

out=${1:?usage: ci-build.sh OUT_DIR}
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

pacman -Syu --noconfirm --needed git rustup

git config --global --add safe.directory "$repo"
git config --global --add safe.directory "$repo/.git"
[[ $(git -C "$repo" rev-parse --is-shallow-repository) == false ]] || {
    printf 'shallow checkout: pkgver counts commits, so fetch the full history\n' >&2
    exit 1
}

id builder &>/dev/null || useradd -m builder
work=/home/builder/deskshell
rm -rf "$work"
# makepkg runs as a user and rewrites pkgver in the PKGBUILD, so it builds from a private clone.
git clone --quiet --no-hardlinks "$repo" "$work"
git -C "$work" checkout --quiet --detach "$(git -C "$repo" rev-parse HEAD)"
mkdir -p "$out"
if [[ -n ${TARGET_CPU:-} ]]; then
    mkdir -p /home/builder/.config/pacman
    printf 'RUSTFLAGS+=" -C target-cpu=%s"\n' "$TARGET_CPU" >/home/builder/.config/pacman/makepkg.conf
fi
chown -R builder: /home/builder "$out"

runuser -u builder -- bash -c 'cd "$1" && rustup toolchain install' bash "$work"
mapfile -t deps < <(runuser -u builder -- bash -c 'cd "$1/packaging" && makepkg --printsrcinfo' bash "$work" \
    | sed -n 's/^\t\(make\)\?depends = //p')
mapfile -t missing < <(pacman -T "${deps[@]}" || true)
((${#missing[@]} == 0)) || pacman -S --noconfirm --needed --asdeps "${missing[@]}"
runuser -u builder -- bash -c 'cd "$1/packaging" && PKGDEST="$2" makepkg --noconfirm' bash "$work" "$out"

cd "$out"
sha256sum -- *.pkg.tar.zst >SHA256SUMS
cat SHA256SUMS
