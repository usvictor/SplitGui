#!/usr/bin/env python3
"""Rust gnullvm -> Zig/MinGW linker adapter (UCRT, Windows 10+).

Used only for cross-builds; native GitHub builds continue to use MSVC.
Set SPLITGUI_ZIG to the absolute Zig executable. Disable profile LTO for
this cross-link route to avoid incompatible/empty cross-toolchain objects.
"""
import os
import pathlib
import shutil
import sys

original = sys.argv[1:]
search = [pathlib.Path(original[i + 1]) for i, arg in enumerate(original[:-1]) if arg == "-L"]
args = []
for arg in original:
    if arg in {"-nolibc", "--unwindlib=none", "-Wl,-Bstatic", "-Wl,-Bdynamic", "-lmsvcrt", "-Wl,-O1"}:
        continue
    if arg.startswith("-lwindows."):
        archive = "lib" + arg[2:] + ".a"
        found = next((p / archive for p in search if (p / archive).is_file()), None)
        if found is None:
            raise SystemExit("Cannot locate Rust Windows import archive: " + archive)
        arg = str(found)
    args.append(arg)
zig = os.environ.get("SPLITGUI_ZIG") or shutil.which("zig")
if not zig:
    raise SystemExit("Set SPLITGUI_ZIG to the Zig executable")
os.execv(zig, [zig, "cc", "-target", "x86_64-windows-gnu"] + args)
