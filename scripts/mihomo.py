#!/usr/bin/env python3
"""Verify/copy vendored Mihomo; --update explicitly refreshes it from pinned archives."""
import argparse
import hashlib
import gzip
import io
import zipfile
from concurrent.futures import ThreadPoolExecutor
from urllib.request import Request, urlopen
import json
from pathlib import Path
import shutil

ROOT = Path(__file__).resolve().parents[1] / 'assets/mihomo'
PLATFORMS = [(system, arch) for system in ('linux', 'win32', 'darwin') for arch in ('x64', 'arm64')]


def binary_path(system, arch):
    if (system, arch) not in PLATFORMS:
        raise ValueError(f'Unsupported Mihomo platform: {system}/{arch}')
    return ROOT / system / arch / ('mihomo.exe' if system == 'win32' else 'mihomo')


def verify(system, arch):
    manifest = json.loads((ROOT / 'manifest.json').read_text(encoding='utf-8'))
    records = [r for r in manifest['binaries'] if (r['os'], r['architecture']) == (system, arch)]
    if len(records) != 1:
        raise ValueError(f'Missing or duplicate Mihomo manifest entry: {system}/{arch}')
    path = binary_path(system, arch)
    record = records[0]
    if record['path'] != path.relative_to(ROOT).as_posix():
        raise ValueError(f'Unexpected Mihomo path: {record["path"]}')
    if hashlib.sha256(path.read_bytes()).hexdigest() != record['sha256']:
        raise ValueError(f'Mihomo SHA256 mismatch: {path}')
    return path


def update_binaries():
    """Maintainer-only operation: archive URLs and digests are pinned in the manifest."""
    manifest_path = ROOT / 'manifest.json'
    manifest = json.loads(manifest_path.read_text(encoding='utf-8'))
    records = manifest['binaries']
    if sorted((r['os'], r['architecture']) for r in records) != sorted(PLATFORMS):
        raise ValueError('The manifest must contain each supported platform exactly once')

    def fetch(record):
        path = binary_path(record['os'], record['architecture'])
        if path.relative_to(ROOT).as_posix() != record['path']:
            raise ValueError('Manifest path does not match platform')
        request = Request(record['url'], headers={'User-Agent': 'rproxy'})
        with urlopen(request, timeout=120) as response:
            archive = response.read()
        if hashlib.sha256(archive).hexdigest() != record['archive_sha256']:
            raise ValueError(f"Archive SHA256 mismatch: {record['path']}")
        if record['url'].endswith('.gz'):
            binary = gzip.decompress(archive)
        else:
            with zipfile.ZipFile(io.BytesIO(archive)) as zipped:
                names = [n for n in zipped.namelist() if n.endswith('.exe')]
                if len(names) != 1:
                    raise ValueError('Unexpected Mihomo archive contents')
                binary = zipped.read(names[0])
        path.parent.mkdir(parents=True, exist_ok=True)
        temporary = path.with_suffix('.download')
        temporary.write_bytes(binary)
        temporary.chmod(0o755)
        temporary.replace(path)
        record['sha256'] = hashlib.sha256(binary).hexdigest()
        print(f"Updated {record['path']}", flush=True)

    with ThreadPoolExecutor(max_workers=3) as pool:
        list(pool.map(fetch, records))
    manifest_path.write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8')


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--os', choices=['linux', 'win32', 'darwin'])
    p.add_argument('--arch', choices=['x64', 'amd64', 'arm64'])
    p.add_argument('--output', type=Path, help='Copy the selected binary and license into this directory')
    p.add_argument('--update', action='store_true', help='Download all pinned archives and update the vendored files')
    args = p.parse_args()
    if args.update:
        if args.os or args.arch or args.output:
            p.error('--update refreshes the whole set; it cannot be combined with selection options')
        update_binaries()
        return
    if args.arch == 'amd64':
        args.arch = 'x64'
    if bool(args.os) != bool(args.arch) or (args.output and not args.os):
        p.error('--os and --arch must be supplied together; --output requires a single platform')
    for system, arch in [(args.os, args.arch)] if args.os else PLATFORMS:
        path = verify(system, arch)
        if args.output:
            args.output.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, args.output / path.name)
            (args.output / path.name).chmod(0o755)
            shutil.copy2(ROOT / 'LICENSE', args.output / 'LICENSE')
        print(f'Verified Mihomo {system}/{arch}')


if __name__ == '__main__':
    main()
