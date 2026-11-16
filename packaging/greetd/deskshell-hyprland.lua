-- The greeter's own Hyprland. deskshell-greeter.sh starts it with -c.

hl.monitor({
    output   = "",
    mode     = "preferred",
    position = "auto",
    scale    = 1,
})

hl.config({
    ecosystem = {
        no_update_news  = true,
        no_donation_nag = true,
    },
    misc = {
        disable_hyprland_logo           = true,
        disable_splash_rendering        = true,
        disable_hyprland_guiutils_check = true,
        disable_watchdog_warning        = true,
        vrr                             = 0,
        mouse_move_enables_dpms         = true,
        key_press_enables_dpms          = true,
        enable_anr_dialog               = false,
    },
    xwayland = {
        enabled = false,
    },
    input = {
        kb_layout          = "us",
        numlock_by_default = true,
    },
    cursor = {
        inactive_timeout     = 3,
        sync_gsettings_theme = false,
    },
    decoration = {
        blur   = { enabled = false },
        shadow = { enabled = false },
    },
    animations = {
        enabled = false,
    },
    debug = {
        enable_stdout_logs = true,
    },
})

-- Exiting without a login sends deskshell-greeter.sh to its retry prompt.
hl.bind("CTRL + ALT + BACKSPACE", hl.dsp.exit())

-- The greeter needs no session bus. A real one would start portals and PipeWire as the
-- greeter user, and they outlive this compositor. Its log goes to the journal with the
-- compositor's. When the greeter exits, after a login or a crash, this compositor exits
-- too.
hl.on("hyprland.start", function()
    local lock = "$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/hyprland.lock"
    hl.exec_cmd("DBUS_SESSION_BUS_ADDRESS=unix:path=/dev/null systemd-cat --identifier=deskshell-greeter \"${DESKSHELL_GREETER:-/usr/bin/deskshell-greeter}\"; hyprctl dispatch 'hl.dsp.exit()' || kill \"$(head -n1 \"" .. lock .. "\")\"")
end)
