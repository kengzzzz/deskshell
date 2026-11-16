#!/usr/bin/env bash
# The FIDO reader's guest check. Run it on the host from the repo root. It builds fake_fido
# and touch_probe, boots one guest, prints one line per check and the raw logs, and stops
# the guest.
#
# Every time it compares comes from the guest's clock: fake_fido logs when it sent a
# frame, and touch_probe logs when the state changed. The signals go over ssh, whose
# delay does not count.
set -euo pipefail

vm=dev/vm/vm
bin=/work/debug/examples
out=$(mktemp -d)
fails=0

cargo build -p deskshell-auth --example fake_fido --example touch_probe
slot=$($vm up --share "$PWD/target")
trap '$vm down "$slot"' EXIT
g() { $vm ssh "$slot" "$@"; }

pull() {
    for f in p0.out p0.err p1.out p1.err fido.out; do
        g cat "/tmp/$f" > "$out/$f" 2> /dev/null || : > "$out/$f"
    done
}

check() {
    if [[ $2 == 1 ]]; then
        echo "PASS  $1  ($3)"
    else
        echo "FAIL  $1  ($3)"
        fails=$((fails + 1))
    fi
}

# nth FILE REGEX N: the time of the Nth line that matches.
nth() { awk -v p="$2" -v n="$3" '$0 ~ p { if (++c == n) { print $1; exit } }' "$out/$1"; }
# after FILE REGEX TIME: the time of the first matching line at or after TIME.
after() { awk -v p="$2" -v t="$3" '$0 ~ p && $1 >= t { print $1; exit }' "$out/$1"; }
# since A B: B - A in seconds, or "none" when either is missing.
since() {
    if [[ -z $1 || -z $2 ]]; then echo none; return; fi
    awk -v a="$1" -v b="$2" 'BEGIN { printf "%.3f", b - a }'
}
# within VALUE LOW HIGH: 1 when LOW <= VALUE < HIGH.
within() {
    [[ $1 == none ]] && { echo 0; return; }
    awk -v v="$1" -v lo="$2" -v hi="$3" 'BEGIN { print (v >= lo && v < hi) ? 1 : 0 }'
}
signal() { g pkill "-$1" -x fake_fido; }
probe() {
    g "as-tester env RUST_LOG=deskshell_auth=trace setsid -f $bin/touch_probe > /tmp/$1.out 2> /tmp/$1.err < /dev/null"
}

echo "slot $slot, logs in $out"
probe p0
sleep 1
g "setsid -f $bin/fake_fido > /tmp/fido.out 2>&1 < /dev/null"
sleep 2
pull
node=$(g 'grep -l "^HID_ID=0003:00001209:00000001" /sys/class/hidraw/*/device/uevent' | awk -F/ '{ print $5 }')
plugged=$(nth fido.out created 1)
opened=$(after p0.out 'u2f=false' "$plugged")
lag=$(since "$plugged" "$opened")
check "the probe opens a new node within 1 s of the plug" "$(within "$lag" 0 1)" "$node opened ${lag}s after the plug"
attrib=$(grep -c "node=\"\?$node\"\?.*IN_ATTRIB\|IN_ATTRIB.*$node" "$out/p0.err" || true)
denied=$(grep -c "$node.*open denied\|open denied.*$node" "$out/p0.err" || true)
echo "INFO  IN_ATTRIB events for $node: $attrib, denied opens: $denied"
grep "$node" "$out/p0.err" | sed 's/^/      /' || true

acl=$(g getfacl -p "/dev/$node" 2> /dev/null || true)
tags=$(g udevadm info "/dev/$node" 2> /dev/null | grep -E 'TAGS=|ID_SECURITY_TOKEN|ID_FIDO_TOKEN' || true)
check "the test user gets the node through uaccess" "$(grep -q '^user:tester:rw' <<< "$acl" && grep -q uaccess <<< "$tags" && echo 1 || echo 0)" "$(grep '^user:' <<< "$acl" | tr '\n' ' ')"
sed 's/^/      /' <<< "$tags"

probe p1
sleep 1.5

