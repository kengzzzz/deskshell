#!/usr/bin/env bash
# check.sh: runs the self-checks of the VM tooling against fresh guests.
# It needs a built base image and all slots free. It prints one line per check.
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
vm=$here/vm
dir=${DESKSHELL_VM_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/deskshell-vm}
work=''
owner_pid=''
mine=()
failed=0

cleanup() {
    [[ -n $owner_pid ]] && kill "$owner_pid" 2>/dev/null || true
    local k
    for k in ${mine[@]+"${mine[@]}"}; do "$vm" down "$k" > /dev/null 2>&1 || true; done
    [[ -n $work ]] && rm -rf "$work"
}

# Only slots this script started are ever stopped.
forget() {
    local k rest=()
    for k in ${mine[@]+"${mine[@]}"}; do [[ $k == "$1" ]] || rest+=("$k"); done
    mine=(${rest[@]+"${rest[@]}"})
}
down_mine() {
    "$vm" down "$1"
    forget "$1"
}

pass() { printf 'PASS  %s\n' "$*"; }
fail() { printf 'FAIL  %s\n' "$*"; failed=1; }
ms_since() { echo $((($(date +%s%N) - $1) / 1000000)); }

guest_qemus() {
    local p n=0 cmd
    for p in /proc/[0-9]*; do
        [[ $(cat "$p/comm" 2>/dev/null) == qemu-system-x86 ]] || continue
        cmd=$(tr '\0' ' ' < "$p/cmdline" 2>/dev/null) || continue
        [[ $cmd == *"-name deskshell-vm-"* ]] && n=$((n + 1))
    done
    echo "$n"
}

