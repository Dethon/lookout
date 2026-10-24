#!/usr/bin/env python3
"""Builds Lookout's release packages into dist/.

    scripts/package.py [--skip-build] [--only windows|linux]

Windows: an installer and a portable zip, cross-built with cargo-xwin. The
installer needs NSIS; without `makensis` on the PATH, Debian and Ubuntu
unpack it into target/nsis, no root needed. Set PUBLISHER to name a
publisher in the installer.
Linux: a tarball with an installer script, and a .deb when dpkg-deb is
available.

Uses only the standard library of Python 3.11 or later.
"""

import argparse
import io
import os
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "crates" / "app" / "assets"
DIST = ROOT / "dist"
INSTALLER = ROOT / "installer" / "lookout.nsi"
LOCAL_NSIS = ROOT / "target" / "nsis"
WINDOWS_TARGET = "x86_64-pc-windows-msvc"

# Shared libraries the Linux build links, and those winit opens at run time
# for X11 or Wayland.
DEB_DEPENDS = "libfontconfig1, libfreetype6"
DEB_RECOMMENDS = "libxkbcommon-x11-0, libwayland-client0, libxcursor1, libxi6, libxrandr2"

INSTALL_SH = """#!/bin/sh
# Installs Lookout for the current user, under ~/.local.
set -e
here=$(dirname "$0")
prefix=${PREFIX:-$HOME/.local}
install -Dm755 "$here/lookout" "$prefix/bin/lookout"
install -Dm644 "$here/lookout.desktop" "$prefix/share/applications/lookout.desktop"
for icon in "$here"/icons/*/lookout.png; do
    size=$(basename "$(dirname "$icon")")
    install -Dm644 "$icon" "$prefix/share/icons/hicolor/$size/apps/lookout.png"
done
echo "Installed to $prefix. Make sure $prefix/bin is on your PATH."
"""


def version() -> str:
    with open(ROOT / "Cargo.toml", "rb") as manifest:
        return tomllib.load(manifest)["workspace"]["package"]["version"]


def run(*command: str) -> None:
    print("$", " ".join(command), flush=True)
    subprocess.run(command, cwd=ROOT, check=True)


def git_config(key: str) -> str:
    result = subprocess.run(
        ["git", "config", key], cwd=ROOT, capture_output=True, text=True
    )
    return result.stdout.strip()


def icon_pngs() -> dict[int, bytes]:
    """The PNG images inside assets/icon.ico, by size."""
    ico = (ASSETS / "icon.ico").read_bytes()
    (count,) = struct.unpack_from("<H", ico, 4)
    images = {}
    for index in range(count):
        width, _, _, _, _, _, size, offset = struct.unpack_from(
            "<BBBBHHII", ico, 6 + 16 * index
        )
        images[width or 256] = ico[offset : offset + size]
    return images


def makensis() -> list[str] | None:
    """The command running NSIS's compiler, unpacking NSIS if need be."""
    if shutil.which("makensis"):
        return ["makensis"]
    binary = LOCAL_NSIS / "usr" / "bin" / "makensis"
    if not binary.exists() and shutil.which("apt-get") and shutil.which("dpkg"):
        print("makensis not found; unpacking NSIS into target/nsis", flush=True)
        with tempfile.TemporaryDirectory() as debs:
            subprocess.run(
                ["apt-get", "download", "nsis", "nsis-common"],
                cwd=debs, check=True, capture_output=True,
            )
            for deb in Path(debs).glob("*.deb"):
                subprocess.run(["dpkg", "-x", str(deb), str(LOCAL_NSIS)], check=True)
    if binary.exists():
        return ["env", f"NSISDIR={LOCAL_NSIS / 'usr' / 'share' / 'nsis'}", str(binary)]
    return None


def windows(skip_build: bool, release: str) -> None:
    if not skip_build:
        run(
            "cargo", "xwin", "build", "--release", "--target", WINDOWS_TARGET,
            "-p", "lookout", "--bin", "lookout",
        )
    # The portable folder, which the zip and the installer both hold.
    folder = DIST / "lookout-windows-x64"
    shutil.rmtree(folder, ignore_errors=True)
    folder.mkdir(parents=True)
    shutil.copy(ROOT / "target" / WINDOWS_TARGET / "release" / "lookout.exe", folder)
    shutil.copy(ROOT / "LICENSE", folder / "LICENSE.txt")

    archive = DIST / f"lookout-{release}-windows-x64.zip"
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as zip:
        for file in sorted(folder.iterdir()):
            zip.write(file, f"Lookout/{file.name}")
    print("wrote", archive.relative_to(ROOT))

    compiler = makensis()
    if compiler is None:
        sys.exit("NSIS (makensis) is needed for the installer: apt install nsis")
    setup = DIST / f"Lookout-{release}-setup.exe"
    publisher = os.environ.get("PUBLISHER")
    run(
        *compiler, "-V2", "-INPUTCHARSET", "UTF8",
        f"-DVERSION={release}",
        f"-DDIST={folder}",
        f"-DICON={ASSETS / 'icon.ico'}",
        f"-DOUTFILE={setup}",
        *([f"-DPUBLISHER={publisher}"] if publisher else []),
        str(INSTALLER),
    )
    print("wrote", setup.relative_to(ROOT))


