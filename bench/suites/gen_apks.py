"""Freeze an APK benchmark list: the newest version of each app in a directory, hashed.

    python3 gen_apks.py ~/apps > apps.json
    python3 gen_apks.py ~/apps/fdroid > fdroid.json

Versions are read from the file name, in either of the two layouts present:
APKPure's `<Name>_<version>_APKPure.<ext>` and F-Droid's `<package>_<versionCode>.apk`.
"""

import hashlib
import json
import re
import sys
from pathlib import Path


def version_key(v: str) -> tuple:
    return tuple(int(p) if p.isdigit() else p for p in re.split(r"[._]", v))


def parse(name: str) -> tuple[str, str] | None:
    if m := re.match(r"(.+)_([0-9][0-9A-Za-z.]*)_APKPure\.x?apk$", name):
        return m.group(1), m.group(2)
    if m := re.match(r"(.+)_([0-9]+)(?:_[0-9a-f]+)?\.apk$", name):
        return m.group(1), m.group(2)
    return None


def main() -> None:
    root = Path(sys.argv[1]).expanduser()
    newest: dict[str, tuple[str, Path]] = {}
    for f in sorted(root.iterdir()):
        if not f.is_file() or f.suffix not in (".apk", ".xapk"):
            continue
        parsed = parse(f.name)
        if parsed is None:
            sys.exit(f"cannot read a version from {f.name}")
        app, version = parsed
        if app not in newest or version_key(version) > version_key(newest[app][0]):
            newest[app] = (version, f)
    out = []
    for app, (version, f) in sorted(newest.items()):
        slug = re.sub(r"[^A-Za-z0-9]+", "-", app.replace("%2C", "").replace("%26", "")).strip("-")
        out.append({"bench": slug, "app": app, "version": version, "path": str(f),
                    "size": f.stat().st_size,
                    "sha256": hashlib.sha256(f.read_bytes()).hexdigest()})
    json.dump(out, sys.stdout, indent=1)
    print()


main()