png_has_color() {
    python3 -I - "$1" <<'EOF'
import struct, sys, zlib
data = open(sys.argv[1], "rb").read()
pos, idat = 8, b""
while pos < len(data):
    n, typ = struct.unpack(">I4s", data[pos:pos + 8])
    body = data[pos + 8:pos + 8 + n]
    if typ == b"IHDR":
        w, h, depth, ctype = struct.unpack(">IIBB", body[:10])
    elif typ == b"IDAT":
        idat += body
    pos += 12 + n
bpp = {2: 3, 6: 4}[ctype]
raw, stride, prev, i, peak = zlib.decompress(idat), w * bpp, bytearray(w * bpp), 0, 0
for _ in range(h):
    f, line = raw[i], bytearray(raw[i + 1:i + 1 + stride])
    i += 1 + stride
    for x in range(stride):
        a = line[x - bpp] if x >= bpp else 0
        b = prev[x]
        c = prev[x - bpp] if x >= bpp else 0
        if f == 1:
            line[x] = (line[x] + a) & 255
        elif f == 2:
            line[x] = (line[x] + b) & 255
        elif f == 3:
            line[x] = (line[x] + (a + b) // 2) & 255
        elif f == 4:
            p = a + b - c
            pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
            line[x] = (line[x] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
    peak = max(peak, max(v for k, v in enumerate(line) if k % bpp < 3))
    prev = line
print(f"{w}x{h}, brightest channel {peak}")
sys.exit(0 if peak > 0 else 1)
EOF
}

[[ -f $dir/base.qcow2 ]] || { echo "no base image, run: vm build-base"; exit 1; }
[[ -z $("$vm" list | tail -n +2) ]] || { echo "slots are in use, stop them first"; exit 1; }
work=$(mktemp -d)
trap cleanup EXIT
[[ -f $dir/base.info ]] && printf 'INFO  base image: %s\n' "$(head -1 "$dir/base.info")"

t0=$(date +%s%N)
slot=$("$vm" up 2> "$work/up.err") || { cat "$work/up.err"; exit 1; }
mine+=("$slot")
pass "up reached Hyprland in $(ms_since "$t0") ms ($(tail -1 "$work/up.err"))"

"$vm" ssh "$slot" 'lsmod | grep -q "^vkms " && as-tester hyprctl -j monitors | grep -q "\"name\": \"Virtual-1\""' \
    && pass "Hyprland drives a vkms output (Virtual-1)" || fail "no vkms monitor in hyprctl"

if "$vm" screenshot "$slot" "$work/shot.png" && info=$(png_has_color "$work/shot.png"); then
    pass "screenshot is a PNG that is not all black ($info)"
else
    fail "screenshot is missing or all black"
fi

"$vm" ssh "$slot" 'cat > /tmp/read-line.sh <<"EOF"
#!/bin/sh
touch /tmp/ready
read -r line
printf %s "$line" > /tmp/typed
EOF
chmod 755 /tmp/read-line.sh
rm -f /tmp/ready /tmp/typed
as-tester hyprctl dispatch "hl.dsp.exec_cmd(\"foot /tmp/read-line.sh\")" > /dev/null
for _ in $(seq 100); do [ -e /tmp/ready ] && as-tester hyprctl -j activewindow | grep -q "\"class\": \"foot\"" && exit 0; sleep 0.1; done
exit 1' || fail "foot did not start"
text='Hello, VM! ~`@#$%^&*()_+{}|:"<>? 123'
"$vm" type "$slot" "$text"
"$vm" key "$slot" ret
typed=$("$vm" ssh "$slot" 'for _ in $(seq 50); do [ -e /tmp/typed ] && break; sleep 0.1; done; cat /tmp/typed')
[[ $typed == "$text" ]] && pass "type reached a foot window: $typed" || fail "foot got '$typed'"

hid_count() { "$vm" ssh "$slot" 'grep -ls "^HID_ID=0003:00001050:" /sys/class/hidraw/*/device/uevent | wc -l'; }
before=$(hid_count)
"$vm" hid "$slot" plug
plugged=$(hid_count)
"$vm" hid "$slot" unplug
unplugged=$(hid_count)
[[ $before == 0 && $plugged == 1 && $unplugged == 0 ]] \
    && pass "hid plug and unplug add and remove the 1050 hidraw entry (0 -> 1 -> 0)" \
    || fail "hid entries: before $before, plugged $plugged, unplugged $unplugged"

pam=$("$vm" ssh "$slot" 'set -u
try() { as-tester pam-try "$@"; }
tally() { faillock --user tester | grep -c deskshell-test || true; }
plug() { systemctl start fake-yubikey; for _ in $(seq 50); do grep -qs "^HID_ID=0003:00001050:" /sys/class/hidraw/*/device/uevent && break; sleep 0.1; done; }
faillock --user tester --reset
t=$(date +%s%N)
out=$(try 123456 || true)
echo "absent: $(grep -c "^prompt:" <<< "$out") prompts, $(grep "^result:" <<< "$out"), tally $(tally), $((($(date +%s%N) - t) / 1000000)) ms"
faillock --user tester --reset
plug
out=$(try 123456 || true)
echo "right: $(grep -c "^prompt: Please enter the PIN" <<< "$out") pin prompts, $(grep -c "^info: .*touch" <<< "$out") touch cues, $(grep "^result:" <<< "$out"), tally $(tally)"
for i in 1 2 3; do out=$(try 000000 || true); echo "wrong $i: $(grep "^result:" <<< "$out"), tally $(tally)"; done
out=$(try 123456 || true)
echo "right after 3 wrong: $(grep "^result:" <<< "$out"), locked message $(grep -c "account is locked" <<< "$out"), tally $(tally)"
faillock --user tester --reset
sed -i "s/^touch_delay_ms=.*/touch_delay_ms=1000/" /etc/fakeu2f.conf
t=$(date +%s%N)
out=$(try 123456 || true)
echo "touch delay: $(grep "^result:" <<< "$out"), $((($(date +%s%N) - t) / 1000000)) ms"
(sleep 0.5; systemctl stop fake-yubikey) &
out=$(try 123456 || true)
wait
echo "unplug during touch: $(grep "^result:" <<< "$out")"
sed -i "s/^touch_delay_ms=.*/touch_delay_ms=0/" /etc/fakeu2f.conf
faillock --user tester --reset
systemctl stop fake-yubikey')
printf '%s\n' "$pam" | sed 's/^/INFO  pam: /'
grep -Eq '^absent: 0 prompts, result: Authentication failure \(7\), tally 1, [0-9]+ ms$' <<< "$pam" \
    && pass "PAM: an absent key fails without a PIN prompt (and counts toward faillock)" \
    || fail "PAM absent key case"
grep -q '^right: 1 pin prompts, 1 touch cues, result: Success (0), tally 0$' <<< "$pam" \
    && pass "PAM: the right PIN succeeds after the PIN prompt and the touch cue" \
    || fail "PAM right PIN case"
grep -q '^right after 3 wrong: result: Authentication failure (7), locked message 1, tally 3$' <<< "$pam" \
    && pass "PAM: three wrong PINs lock the account, the right PIN then fails" \
    || fail "PAM lockout case"
grep -Eq '^touch delay: result: Success \(0\), (1[0-9]{3}) ms$' <<< "$pam" \
    && grep -q '^unplug during touch: result: Authentication failure (7)$' <<< "$pam" \
    && pass "PAM: touch_delay_ms delays the success, and an unplug during the wait fails" \
    || fail "PAM touch delay case"

vkms=$("$vm" ssh "$slot" 'set -e
c=/sys/kernel/config/vkms/check
mkdir $c
mkdir $c/planes/plane0 $c/crtcs/crtc0 $c/encoders/encoder0 $c/connectors/connector0
echo 1 > $c/planes/plane0/type
ln -s $c/crtcs/crtc0 $c/planes/plane0/possible_crtcs/
ln -s $c/crtcs/crtc0 $c/encoders/encoder0/possible_crtcs/
ln -s $c/encoders/encoder0 $c/connectors/connector0/possible_encoders/
echo 1 > $c/enabled
sleep 0.5
card=$(ls -d /sys/devices/faux/check/drm/card*/card*-* | head -1)
a=$(cat $card/status)
echo 2 > $c/connectors/connector0/status
sleep 0.5
b=$(cat $card/status)
echo 1 > $c/connectors/connector0/status
sleep 0.5
echo "$a $b $(cat $card/status)"')
[[ $vkms == "connected disconnected connected" ]] \
    && pass "vkms configfs connector hotplug works ($vkms)" || fail "vkms configfs: $vkms"
down_mine "$slot"

t0=$(date +%s%N)
for i in 1 2 3 4 5 6; do "$vm" up > "$work/par.$i" 2> "$work/par.$i.err" & done
wait
for i in 1 2 3 4 5 6; do
    s=$(cat "$work/par.$i")
    [[ -n $s ]] && mine+=("$s")
done
up=$(cat "$work"/par.? | sort -n | tr '\n' ' ')
if [[ $up == "1 2 3 4 5 6 " ]]; then
    pass "6 guests up at once in $(ms_since "$t0") ms"
else
    fail "6 guests: got slots '$up'"
    cat "$work"/par.?.err
fi
times=$(sed -n 's/^vm: slot \([0-9]\) ready in \([0-9.]*\) s.*/slot \1 \2 s/p' "$work"/par.?.err | sort | paste -sd, -)
printf 'INFO  boot times with 6 starting together: %s\n' "$times"
for s in $up; do down_mine "$s"; done
qemus=$(guest_qemus)
overlays=$(find "$dir/slots" -name overlay.qcow2 | wc -l)
scopes=$(systemctl --user list-units --no-legend 'deskshell-vm-*' | wc -l)
[[ $qemus == 0 && $overlays == 0 && $scopes == 0 ]] \
    && pass "all down: no guest processes, overlays or scopes left" \
    || fail "after down: $qemus qemu, $overlays overlays, $scopes scopes"

sleep 600 &
owner_pid=$!
slot=$(DESKSHELL_VM_OWNER=$owner_pid "$vm" up 2> /dev/null)
mine+=("$slot")
"$vm" gc
"$vm" list | grep -q "^$slot " && kept=yes || kept=no
kill "$owner_pid"
wait "$owner_pid" 2>/dev/null || true
owner_pid=
"$vm" gc 2> "$work/gc.err"
"$vm" list | grep -q "^$slot " && gone=no || gone=yes
[[ $gone == yes ]] && forget "$slot"
[[ $kept == yes && $gone == yes && ! -e $dir/slots/$slot/overlay.qcow2 && $(guest_qemus) == 0 ]] \
    && pass "gc keeps a guest with a live owner and stops it once the owner is killed ($(cat "$work/gc.err"))" \
    || fail "gc: kept $kept, gone $gone"

if s=$(DESKSHELL_VM_BOOT_TIMEOUT=1 "$vm" up 2> "$work/timeout.err"); then
    mine+=("$s")
    fail "a 1 s boot timeout did not fail"
else
    left=$(($(guest_qemus) + $(find "$dir/slots" -name overlay.qcow2 | wc -l) + $(systemctl --user list-units --no-legend 'deskshell-vm-*' | wc -l)))
    [[ $left == 0 && -z $("$vm" list | tail -n +2) ]] \
        && pass "a failed up cleans up after itself ($(tail -1 "$work/timeout.err"))" \
        || fail "a failed up left $left things behind"
fi

exit "$failed"
