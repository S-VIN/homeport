#!/usr/bin/env python3
"""One build entry point on every OS. Requires Rust and Python 3.11+."""
import argparse
import os
from pathlib import Path
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def run(*args):
    subprocess.run(args, cwd=ROOT, check=True)


def prepare_ci(expected_host):
    """Configure the same short artifact path for Cargo, packaging and rust-cache."""
    if expected_host:
        version = subprocess.check_output(['rustc', '-vV'], cwd=ROOT, text=True)
        host = next(line.removeprefix('host: ') for line in version.splitlines() if line.startswith('host: '))
        if host != expected_host:
            raise SystemExit(f'Runner toolchain is {host}; expected {expected_host}')
    # Hosted runners are isolated per job. Keep Windows paths short enough for
    # sccache's rustc dependency scan, which expands the arguments again.
    target = Path(ROOT.anchor) / 'b' if os.name == 'nt' else Path('/tmp/rproxy-build')
    target.mkdir(parents=True, exist_ok=True)
    with open(os.environ['GITHUB_ENV'], 'a', encoding='utf-8') as env:
        env.write(f'CARGO_TARGET_DIR={target}\n')
    # rust-cache joins this value to the workspace, so it must be relative.
    with open(os.environ['GITHUB_OUTPUT'], 'a', encoding='utf-8') as output:
        output.write(f'cache-target={os.path.relpath(target, ROOT)}\n')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--prepare-ci', nargs='?', const='', metavar='HOST',
                        help='prepare CI artifact paths, optionally checking the native Rust host')
    parser.add_argument('--web-only', action='store_true')
    parser.add_argument('--native-only', action='store_true')
    args, cargo_args = parser.parse_known_args()
    if args.prepare_ci is not None:
        prepare_ci(args.prepare_ci)
        return
    target = Path(os.environ.get('CARGO_TARGET_DIR', 'target'))
    if not target.is_absolute():
        target = ROOT / target
    target = target.resolve()
    # rust-cache prunes executables under target/; keep the CLI in its own cache.
    cli = ROOT / '.cache/wasm-bindgen'
    os.environ['PATH'] = str(cli / 'bin') + os.pathsep + os.environ['PATH']
    if not args.native_only:
        lock = tomllib.loads((ROOT / 'Cargo.lock').read_text())
        version = next(p['version'] for p in lock['package'] if p['name'] == 'wasm-bindgen')
        run('rustup', 'target', 'add', 'wasm32-unknown-unknown')
        try:
            installed = subprocess.check_output(['wasm-bindgen', '--version'], text=True).strip()
        except FileNotFoundError:
            installed = ''
        if installed != f'wasm-bindgen {version}':
            run('cargo', 'install', 'wasm-bindgen-cli', '--locked', '--version', version, '--root', str(cli))
        run('cargo', 'build', '--locked', '-p', 'rproxy-remote', '--lib', '--target', 'wasm32-unknown-unknown', '--profile', 'web')
        run('wasm-bindgen', '--target', 'web', '--no-typescript', '--out-dir', 'web/pkg', str(target / 'wasm32-unknown-unknown/web/rproxy_remote.wasm'))
    if not args.web_only:
        run('cargo', 'build', '--locked', '--bin', 'rproxy', *cargo_args)


if __name__ == '__main__':
    main()
