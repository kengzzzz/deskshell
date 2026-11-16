# syntax=docker/dockerfile:1
ARG BASE_IMAGE=archlinux:base@sha256:d4594bb2300cba11f41c2f27923eb56f63d9988909c402f9c95517d1e362c899

FROM ${BASE_IMAGE} AS helpers
ARG SYNC_STAMP
RUN --mount=type=cache,id=deskshell-vm-pacman,target=/var/cache/pacman/pkg,sharing=locked \
    echo "sync $SYNC_STAMP" && pacman -Syu --noconfirm --needed gcc
COPY pam-fakeu2f/ /src/pam-fakeu2f/
COPY fakehid/ /src/fakehid/
RUN mkdir /out && \
    cflags='-O2 -Wall -Wextra -Werror -D_GNU_SOURCE' && \
    gcc $cflags -shared -fPIC -o /out/pam_fakeu2f.so /src/pam-fakeu2f/pam_fakeu2f.c -lpam && \
    gcc $cflags -o /out/pam-try /src/pam-fakeu2f/pam-try.c -lpam && \
    gcc $cflags -o /out/fake-yubikey /src/fakehid/fake-yubikey.c

FROM ${BASE_IMAGE}
ARG SYNC_STAMP
COPY rootfs/etc/mkinitcpio.conf.d/ /etc/mkinitcpio.conf.d/
RUN --mount=type=cache,id=deskshell-vm-pacman,target=/var/cache/pacman/pkg,sharing=locked \
    echo "sync $SYNC_STAMP" && \
    pacman -Syu --noconfirm --needed \
        linux mkinitcpio e2fsprogs openssh greetd dbus-broker-units \
        hyprland mesa vulkan-swrast grim wtype foot wayvnc ttf-dejavu strace polkit \
        pipewire pipewire-pulse wireplumber libpulse cliphist wl-clipboard \
        hyprsunset xdg-desktop-portal xdg-desktop-portal-hyprland && \
    rm -f /boot/initramfs-linux-fallback.img
COPY rootfs/ /
COPY --from=helpers /out/pam_fakeu2f.so /usr/lib/security/
COPY --from=helpers /out/pam-try /out/fake-yubikey /usr/local/bin/
RUN useradd -m -u 1000 -U -s /bin/bash tester && \
    echo tester:tester | chpasswd && \
    systemd-machine-id-setup && \
    systemctl enable sshd systemd-networkd greetd && \
    systemctl mask systemd-networkd-wait-online.service && \
    ssh-keygen -A && \
    systemd-hwdb update && \
    journalctl --update-catalog && \
    ldconfig && \
    install -d -m 0700 /root/.ssh
COPY --chown=1000:1000 tester/ /home/tester/
COPY key.pub /root/.ssh/authorized_keys
RUN chmod 0600 /root/.ssh/authorized_keys && touch /etc/.updated /var/.updated
