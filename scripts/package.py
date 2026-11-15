#!/usr/bin/env python3
"""Package the release build for this OS and architecture into dist/.

Packages are named rproxy-<version>-<os>-<arch>…; on CI their names also go to
GITHUB_OUTPUT as package-1, package-2 for upload. Templates live in scripts/packaging/.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import time
import tomllib
from urllib.request import urlopen

from mihomo import verify as verify_mihomo

ROOT = Path(__file__).resolve().parents[1]
DIST = ROOT / 'dist'
TEMPLATES = ROOT / 'scripts/packaging'
APP_ID = 'io.github.rproxy.Rproxy'
VERSION = tomllib.loads((ROOT / 'Cargo.toml').read_text())['package']['version']
# linuxdeploy is pinned to a release; its digests come from that release's assets.
LINUXDEPLOY = 'https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-{}.AppImage'
LINUXDEPLOY_SHA256 = {
    'x86_64': 'c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d',
    'aarch64': '620095110d693282b8ebeb244a95b5e911cf8f65f76c88b4b47d16ae6346fcff',
}


def run(*args, **kwargs):
    subprocess.run([str(a) for a in args], check=True, **kwargs)


def copy(source, target):
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, target)


def fresh(path):
    """An empty directory, cleared of a previous run's output."""
    if path.exists():
        shutil.rmtree(path)
    return path


def bundle(path, system, arch, resources=None):
    """rproxy and its mihomo core side by side; the core's license under resources/licenses."""
    exe = 'rproxy.exe' if system == 'win32' else 'rproxy'
    target = ROOT / os.environ.get('CARGO_TARGET_DIR', 'target') / os.environ.get('CARGO_BUILD_TARGET', '')
    copy(target / 'release' / exe, path / exe)
    helper = 'rproxy-core-helper.exe' if system == 'win32' else 'rproxy-core-helper'
    copy(target / 'release' / helper, path / helper)
    core = verify_mihomo(system, arch)
    copy(core, path / core.name)
    (path / core.name).chmod(0o755)
    copy(ROOT / 'assets/mihomo/LICENSE', (resources or path) / 'licenses/mihomo/LICENSE')


def windows(arch, name):
    """A per-user installer and a portable folder (CI zips the folder)."""
    portable = fresh(DIST / f'{name}-portable')
    bundle(portable, 'win32', arch)
    setup = DIST / f'{name}-setup.exe'
    nsis = shutil.which('makensis') or r'C:\Program Files (x86)\NSIS\makensis.exe'
    run(nsis, f'/DSTAGE={portable}', f'/DOUTPUT={setup}', f'/DVERSION={VERSION}', TEMPLATES / 'windows.nsi')
    # Added after the installer took its files: only the folder keeps its profile beside the binary.
    (portable / 'portable.txt').touch()
    return [setup, portable]


def macos(arch, name):
    """An ad-hoc signed app in a DMG with an Applications link."""
    image = fresh(DIST / f'macos-{arch}')
    app = image / 'rproxy.app'
    bundle(app / 'Contents/MacOS', 'darwin', arch, resources=app / 'Contents/Resources')
    info = dict(CFBundleExecutable='rproxy', CFBundleName='rproxy', CFBundleIdentifier=APP_ID,
                CFBundleVersion=VERSION, CFBundleShortVersionString=VERSION,
                CFBundlePackageType='APPL', NSHighResolutionCapable=True,
                LSMinimumSystemVersion='11.0', LSApplicationCategoryType='public.app-category.utilities')
    (app / 'Contents/Info.plist').write_bytes(plistlib.dumps(info))
    # Sign nested code first. Signing the app also signs its main executable.
    run('codesign', '--force', '--sign', '-', app / 'Contents/MacOS/mihomo')
    run('codesign', '--force', '--sign', '-', app / 'Contents/MacOS/rproxy-core-helper')
    run('codesign', '--force', '--sign', '-', app)
    run('codesign', '--verify', '--deep', '--strict', app)
    (image / 'Applications').symlink_to('/Applications', target_is_directory=True)
    dmg = DIST / f'{name}.dmg'
    # hdiutil sometimes fails with "Resource busy" on CI runners; a retry succeeds.
    for attempt in range(3):
        try:
            run('hdiutil', 'create', '-volname', 'rproxy', '-srcfolder', image, '-ov', '-format', 'UDZO', dmg)
            break
        except subprocess.CalledProcessError:
            if attempt == 2:
                raise
            time.sleep(10)
    return [dmg]


