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
# linuxdeploy is pinned to a release; its digests come from that release's assets.
LINUXDEPLOY = 'https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-{}.AppImage'
LINUXDEPLOY_SHA256 = {
    'x86_64': 'c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d',
    'aarch64': '620095110d693282b8ebeb244a95b5e911cf8f65f76c88b4b47d16ae6346fcff',
}


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
# Windows locks running executables: ask the user to quit rproxy first.
!macro CHECK_RUNNING UN
Function ${UN}CheckRunning
  retry:
  StrCpy $1 "rproxy.exe"
  Call ${UN}Locked
  StrCmp $0 "locked" running
  StrCpy $1 "mihomo.exe"
  Call ${UN}Locked
  StrCmp $0 "locked" running done
  running:
  MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "rproxy is running. Quit it from its tray icon (Quit rproxy), then click Retry." /SD IDCANCEL IDRETRY retry
  Abort
  done:
FunctionEnd
Function ${UN}Locked
  StrCpy $0 ""
  IfFileExists "$INSTDIR\$1" 0 free
  ClearErrors
  FileOpen $2 "$INSTDIR\$1" a
  IfErrors 0 +3
  StrCpy $0 "locked"
  Goto free
  FileClose $2
  free:
FunctionEnd
!macroend
!insertmacro CHECK_RUNNING ""
!insertmacro CHECK_RUNNING "un."
InstallDir "$LOCALAPPDATA\Programs\rproxy"
InstallDirRegKey HKCU "Software\rproxy" "InstallDir"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
Section
  SetOutPath "$INSTDIR"
  Call CheckRunning
  File /r "${STAGE}\*"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\rproxy" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\rproxy" "DisplayName" "rproxy"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\rproxy" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\rproxy" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
  CreateShortcut "$SMPROGRAMS\rproxy.lnk" "$INSTDIR\rproxy.exe"
SectionEnd
Section "Uninstall"
  Call un.CheckRunning
  Delete "$SMPROGRAMS\rproxy.lnk"
  Delete "$INSTDIR\rproxy.exe"
  Delete "$INSTDIR\mihomo.exe"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir /r "$INSTDIR\licenses"
  # Remove the payload of older installations too.
  RMDir /r "$INSTDIR\assets"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\rproxy"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\rproxy"
SectionEnd
!endif
'''

# pacman runs this scriptlet as root: let the packaged core create the TUN interface.
PACMAN_INSTALL = '''post_install() {
  setcap cap_net_admin,cap_net_bind_service+ep /opt/rproxy/mihomo
}