signal USR1
sleep 1
signal USR2
sleep 1
pull
first=$(nth fido.out 'first frame sent' 1)
on=$(after p0.out 'u2f=true' "$first")
final=$(nth fido.out 'final frame sent' 1)
off=$(after p0.out 'u2f=false' "$final")
lag=$(since "$first" "$on")
check "the wait turns on within 200 ms of the first frame" "$(within "$lag" 0 0.2)" "${lag}s"
lag=$(since "$final" "$off")
check "the wait turns off within 400 ms of the final frame" "$(within "$lag" 0 0.4)" "${lag}s"

signal USR1
sleep 0.5
signal HUP
sleep 3
pull
last=$(grep 'no final frame' "$out/fido.out" | head -1 | awk '{ print $NF }')
off=$(after p0.out 'u2f=false' "$last")
lag=$(since "$last" "$off")
check "with no final frame the wait ends about 2 s after the last frame" "$(within "$lag" 1.8 2.5)" "${lag}s"

start=$(nth p1.out 'u2f=false' 1)
same=$(paste -d' ' \
    <(awk -v t="$start" '$1 > t' "$out/p0.out") \
    <(awk -v t="$start" '$1 > t' "$out/p1.out") |
    awk 'NF != 6 || $2 != $5 || $3 != $6 { bad++ } { d = $4 - $1; if (d < 0) d = -d; if (d > max) max = d; n++ }
         END { printf "%d %d %.3f", n, bad, max }')
read -r count bad spread <<< "$same"
check "two probes at once see the same changes" "$([[ $count -ge 4 && $bad -eq 0 ]] && within "$spread" 0 0.05 || echo 0)" "$count changes, $bad differ, spread ${spread}s"

before=$(wc -l < "$out/p0.out")
$vm hid "$slot" plug
sleep 1.5
pull
stock=$(g 'grep -l "^HID_ID=0003:00001050:" /sys/class/hidraw/*/device/uevent' | awk -F/ '{ print $5 }')
grep -q "node=\"\?$stock\"\?.*opened\|opened.*$stock" "$out/p0.err" && stock_open=1 || stock_open=0
check "the stock fake key's node opens too" "$stock_open" "$stock"
check "plugging an idle node changes nothing" "$([[ $(wc -l < "$out/p0.out") -eq $before ]] && echo 1 || echo 0)" "$(tail -1 "$out/p0.out")"

signal USR1
sleep 1
signal USR2
sleep 1
pull
first=$(nth fido.out 'first frame sent' 3)
final=$(nth fido.out 'final frame sent' 2)
phase=$(awk -v a="$first" -v b="$final" '$1 >= a && $1 <= b + 1' "$out/p0.out")
on=$(after p0.out 'u2f=true' "$first")
off=$(after p0.out 'u2f=false' "$final")
check "beside the idle stock key, frames turn the wait on" "$(within "$(since "$first" "$on")" 0 0.2)" "$(since "$first" "$on")s"
check "and off, with no other change" "$([[ $(wc -l <<< "$phase") -eq 2 ]] && within "$(since "$final" "$off")" 0 0.4 || echo 0)" "$(since "$final" "$off")s, $(wc -l <<< "$phase") changes"

signal USR1
sleep 0.5
killed=$(g 'date +%s.%3N; pkill -KILL -x fake_fido' | head -1)
sleep 1
pull
off=$(after p0.out 'u2f=' "$killed")
lag=$(since "$killed" "$off")
check "an unplug during a wait ends it at once" "$([[ $(after p0.out 'u2f=false' "$killed") == "$off" ]] && within "$lag" 0 0.2 || echo 0)" "${lag}s, then $(tail -1 "$out/p0.out" | cut -d' ' -f2)"
grep "$node" "$out/p0.err" | tail -2 | sed 's/^/      /' || true

$vm hid "$slot" unplug
sleep 1
pull
check "no FIDO node left: u2f is unknown" "$(tail -1 "$out/p0.out" | grep -q 'u2f=none' && echo 1 || echo 0)" "$(tail -1 "$out/p0.out")"

for f in fido.out p0.out p1.out p0.err; do
    echo "---- $f"
    cat "$out/$f"
done
echo "$fails failed"
[[ $fails -eq 0 ]]
