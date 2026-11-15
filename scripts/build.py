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


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--web-only', action='store_true')
    parser.add_argument('--native-only', action='store_true')
    args, cargo_args = parser.parse_known_args()
    target = Path(os.environ.get('CARGO_TARGET_DIR', 'target'))
    if not target.is_absolute():
        target = ROOT / target
    target = target.resolve()
    # A local wasm-bindgen CLI lives outside target/ so `cargo clean` keeps it;
    # CI puts the prebuilt release on PATH instead.
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
        run('wasm-bindgen', '--target', 'web', '--no-typescript', '--remove-name-section', '--out-dir', 'web/pkg', str(target / 'wasm32-unknown-unknown/web/rproxy_remote.wasm'))
    if not args.web_only:
        run('cargo', 'build', '--locked', '--bin', 'rproxy', '--bin', 'rproxy-core-helper', *cargo_args)


if __name__ == '__main__':
    main()
