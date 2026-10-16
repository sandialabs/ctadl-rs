#!/bin/sh
# Install (or reinstall) the two launchd agents and start them.
#
#   com.ctadl-bench.daemon  the scheduler; standard priority, so it can start p-tier work
#                            (everything it runs is placed on a tier explicitly)
#   com.ctadl-bench.web     the results page; ProcessType=Background keeps it on the
#                            efficiency cores
#
#   install.sh [--host ADDR] [--port N]     default 127.0.0.1:8787
#   install.sh --uninstall
set -eu
here=$(cd "$(dirname "$0")" && pwd)
bench=$(cd "$here/.." && pwd)
logs=/Volumes/Shampoo/ctadl-bench/logs
agents="$HOME/Library/LaunchAgents"
# The launcher is what holds Full Disk Access (see launch.c). Rebuilding it changes its code
# hash and voids the grant, so it is built only when missing.
launch="$HOME/.local/bin/ctadl-bench-launch"
host=127.0.0.1
port=8787
uninstall=
while [ $# -gt 0 ]; do
  case "$1" in
    --host) host=$2; shift 2 ;;
    --port) port=$2; shift 2 ;;
    --uninstall) uninstall=1; shift ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
done

for name in daemon web; do
  launchctl bootout "gui/$(id -u)/com.ctadl-bench.$name" 2>/dev/null || true
  rm -f "$agents/com.ctadl-bench.$name.plist"
done
[ -n "$uninstall" ] && { echo "uninstalled"; exit 0; }

mkdir -p "$logs" "$agents" "$(dirname "$launch")"
if [ ! -x "$launch" ]; then
  /usr/bin/cc -O2 -o "$launch" "$here/launch.c"
  echo "built $launch -- add it under System Settings > Privacy & Security > Full Disk Access"
fi

cat > "$agents/com.ctadl-bench.daemon.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.ctadl-bench.daemon</string>
  <key>ProgramArguments</key><array>
    <string>$launch</string><string>$logs/daemon.log</string>
    <string>$bench/run.sh</string><string>daemon</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>60</integer>
  <key>ProcessType</key><string>Standard</string>
</dict></plist>
EOF

cat > "$agents/com.ctadl-bench.web.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.ctadl-bench.web</string>
  <key>ProgramArguments</key><array>
    <string>$launch</string><string>$logs/web.log</string>
    <string>/usr/sbin/taskpolicy</string><string>-b</string>
    <string>$bench/run.sh</string><string>serve</string>
    <string>--host</string><string>$host</string><string>--port</string><string>$port</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>60</integer>
  <key>ProcessType</key><string>Background</string>
  <key>LowPriorityIO</key><true/>
  <key>Nice</key><integer>10</integer>
</dict></plist>
EOF

for name in daemon web; do
  launchctl bootstrap "gui/$(id -u)" "$agents/com.ctadl-bench.$name.plist"
done
echo "installed; results at http://$host:$port/  logs in $logs"
