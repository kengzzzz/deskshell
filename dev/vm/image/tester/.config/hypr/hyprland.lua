hl.monitor({
    output   = "",
    mode     = "1280x720@60",
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
        allow_session_lock_restore      = true,
        background_color                = 0xff203040,
    },
    debug = {
        disable_logs = false,
    },
    animations = {
        enabled = false,
    },
    decoration = {
        blur   = { enabled = false },
        shadow = { enabled = false },
    },
})

hl.on("hyprland.start", function()
    hl.exec_cmd("wayvnc 0.0.0.0 5900")
end)
