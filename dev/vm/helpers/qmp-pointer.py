"""pointer.py QMP_SOCKET X Y [X Y ...]: absolute pointer moves on a 1280x720 guest."""

import json
import socket
import sys
import time


def call(file, name, args=None):
    msg = {"execute": name}
    if args is not None:
        msg["arguments"] = args
    file.write(json.dumps(msg) + "\n")
    file.flush()
    while True:
        reply = json.loads(file.readline())
        if "error" in reply:
            raise SystemExit(f"QMP {name}: {reply['error']}")
        if "return" in reply:
            return reply["return"]


sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.settimeout(10)
sock.connect(sys.argv[1])
file = sock.makefile("rw", encoding="utf-8")
file.readline()
call(file, "qmp_capabilities")
points = [int(v) for v in sys.argv[2:]]
for x, y in zip(points[::2], points[1::2]):
    call(file, "input-send-event", {"events": [
        {"type": "abs", "data": {"axis": "x", "value": x * 32767 // 1280}},
        {"type": "abs", "data": {"axis": "y", "value": y * 32767 // 720}},
    ]})
    time.sleep(0.15)
