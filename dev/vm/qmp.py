"""qmp.py SOCKET key KEY... | type TEXT: keyboard input for a guest through QMP."""

import json
import socket
import sys
import time

HOLD_MS = 15

ALIASES = {
    "super": "meta_l",
    "win": "meta_l",
    "enter": "ret",
    "return": "ret",
    "space": "spc",
    "escape": "esc",
    "del": "delete",
    "bksp": "backspace",
    "pageup": "pgup",
    "pagedown": "pgdn",
}

PLAIN = {
    " ": "spc", "\n": "ret", "\t": "tab",
    "-": "minus", "=": "equal", "[": "bracket_left", "]": "bracket_right",
    "\\": "backslash", ";": "semicolon", "'": "apostrophe", "`": "grave_accent",
    ",": "comma", ".": "dot", "/": "slash",
}

SHIFTED = {
    "!": "1", "@": "2", "#": "3", "$": "4", "%": "5", "^": "6", "&": "7", "*": "8",
    "(": "9", ")": "0", "_": "minus", "+": "equal", "{": "bracket_left",
    "}": "bracket_right", "|": "backslash", ":": "semicolon", '"': "apostrophe",
    "~": "grave_accent", "<": "comma", ">": "dot", "?": "slash",
}


class Qmp:
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(10)
        self.sock.connect(path)
        self.file = self.sock.makefile("rw", encoding="utf-8")
        self.file.readline()
        self.call("qmp_capabilities")

    def call(self, name, args=None):
        msg = {"execute": name}
        if args is not None:
            msg["arguments"] = args
        self.file.write(json.dumps(msg) + "\n")
        self.file.flush()
        while True:
            line = self.file.readline()
            if not line:
                raise SystemExit("QMP connection closed")
            reply = json.loads(line)
            if "error" in reply:
                raise SystemExit(f"QMP {name}: {reply['error'].get('desc', reply['error'])}")
            if "return" in reply:
                return reply["return"]


def combo(spec):
    keys = []
    for part in spec.split("-"):
        if not part:
            raise SystemExit(f"bad key: {spec!r}")
        keys.append(ALIASES.get(part.lower(), part.lower()))
    return keys


def char_keys(ch):
    if "a" <= ch <= "z" or "0" <= ch <= "9":
        return [ch]
    if "A" <= ch <= "Z":
        return ["shift", ch.lower()]
    if ch in PLAIN:
        return [PLAIN[ch]]
    if ch in SHIFTED:
        return ["shift", SHIFTED[ch]]
    raise SystemExit(f"cannot type {ch!r} on a US layout")


def send(qmp, groups):
    for keys in groups:
        qmp.call("send-key", {
            "keys": [{"type": "qcode", "data": k} for k in keys],
            "hold-time": HOLD_MS,
        })
    # send-key returns before QEMU has replayed the queued presses and releases.
    time.sleep(len(groups) * HOLD_MS / 1000 + 0.1)


def main():
    if len(sys.argv) < 4 or sys.argv[2] not in ("key", "type"):
        raise SystemExit(__doc__)
    path, mode, args = sys.argv[1], sys.argv[2], sys.argv[3:]
    if mode == "key":
        groups = [combo(a) for a in args]
    else:
        groups = [char_keys(ch) for ch in " ".join(args)]
    send(Qmp(path), groups)


if __name__ == "__main__":
    main()
