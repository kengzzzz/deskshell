#!/bin/sh
# Runs as root inside a disposable test guest.
set -e
cat > /usr/local/bin/uwsm-app <<'EOF'
#!/bin/sh
echo "uwsm-app $*" >> /tmp/launches.log
[ "$1" = "--" ] && shift
exec "$@"
EOF
cat > /usr/local/bin/xdg-open <<'EOF'
#!/bin/sh
echo "xdg-open $*" >> /tmp/launches.log
EOF
cat > /usr/local/bin/kitty <<'EOF'
#!/bin/sh
echo "kitty $*" >> /tmp/launches.log
EOF
chmod 755 /usr/local/bin/uwsm-app /usr/local/bin/xdg-open /usr/local/bin/kitty
touch /tmp/launches.log
chmod 666 /tmp/launches.log

install -d -o tester -g tester /home/tester/.cache /home/tester/.cache/deskshell
cat > /home/tester/.cache/deskshell/bangs.json <<'EOF'
[{"t": "yt", "s": "YouTube", "d": "www.youtube.com", "c": "Multimedia", "sc": "Video", "r": 90},
 {"t": "w", "s": "Wikipedia", "d": "en.wikipedia.org", "c": "Research", "sc": "Reference", "r": 50},
 {"t": "gh", "s": "GitHub", "d": "github.com", "c": "Tech", "r": 40}]
EOF
chown tester:tester /home/tester/.cache/deskshell/bangs.json

install -d -o tester -g tester /home/tester/.local /home/tester/.local/share /home/tester/.local/share/applications
cat > /home/tester/.local/share/applications/no-icon-sample.desktop <<'EOF'
[Desktop Entry]
Type=Application
Name=No Icon Sample
Comment=An entry whose icon is missing
Exec=foot
Icon=sample-icon-that-does-not-exist
EOF
cat > /home/tester/.local/share/applications/thai-sample.desktop <<'EOF'
[Desktop Entry]
Type=Application
Name=เทอร์มินัล ตัวอย่าง
Comment=รายการทดสอบภาษาไทย
Exec=foot
Icon=foot
EOF
chown tester:tester /home/tester/.local/share/applications/*.desktop
echo setup done
