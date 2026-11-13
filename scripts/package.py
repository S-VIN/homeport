#!/usr/bin/env python3
"""Package the native release build; each CI runner packages its own architecture."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import time
import tempfile
import zipfile
import tomllib
from urllib.request import urlopen

from mihomo import verify as verify_mihomo

ROOT = Path(__file__).resolve().parents[1]
DIST = ROOT / 'dist'
APP_ID = 'io.github.rproxy.Rproxy'
VERSION = tomllib.loads((ROOT / 'Cargo.toml').read_text())['package']['version']


# Native package metadata lives beside the packaging code; generated files go to dist/.
WINDOWS_INSTALLER = r'''Unicode true
!ifndef PORTABLE
!include "MUI2.nsh"
!endif
!include "FileFunc.nsh"
Name "rproxy"
OutFile "${OUTPUT}"
RequestExecutionLevel user
SetCompressor /SOLID lzma
!ifdef PORTABLE
SilentInstall silent
Section
  InitPluginsDir
  SetOutPath "$PLUGINSDIR\rproxy"
  File /r "${STAGE}\*"
  # Data stays beside the portable EXE; extracted binaries are temporary.
  ${GetParameters} $0
  ExecWait '"$PLUGINSDIR\rproxy\rproxy.exe" --data-dir "$EXEDIR\rproxy-data" $0' $1
  SetErrorLevel $1
SectionEnd
!else
InstallDir "$LOCALAPPDATA\Programs\rproxy"
InstallDirRegKey HKCU "Software\rproxy" "InstallDir"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
Section
  SetOutPath "$INSTDIR"
  File /r "${STAGE}\*"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\rproxy" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\rproxy" "DisplayName" "rproxy"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\rproxy" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\rproxy" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
  CreateShortcut "$SMPROGRAMS\rproxy.lnk" "$INSTDIR\rproxy.exe"
SectionEnd
Section "Uninstall"
  Delete "$SMPROGRAMS\rproxy.lnk"
  Delete "$INSTDIR\rproxy.exe"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir /r "$INSTDIR\assets"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\rproxy"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\rproxy"
SectionEnd
!endif
'''

LINUX_DESKTOP = '''[Desktop Entry]
Type=Application
Name=rproxy
Comment=Proxy manager
Exec=rproxy
Icon=io.github.rproxy.Rproxy
Terminal=false
Categories=Network;
StartupWMClass=rproxy
'''

def run(*args, **kwargs):
    subprocess.run([str(a) for a in args], check=True, **kwargs)


def copy(source, target):
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, target)


def bundle(path, system, arch):
    suffix = '.exe' if system == 'win32' else ''
    target = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target'))
    if os.environ.get('CARGO_BUILD_TARGET'):
        target /= os.environ['CARGO_BUILD_TARGET']
    copy(target / f'release/rproxy{suffix}', path / f'rproxy{suffix}')
    core = verify_mihomo(system, arch)
    copy(core, path / 'assets/mihomo' / core.name)
    (path / 'assets/mihomo' / core.name).chmod(0o755)
    copy(ROOT / 'assets/mihomo/LICENSE', path / 'assets/mihomo/LICENSE')


def windows(arch, name):
    stage = DIST / f'windows-{arch}'
    if stage.exists():
        shutil.rmtree(stage)
    bundle(stage, 'win32', arch)
    nsis = shutil.which('makensis') or r'C:\Program Files (x86)\NSIS\makensis.exe'
    installer = DIST / 'windows.nsi'
    installer.write_text(WINDOWS_INSTALLER, encoding='utf-8')
    for kind in ['setup', 'portable']:
        defines = [f'/DSTAGE={stage}', f'/DOUTPUT={DIST / (name + "-" + kind + ".exe")}', f'/DVERSION={VERSION}']
        if kind == 'portable':
            defines.append('/DPORTABLE')
        run(nsis, *defines, installer)
    (stage / 'portable.txt').touch()
    shutil.make_archive(str(DIST / (name + '-portable')), 'zip', stage)


def macos(arch, name):
    image = DIST / f'macos-{arch}'
    if image.exists():
        shutil.rmtree(image)
    app = image / 'rproxy.app'
    bundle(app / 'Contents/MacOS', 'darwin', arch)
    info = dict(CFBundleExecutable='rproxy', CFBundleName='rproxy', CFBundleIdentifier=APP_ID,
                CFBundleVersion=VERSION, CFBundleShortVersionString=VERSION,
                CFBundlePackageType='APPL', NSHighResolutionCapable=True,
                LSMinimumSystemVersion='11.0', LSApplicationCategoryType='public.app-category.utilities')
    (app / 'Contents/Info.plist').write_bytes(plistlib.dumps(info))
    run('codesign', '--force', '--sign', '-', app / 'Contents/MacOS/assets/mihomo/mihomo')
    run('codesign', '--force', '--sign', '-', app / 'Contents/MacOS/rproxy')
    run('codesign', '--force', '--sign', '-', app)
    run('codesign', '--verify', '--deep', '--strict', app)
    (image / 'Applications').symlink_to('/Applications', target_is_directory=True)
    run('hdiutil', 'create', '-volname', 'rproxy', '-srcfolder', image, '-ov', '-format', 'UDZO', DIST / (name + '.dmg'))


def linux(arch, name):
    native_arch = 'x86_64' if arch == 'x64' else 'aarch64'
    appdir = DIST / f'AppDir-{arch}'
    if appdir.exists():
        shutil.rmtree(appdir)
    bundle(appdir / 'usr/bin', 'linux', arch)
    desktop = DIST / f'{APP_ID}.desktop'
    desktop.write_text(LINUX_DESKTOP, encoding='utf-8')
    icon = appdir / f'usr/share/icons/hicolor/256x256/apps/{APP_ID}.png'
    # The repository logo is resized by ImageMagick, available in Linux CI.
    icon.parent.mkdir(parents=True, exist_ok=True)
    run('convert', ROOT / 'assets/logos/blue.png', '-resize', '256x256', icon)
    copy(desktop, appdir / f'usr/share/applications/{APP_ID}.desktop')

    # Flatpak packages the clean binary tree; platform libraries come from its runtime.
    manifest = {
        'app-id': APP_ID, 'runtime': 'org.freedesktop.Platform', 'runtime-version': '25.08',
        'sdk': 'org.freedesktop.Sdk', 'command': 'rproxy',
        'finish-args': ['--share=network', '--share=ipc', '--socket=wayland', '--socket=fallback-x11',
                        '--device=dri', '--talk-name=org.kde.StatusNotifierWatcher'],
        'modules': [{'name': 'rproxy', 'buildsystem': 'simple',
                     'build-commands': ['cp -a usr/. /app/'],
                     'sources': [{'type': 'dir', 'path': str(appdir)}]}],
    }
    manifest_path = DIST / 'flatpak.json'
    manifest_path.write_text(json.dumps(manifest, indent=2))
    run('flatpak-builder', '--user', '--install-deps-from=flathub', '--force-clean',
        f'--repo={DIST / "flatpak-repo"}', DIST / 'flatpak-build', manifest_path)
    run('flatpak', 'build-bundle', '--runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo',
        DIST / 'flatpak-repo', DIST / (name + '.flatpak'), APP_ID)

    # A pacman package, matching the extra Linux format in x-proxy-router.
    pkg = DIST / 'pacman'
    if pkg.exists():
        shutil.rmtree(pkg)
    bundle(pkg / 'opt/rproxy', 'linux', arch)
    copy(desktop, pkg / f'usr/share/applications/{APP_ID}.desktop')
    copy(icon, pkg / f'usr/share/icons/hicolor/256x256/apps/{APP_ID}.png')
    (pkg / 'usr/bin').mkdir(parents=True)
    (pkg / 'usr/bin/rproxy').symlink_to('/opt/rproxy/rproxy')
    size = sum(p.stat().st_size for p in pkg.rglob('*') if p.is_file())
    (pkg / '.PKGINFO').write_text(f'pkgname = rproxy\npkgver = {VERSION}-1\npkgdesc = Proxy manager\n'
        f'arch = {native_arch}\nbuilddate = {int(time.time())}\nsize = {size}\n'
        'depend = glibc\ndepend = gcc-libs\ndepend = libxkbcommon\ndepend = libx11\n'
        'depend = libxcursor\ndepend = libxi\ndepend = libxrandr\ndepend = wayland\ndepend = libglvnd\n')
    run('tar', '--zstd', '-cf', DIST / (name + '.pkg.tar.zst'), '-C', pkg, '.')

    tool = DIST / f'linuxdeploy-{native_arch}.AppImage'
    # Verify the downloaded executable against the digest supplied by GitHub's release API.
    with urlopen('https://api.github.com/repos/linuxdeploy/linuxdeploy/releases/tags/continuous', timeout=120) as response:
        release = json.load(response)
    asset = next(a for a in release['assets'] if a['name'] == tool.name)
    with urlopen(asset['browser_download_url'], timeout=120) as response:
        content = response.read()
    if asset.get('digest') != 'sha256:' + hashlib.sha256(content).hexdigest():
        raise RuntimeError('linuxdeploy SHA256 verification failed')
    tool.write_bytes(content)
    tool.chmod(0o755)
    env = dict(os.environ, APPIMAGE_EXTRACT_AND_RUN='1', OUTPUT=str(DIST / (name + '.AppImage')),
               ARCH=native_arch, VERSION=VERSION)
    run(tool, '--appdir', appdir, '--desktop-file', desktop, '--icon-file', icon,
        '--exclude-library', 'libwayland-*', '--exclude-library', 'libEGL*',
        '--exclude-library', 'libGL*', '--output', 'appimage', env=env)


def smoke(exe, data, **kwargs):
    run(exe, '--smoke-test', '--data-dir', data, timeout=120, **kwargs)


def check_packages(system, arch):
    label = {'win32': 'windows', 'darwin': 'macos', 'linux': 'linux'}[system]
    def artifact(suffix):
        path = DIST / f'rproxy-{VERSION}-{label}-{arch}{suffix}'
        if not path.is_file():
            raise FileNotFoundError(path)
        return path

    def check_run(*args, **kwargs):
        run(*args, timeout=120, **kwargs)

    with tempfile.TemporaryDirectory() as directory:
        temp = Path(directory)
        if system == 'linux':
            env = dict(os.environ, APPIMAGE_EXTRACT_AND_RUN='1')
            smoke(artifact('.AppImage'), temp / 'appimage', env=env)
            check_run('flatpak', 'install', '--user', '-y', artifact('.flatpak'))
            check_run('flatpak', 'run', APP_ID, '--smoke-test', '--data-dir', '/tmp/rproxy-smoke')
            check_run('tar', '--zstd', '-xf', artifact('.pkg.tar.zst'), '-C', temp)
            smoke(temp / 'opt/rproxy/rproxy', temp / 'pacman')
        elif system == 'darwin':
            mount = temp / 'image'
            check_run('hdiutil', 'attach', '-nobrowse', '-readonly', '-mountpoint', mount, artifact('.dmg'))
            try:
                app = mount / 'rproxy.app'
                check_run('codesign', '--verify', '--deep', '--strict', app)
                smoke(app / 'Contents/MacOS/rproxy', temp / 'macos')
            finally:
                check_run('hdiutil', 'detach', mount)
        else:
            with zipfile.ZipFile(artifact('-portable.zip')) as archive:
                archive.extractall(temp / 'portable')
            smoke(temp / 'portable/rproxy.exe', temp / 'windows')
            portable = temp / 'portable.exe'
            shutil.copy2(artifact('-portable.exe'), portable)
            check_run(portable, '--smoke-test')
            # Install silently into a disposable path, then exercise the installed payload.
            install = temp / 'installed'
            check_run(artifact('-setup.exe'), '/S', f'/D={install}')
            smoke(install / 'rproxy.exe', temp / 'installed-data')
            check_run(install / 'Uninstall.exe', '/S', f'_?={install}')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--os', required=True, choices=['linux', 'win32', 'darwin'])
    parser.add_argument('--arch', required=True, choices=['x64', 'arm64'])
    parser.add_argument('--check', action='store_true', help='Check existing packages instead of building them')
    args = parser.parse_args()
    if args.check:
        check_packages(args.os, args.arch)
        return
    DIST.mkdir(exist_ok=True)
    system = {'win32': 'windows', 'darwin': 'macos', 'linux': 'linux'}[args.os]
    name = f'rproxy-{VERSION}-{system}-{args.arch}'
    {'linux': linux, 'win32': windows, 'darwin': macos}[args.os](args.arch, name)
    outputs = [p for p in DIST.glob(name + '*') if p.is_file() and not p.name.endswith('-SHA256SUMS.txt')]
    (DIST / (name + '-SHA256SUMS.txt')).write_text(''.join(
        f'{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n' for p in outputs))


if __name__ == '__main__':
    main()
