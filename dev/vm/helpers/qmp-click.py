"""click.py QMP_SOCKET X Y [WIDTH HEIGHT]: move the guest tablet to X,Y and click left."""

import json
import socket
import sys
import time


def call(f, name, args=None):
    msg = {"execute": name}
    if args is not None:
        msg["arguments"] = args
    f.write(json.dumps(msg) + "\n")
    f.flush()
    while True:
        reply = json.loads(f.readline())
        if "error" in reply:
            raise SystemExit(f"QMP {name}: {reply['error']}")
        if "return" in reply:
            return reply["return"]


def main():
    path, x, y = sys.argv[1], float(sys.argv[2]), float(sys.argv[3])
    width = float(sys.argv[4]) if len(sys.argv) > 4 else 1280
    height = float(sys.argv[5]) if len(sys.argv) > 5 else 720
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.settimeout(10)
    sock.connect(path)
    f = sock.makefile("rw", encoding="utf-8")
    f.readline()
    call(f, "qmp_capabilities")
    ax = int(x / width * 32767)
    ay = int(y / height * 32767)
    call(f, "input-send-event", {"events": [
        {"type": "abs", "data": {"axis": "x", "value": ax}},
        {"type": "abs", "data": {"axis": "y", "value": ay}},
    ]})
    time.sleep(0.15)
    call(f, "input-send-event", {"events": [{"type": "btn", "data": {"down": True, "button": "left"}}]})
    time.sleep(0.05)
    call(f, "input-send-event", {"events": [{"type": "btn", "data": {"down": False, "button": "left"}}]})
    time.sleep(0.1)


if __name__ == "__main__":
    main()