post_upgrade() {
  post_install
}
'''

LINUX_DESKTOP = '''[Desktop Entry]
Type=Application
Name=rproxy
Comment=Proxy manager
Exec=rproxy
Icon=io.github.rproxy.Rproxy
Terminal=false
Categories=Network;
StartupWMClass=io.github.rproxy.Rproxy
'''

def run(*args, **kwargs):
    subprocess.run([str(a) for a in args], check=True, **kwargs)


def copy(source, target):
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, target)


def bundle(path, system, arch, *, resources=None):
    suffix = '.exe' if system == 'win32' else ''
    target = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target'))
    if not target.is_absolute():
        target = ROOT / target
    if os.environ.get('CARGO_BUILD_TARGET'):
        target /= os.environ['CARGO_BUILD_TARGET']
    copy(target / f'release/rproxy{suffix}', path / f'rproxy{suffix}')
    core = verify_mihomo(system, arch)
    # Every package puts the core beside rproxy; only the package's resource root varies.
    copy(core, path / core.name)
    (path / core.name).chmod(0o755)
    copy(ROOT / 'assets/mihomo/LICENSE', (resources or path) / 'licenses/mihomo/LICENSE')


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
    bundle(app / 'Contents/MacOS', 'darwin', arch, resources=app / 'Contents/Resources')
    info = dict(CFBundleExecutable='rproxy', CFBundleName='rproxy', CFBundleIdentifier=APP_ID,
                CFBundleVersion=VERSION, CFBundleShortVersionString=VERSION,
                CFBundlePackageType='APPL', NSHighResolutionCapable=True,
                LSMinimumSystemVersion='11.0', LSApplicationCategoryType='public.app-category.utilities')
    (app / 'Contents/Info.plist').write_bytes(plistlib.dumps(info))
    # Sign nested code first. Signing the app also signs its main executable.
    run('codesign', '--force', '--sign', '-', app / 'Contents/MacOS/mihomo')
    run('codesign', '--force', '--sign', '-', app)
    run('codesign', '--verify', '--deep', '--strict', app)
    (image / 'Applications').symlink_to('/Applications', target_is_directory=True)
    # hdiutil sometimes fails with "Resource busy" on CI runners; a retry succeeds.
    for attempt in range(3):
        try:
            run('hdiutil', 'create', '-volname', 'rproxy', '-srcfolder', image, '-ov', '-format', 'UDZO', DIST / (name + '.dmg'))
            break
        except subprocess.CalledProcessError:
            if attempt == 2:
                raise
            time.sleep(10)


def linux(arch, name):
    native_arch = 'x86_64' if arch == 'x64' else 'aarch64'
    appdir = DIST / f'AppDir-{arch}'
    if appdir.exists():
        shutil.rmtree(appdir)
    bundle(appdir / 'usr/bin', 'linux', arch, resources=appdir / 'usr/share')
    desktop = DIST / f'{APP_ID}.desktop'
    desktop.write_text(LINUX_DESKTOP, encoding='utf-8')
    icon = appdir / f'usr/share/icons/hicolor/512x512/apps/{APP_ID}.png'
    scalable = appdir / f'usr/share/icons/hicolor/scalable/apps/{APP_ID}.svg'
    copy(ROOT / 'assets/logos/rproxy-blue.svg', scalable)
    # Render the SVG directly, without ImageMagick's external delegate chain.
    icon.parent.mkdir(parents=True, exist_ok=True)
    run('rsvg-convert', '--width', '512', '--height', '512', '--output', icon, scalable)
    copy(desktop, appdir / f'usr/share/applications/{APP_ID}.desktop')

    # Flatpak packages the clean binary tree; platform libraries come from its runtime.
    manifest = {
        'app-id': APP_ID, 'runtime': 'org.freedesktop.Platform', 'runtime-version': '26.08',
        'sdk': 'org.freedesktop.Sdk', 'command': 'rproxy',
        # Fewer malloc arenas: the default one per thread wastes memory in a tray app.
        'finish-args': ['--env=MALLOC_ARENA_MAX=2', '--filesystem=xdg-run/rproxy:create', '--share=network', '--share=ipc', '--socket=wayland', '--socket=fallback-x11',
                        '--device=dri', '--talk-name=org.kde.StatusNotifierWatcher'],
        # Cargo already stripped debuginfo; this also avoids a host eu-strip (elfutils).
        'modules': [{'name': 'rproxy', 'buildsystem': 'simple', 'build-options': {'no-debuginfo': True},
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
    copy(icon, pkg / f'usr/share/icons/hicolor/512x512/apps/{APP_ID}.png')
    copy(scalable, pkg / f'usr/share/icons/hicolor/scalable/apps/{APP_ID}.svg')
    (pkg / 'usr/bin').mkdir(parents=True)
    (pkg / 'usr/bin/rproxy').symlink_to('/opt/rproxy/rproxy')
    size = sum(p.stat().st_size for p in pkg.rglob('*') if p.is_file())
    (pkg / '.PKGINFO').write_text(f'pkgname = rproxy\npkgver = {VERSION}-1\npkgdesc = Proxy manager\n'
        f'arch = {native_arch}\nbuilddate = {int(time.time())}\nsize = {size}\n'
        'depend = glibc\ndepend = gcc-libs\ndepend = libxkbcommon\ndepend = libxkbcommon-x11\ndepend = libx11\n'
        'depend = libxcursor\ndepend = libxi\ndepend = libxrandr\ndepend = wayland\ndepend = libglvnd\n'
        'depend = dbus\ndepend = xdg-desktop-portal\ndepend = libcap\n'
        'optdepend = xdg-desktop-portal-gtk: file dialogs on GTK desktops\n'
        'optdepend = xdg-desktop-portal-gnome: GNOME desktop integration\n'
        'optdepend = xdg-desktop-portal-kde: KDE desktop integration\n')
    (pkg / '.INSTALL').write_text(PACMAN_INSTALL)
    # pacman reads .PKGINFO as the first entry; names without "./", files owned by root.
    payload = sorted(p.name for p in pkg.iterdir() if p.name not in ('.PKGINFO', '.INSTALL'))
    run('tar', '--zstd', '--owner=0', '--group=0', '--numeric-owner', '-cf', DIST / (name + '.pkg.tar.zst'),
        '-C', pkg, '.PKGINFO', '.INSTALL', *payload)

    tool = DIST / f'linuxdeploy-{native_arch}.AppImage'
    with urlopen(LINUXDEPLOY.format(native_arch), timeout=120) as response:
        content = response.read()
    if hashlib.sha256(content).hexdigest() != LINUXDEPLOY_SHA256[native_arch]:
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
            # A repeated check replaces the bundle installed by the previous one.
            subprocess.run(['flatpak', 'uninstall', '--user', '-y', APP_ID], capture_output=True)
            check_run('flatpak', 'install', '--user', '-y', artifact('.flatpak'))
            # flatpak run needs a session bus; headless CI runners have none.
            bus = [] if os.environ.get('DBUS_SESSION_BUS_ADDRESS') else ['dbus-run-session', '--']
            check_run(*bus, 'flatpak', 'run', APP_ID, '--smoke-test', '--data-dir', '/tmp/rproxy-smoke')
            listing = subprocess.run(['tar', '--zstd', '--numeric-owner', '-tvf', artifact('.pkg.tar.zst')], capture_output=True, text=True,
                                     check=True, timeout=120).stdout.splitlines()
            if not listing or listing[0].split()[-1] != '.PKGINFO':
                raise RuntimeError(f'.PKGINFO must be the first pacman package entry, not {listing[:1]}')
            if any(line.split()[1] != '0/0' for line in listing):
                raise RuntimeError('pacman package files must be owned by 0/0')
            if '.INSTALL' not in {line.split()[-1] for line in listing}:
                raise RuntimeError('The pacman package has no .INSTALL')
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
