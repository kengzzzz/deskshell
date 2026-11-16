#!/bin/sh
# greetd's default_session command. It runs the greeter's own Hyprland, which runs
# deskshell-greeter, and waits on the VT with a retry prompt when the greeter never came
# up or exited without a login.
greeter=${DESKSHELL_GREETER:-/usr/bin/deskshell-greeter}
config=${DESKSHELL_GREETER_HYPRLAND:-/etc/greetd/deskshell-hyprland.lua}
ready="$XDG_RUNTIME_DIR/deskshell-greeter-ready"
launched="$XDG_RUNTIME_DIR/deskshell-greeter-launched"

# The Hyprland config starts the greeter from this.
export DESKSHELL_GREETER="$greeter"
export HYPRLAND_NO_RT=1
export HYPRLAND_NO_SD_VARS=1
export XDG_CACHE_HOME=/var/cache/deskshell-greeter
export XDG_STATE_HOME=/var/cache/deskshell-greeter
export XDG_DATA_HOME=/var/cache/deskshell-greeter

# A GPU driver that loads from udev can lose the race with greetd at boot.
i=0
while [ "$i" -lt 50 ]; do
    set -- /dev/dri/renderD*
    [ -e "$1" ] && break
    sleep 0.2
    i=$((i + 1))
done

# greetd sends SIGTERM only to this shell when the greeter is slow to exit after a
# login, and SIGKILL 5 s later, so pass it on instead of orphaning the compositor.
trap 'stopped=1; kill -TERM "$pid" 2>/dev/null; (sleep 3; kill -KILL "$pid" 2>/dev/null) &' TERM HUP
# Ctrl+C or Ctrl+\ at the retry prompt must not end the greeter.
trap '' INT QUIT TSTP

# greetd keeps a half finished login when its greeter dies, and the next greeter would
# read that login's replies as its own. A PAM call that still runs holds the cancel
# back, and greetd cancels whatever login exists when it finally fails to answer a client
# that gave up, so the next greeter starts only after a clean result. After such a wait
# cancel-stale also gives greetd half a second to settle. It says on the VT when it has
# to wait, and gives up after 10 min. It runs in the background so the TERM trap still
# works.
cancel_stale() {
    "$greeter" cancel-stale &
    pid=$!
    wait "$pid"
    status=$?
    # A trap interrupts wait with a status over 128 while the child still runs.
    while [ "$status" -gt 128 ] && kill -0 "$pid" 2>/dev/null; do
        wait "$pid"
        status=$?
    done
    return "$status"
}

run_greeter() {
    rm -f "$ready" "$launched"
    systemd-cat --identifier=deskshell-greeter Hyprland -c "$config" <&0 &
    pid=$!

    # A compositor or greeter that hangs before it is up would hold the VT forever.
    (
        sleep 30
        [ -e "$ready" ] && exit 0
        kill -TERM "$pid" 2>/dev/null
        sleep 3
        kill -KILL "$pid" 2>/dev/null
    ) &
    guard=$!

    while kill -0 "$pid" 2>/dev/null; do
        wait "$pid"
    done
    kill "$guard" 2>/dev/null
}

# The greeter never came up, crashed or was closed with Ctrl+Alt+Backspace. Exiting here
# would make greetd exit and restart until systemd gives up, so wait on the VT and try
# again.
while :; do
    if cancel_stale; then
        [ -n "${stopped:-}" ] && exit 0
        run_greeter
        [ -e "$launched" ] || [ -n "${stopped:-}" ] && exit 0
        problem="The deskshell greeter did not start."
    else
        [ -n "${stopped:-}" ] && exit 0
        problem="greetd did not answer, so the greeter did not start."
    fi
    printf '\033[2J\033[H%s\n\n%s\n%s\n%s\n' \
        "$problem" \
        "Press Enter to try again." \
        "Ctrl+Alt+F2 opens a text login." \
        "Logs: journalctl -b -t deskshell-greeter"
    read -r _ || sleep 1
    [ -n "${stopped:-}" ] && exit 0
done
