# Guest helpers

Small tools for guest checks. They run on the host against a guest, or inside a guest. None of them touch the live session.

| File | Use |
|---|---|
| `qmp-pointer.py SOCKET X Y [X Y ...]` | Moves the guest pointer to each point in turn, 150 ms apart, for hover and drag checks. Assumes the 1280x720 guest. |
| `qmp-click.py SOCKET X Y [WIDTH HEIGHT]` | Moves the pointer and clicks the left button. |
| `launcher-guest-setup.sh` | Run as root in a guest. Installs logging stubs for `uwsm-app`, `xdg-open` and `kitty` (calls land in `/tmp/launches.log`), a 3-bang `bangs.json`, and two desktop entries: one with a missing icon and one with a Thai name. |
| `qalc-stub.sh` | A slow fake `qalc` that logs its arguments, waits 2 s and answers. Put it on the guest's `PATH` as `qalc`. |
| `guest-binds.lua` | Made-up Hyprland binds with descriptions, for the keybindings panel. Load it in the guest with `hyprctl eval`. |

The QMP socket of slot N is `$DESKSHELL_VM_DIR/slots/N/qmp` (by default under `~/.local/share/deskshell-vm`). Run the Python tools with `python3 -I`.
