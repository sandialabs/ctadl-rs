#!/bin/sh
# Run ctbench with the Python pinned by bench/flake.nix:  run.sh daemon | serve | status | report
# The launchd agents in launchd/ call this.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)
export PATH="$HOME/.nix-profile/bin:/nix/var/nix/profiles/default/bin:/usr/bin:/bin:/usr/sbin:/sbin"
python=$(nix build --no-link --print-out-paths "git+file://$repo?dir=bench#python")/bin/python3
cd "$here"
exec "$python" -m ctbench "$@"