def linux(arch, name):
    """An AppImage and a Flatpak bundle, both from one AppDir."""
    appdir = fresh(DIST / f'AppDir-{arch}')
    bundle(appdir / 'usr/bin', 'linux', arch, resources=appdir / 'usr/share')
    share = appdir / 'usr/share'
    icon = share / f'icons/hicolor/512x512/apps/{APP_ID}.png'
    copy(TEMPLATES / f'{APP_ID}.desktop', share / f'applications/{APP_ID}.desktop')
    copy(ROOT / 'assets/logos/rproxy-blue-512.png', icon)
    copy(ROOT / 'assets/logos/rproxy-blue.svg', share / f'icons/hicolor/scalable/apps/{APP_ID}.svg')

    # Flatpak first: linuxdeploy later adds libraries to the AppDir that the runtime already has.
    flatpak = DIST / f'{name}.flatpak'
    manifest = {
        'app-id': APP_ID, 'runtime': 'org.freedesktop.Platform', 'runtime-version': '26.08',
        'sdk': 'org.freedesktop.Sdk', 'command': 'rproxy',
        # Fewer malloc arenas: the default one per thread wastes memory in a tray app.
        'finish-args': ['--env=MALLOC_ARENA_MAX=2', '--filesystem=xdg-run/rproxy:create', '--share=network', '--share=ipc',
                        '--socket=wayland', '--socket=fallback-x11', '--device=dri', '--talk-name=org.kde.StatusNotifierWatcher',
                        '--talk-name=org.freedesktop.Flatpak'],
        # Cargo already stripped debuginfo; this also avoids a host eu-strip (elfutils).
        'modules': [{'name': 'rproxy', 'buildsystem': 'simple', 'build-options': {'no-debuginfo': True},
                     'build-commands': ['cp -a usr/. /app/'],
                     'sources': [{'type': 'dir', 'path': str(appdir)}]}],
    }
    (DIST / 'flatpak.json').write_text(json.dumps(manifest, indent=2))
    run('flatpak-builder', '--user', '--install-deps-from=flathub', '--force-clean',
        f'--repo={DIST / "flatpak-repo"}', DIST / 'flatpak-build', DIST / 'flatpak.json')
    run('flatpak', 'build-bundle', '--runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo',
        DIST / 'flatpak-repo', flatpak, APP_ID)

    machine = 'x86_64' if arch == 'x64' else 'aarch64'
    with urlopen(LINUXDEPLOY.format(machine), timeout=120) as response:
        content = response.read()
    if hashlib.sha256(content).hexdigest() != LINUXDEPLOY_SHA256[machine]:
        raise RuntimeError('linuxdeploy SHA256 verification failed')
    linuxdeploy = DIST / f'linuxdeploy-{machine}.AppImage'
    linuxdeploy.write_bytes(content)
    linuxdeploy.chmod(0o755)
    appimage = DIST / f'{name}.AppImage'
    env = dict(os.environ, APPIMAGE_EXTRACT_AND_RUN='1', OUTPUT=str(appimage), ARCH=machine, VERSION=VERSION)
    # Graphics libraries must come from the user's desktop, not from the build machine.
    run(linuxdeploy, '--appdir', appdir, '--desktop-file', TEMPLATES / f'{APP_ID}.desktop', '--icon-file', icon,
        '--exclude-library', 'libwayland-*', '--exclude-library', 'libEGL*',
        '--exclude-library', 'libGL*', '--output', 'appimage', env=env)
    return [appimage, flatpak]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--os', required=True, choices=['linux', 'win32', 'darwin'])
    parser.add_argument('--arch', required=True, choices=['x64', 'arm64'])
    args = parser.parse_args()
    DIST.mkdir(exist_ok=True)
    system = {'win32': 'windows', 'darwin': 'macos', 'linux': 'linux'}[args.os]
    packages = {'win32': windows, 'darwin': macos, 'linux': linux}[args.os](args.arch, f'rproxy-{VERSION}-{system}-{args.arch}')
    for package in packages:
        print(f'Packaged {package.relative_to(ROOT)}')
    if 'GITHUB_OUTPUT' in os.environ:
        with open(os.environ['GITHUB_OUTPUT'], 'a', encoding='utf-8') as output:
            output.writelines(f'package-{i}={p.name}\n' for i, p in enumerate(packages, 1))


if __name__ == '__main__':
    main()