def linux_files(binary: Path) -> dict[str, tuple[bytes, int]]:
    """The installed files, by path under the prefix, with their modes."""
    files = {
        "bin/lookout": (binary.read_bytes(), 0o755),
        "share/applications/lookout.desktop": (
            (ASSETS / "lookout.desktop").read_bytes(),
            0o644,
        ),
    }
    for size, png in icon_pngs().items():
        files[f"share/icons/hicolor/{size}x{size}/apps/lookout.png"] = (png, 0o644)
    return files


def add_file(tar: tarfile.TarFile, name: str, data: bytes, mode: int) -> None:
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mode = mode
    tar.addfile(info, io.BytesIO(data))


def linux(skip_build: bool, release: str) -> None:
    if not skip_build:
        run("cargo", "build", "--release", "-p", "lookout", "--bin", "lookout")
    files = linux_files(ROOT / "target" / "release" / "lookout")

    folder = f"lookout-{release}"
    archive = DIST / f"lookout-{release}-linux-x64.tar.gz"
    with tarfile.open(archive, "w:gz") as tar:
        for path, (data, mode) in files.items():
            name = {
                "bin/lookout": "lookout",
                "share/applications/lookout.desktop": "lookout.desktop",
            }.get(path)
            if name is None:
                # share/icons/hicolor/<size>/apps/lookout.png
                name = "icons/" + path.split("/")[3] + "/lookout.png"
            add_file(tar, f"{folder}/{name}", data, mode)
        add_file(tar, f"{folder}/install.sh", INSTALL_SH.encode(), 0o755)
        add_file(tar, f"{folder}/LICENSE", (ROOT / "LICENSE").read_bytes(), 0o644)
    print("wrote", archive.relative_to(ROOT))

    if shutil.which("dpkg-deb") is None:
        print("dpkg-deb not found: no .deb")
        return
    with tempfile.TemporaryDirectory() as staging:
        staging = Path(staging)
        # The package root becomes / on install.
        staging.chmod(0o755)
        for path, (data, mode) in files.items():
            target = staging / "usr" / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
            target.chmod(mode)
        doc = staging / "usr/share/doc/lookout/copyright"
        doc.parent.mkdir(parents=True)
        doc.write_bytes((ROOT / "LICENSE").read_bytes())
        size_kib = sum(len(data) for data, _ in files.values()) // 1024
        maintainer = f"{git_config('user.name')} <{git_config('user.email')}>"
        control = staging / "DEBIAN" / "control"
        control.parent.mkdir()
        control.write_text(
            f"Package: lookout\n"
            f"Version: {release}\n"
            f"Architecture: amd64\n"
            f"Maintainer: {maintainer}\n"
            f"Installed-Size: {size_kib}\n"
            f"Depends: {DEB_DEPENDS}\n"
            f"Recommends: {DEB_RECOMMENDS}\n"
            f"Section: mail\n"
            f"Priority: optional\n"
            f"Description: Fast, light Outlook mail, calendar and contacts client\n"
            f" A client for Outlook.com and Microsoft 365 with a native interface\n"
            f" that uses a fraction of the new Outlook's memory.\n"
        )
        package = DIST / f"lookout_{release}_amd64.deb"
        run("dpkg-deb", "--root-owner-group", "--build", str(staging), str(package))
        print("wrote", package.relative_to(ROOT))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--skip-build", action="store_true", help="package what is built")
    parser.add_argument("--only", choices=["windows", "linux"])
    options = parser.parse_args()

    release = version()
    DIST.mkdir(exist_ok=True)
    if options.only in (None, "windows"):
        if shutil.which("cargo-xwin") is None and not options.skip_build:
            sys.exit("cargo-xwin is needed for the Windows build: cargo install cargo-xwin")
        windows(options.skip_build, release)
    if options.only in (None, "linux"):
        linux(options.skip_build, release)


if __name__ == "__main__":
    os.chdir(ROOT)
    main()
