"""Exercise the real Compose with custom ports, credentials, bind mounts and restarts.

Build homeport:deployment-test, then run this script with aiohttp installed.
COMPOSE_BIN may point to a standalone Docker Compose binary.
"""
import asyncio
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import socket
import subprocess
from tempfile import TemporaryDirectory

import aiohttp
from aiohttp.abc import AbstractResolver

ROOT = Path(__file__).resolve().parent.parent
IMAGE = os.environ.get('HOMEPORT_TEST_IMAGE', 'homeport:deployment-test')


def command(*args, check=True):
    result = subprocess.run(args, capture_output=True, text=True, check=check)
    return result.stdout.strip()


def free_ports(count):
    sockets = []
    try:
        for _ in range(count):
            sock = socket.socket()
            sock.bind(('127.0.0.1', 0))
            sockets.append(sock)
        return [sock.getsockname()[1] for sock in sockets]
    finally:
        for sock in sockets:
            sock.close()


class LocalResolver(AbstractResolver):
    async def resolve(self, host, port=0, family=socket.AF_INET):
        return [dict(hostname=host, host='127.0.0.1', port=port,
                     family=socket.AF_INET, proto=0, flags=0)]

    async def close(self):
        pass


async def check_login(base, login, username, password, old_password=None):
    connector = aiohttp.TCPConnector(resolver=LocalResolver(), ssl=False)
    async with aiohttp.ClientSession(connector=connector, headers={
        'Accept': 'text/html',
        'User-Agent': 'Mozilla/5.0 AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36',
    }) as session:
        for attempt in range(100):
            try:
                async with session.get(login, allow_redirects=False) as response:
                    if response.status == 200:
                        break
            except aiohttp.ClientError:
                pass
            await asyncio.sleep(.1)
        else:
            raise RuntimeError('Auth stack did not start')
        for route in ('/', '/ws', '/homeport_remote.js', '/homeport_remote_bg.wasm'):
            async with session.get(base + route, allow_redirects=False) as response:
                assert response.status in (302, 303, 307), (route, response.status)
                assert response.headers['Location'].startswith(login)
        async with session.post(login + '/api/user/login', json={
            'username': username, 'password': old_password or 'wrong',
        }) as response:
            assert response.status in (401, 403), response.status
        async with session.post(login + '/api/user/login', json={
            'username': username, 'password': password,
        }) as response:
            assert response.status == 200, response.status
        async with session.get(base) as response:
            assert response.status == 200 and '<html' in await response.text()
        async with session.ws_connect(base + '/ws', origin=base, autoping=False) as ws:
            await ws.ping(b'homeport')
            async with asyncio.timeout(10):
                message = await ws.receive()
                assert message.type == aiohttp.WSMsgType.PONG and message.data == b'homeport'
        for origin in ('https://evil.example', None):
            try:
                await session.ws_connect(base + '/ws', origin=origin)
            except aiohttp.WSServerHandshakeError as error:
                assert error.status == 403, error.status
            else:
                raise AssertionError('Untrusted WebSocket origin accepted')
        async with session.post(login + '/api/user/logout') as response:
            assert response.status == 200
        async with session.get(base + '/ws', allow_redirects=False) as response:
            assert response.status in (302, 303, 307)


async def main():
    cli = [os.environ['COMPOSE_BIN']] if os.environ.get('COMPOSE_BIN') else ['docker', 'compose']
    ui, auth, http, https = free_ports(4)
    base, login = f'https://panel.homeport.test:{https}', f'https://auth.homeport.test:{https}'
    password = secrets.token_urlsafe(16) + " $price 'quoted' $(not-a-command)"
    with TemporaryDirectory(prefix='homeport-compose-') as directory:
        root = Path(directory)
        state = root / 'state'
        state.mkdir()
        shutil.copy2(ROOT / 'docker/compose.yaml', root / 'compose.yaml')
        caddy = (ROOT / 'docker/Caddyfile').read_text()
        caddy = re.sub(r'tls \{\s*issuer acme \{\s*disable_tlsalpn_challenge\s*\}\s*\}', 'tls internal', caddy)
        (root / 'Caddyfile').write_text(caddy)
        settings = dict(HOMEPORT_IMAGE=IMAGE, HOMEPORT_PANEL_DOMAIN='panel.homeport.test',
                        HOMEPORT_AUTH_DOMAIN='auth.homeport.test', DATA_DIR=str(state),
                        TINYAUTH_USERNAME='smoketest', TINYAUTH_PASSWORD=password,
                        HOMEPORT_UI_PORT=str(ui), TINYAUTH_PORT=str(auth), HTTP_PORT=str(http),
                        HTTPS_PORT=str(https), CADDY_BIND='127.0.0.1')

        def write_env():
            lines = (ROOT / 'docker/.env.example').read_text().splitlines()
            for i, line in enumerate(lines):
                key = line.split('=', 1)[0]
                if key in settings:
                    value = settings[key].replace("'", "\\'")
                    lines[i] = f"{key}='{value}'"
            (root / '.env').write_text('\n'.join(lines) + '\n')
            (root / '.env').chmod(0o600)

        def compose(*args, check=True):
            return command(*cli, '--project-directory', str(root), '-p', 'homeport-test-' + root.name,
                           '-f', str(root / 'compose.yaml'), *args, check=check)

        write_env()
        try:
            config = json.loads(compose('config', '--format', 'json'))
            for service in config['services'].values():
                assert service['network_mode'] == 'host' and not service.get('ports')
                for volume in service['volumes']:
                    assert volume['type'] == 'bind', volume
                    if volume['target'] != '/etc/caddy/Caddyfile':
                        assert Path(volume['source']).is_relative_to(state), volume
            await asyncio.to_thread(compose, 'up', '-d', '--wait', '--wait-timeout', '90')
            container = compose('ps', '-q', 'tinyauth')
            env = json.loads(command('docker', 'inspect', '--format', '{{json .Config.Env}}', container))
            assert 'TINYAUTH_PASSWORD=' + password in env
            await check_login(base, login, settings['TINYAUTH_USERNAME'], password)
            # Actual initialized database plus markers in all persistent mounts.
            compose('exec', '-T', 'homeport', 'test', '-s', '/data/homeport-plugins.sqlite3')
            for service, path in [('homeport', '/data'), ('tinyauth', '/data'), ('caddy', '/data'), ('caddy', '/config')]:
                compose('exec', '-T', service, 'sh', '-ec', f"printf persistent > {path}/smoke-marker")
            ca = compose('exec', '-T', 'caddy', 'sha256sum', '/data/caddy/pki/authorities/local/root.crt')
            await asyncio.to_thread(compose, 'down')
            settings['TINYAUTH_PASSWORD'] = secrets.token_urlsafe(24) + ' $updated'
            write_env()
            await asyncio.to_thread(compose, 'up', '-d', '--wait', '--wait-timeout', '90')
            for service, path in [('homeport', '/data'), ('tinyauth', '/data'), ('caddy', '/data'), ('caddy', '/config')]:
                assert compose('exec', '-T', service, 'cat', f'{path}/smoke-marker') == 'persistent'
            assert compose('exec', '-T', 'caddy', 'sha256sum', '/data/caddy/pki/authorities/local/root.crt') == ca
            await check_login(base, login, settings['TINYAUTH_USERNAME'], settings['TINYAUTH_PASSWORD'], password)
            print('PASS: real Compose; custom ports; plaintext credentials and rotation; protected assets/WS; origin checks; bind data and CA survive recreation')
        except Exception:
            logs = compose('logs', '--no-color', '--tail', '20', check=False)
            for value in (password, settings['TINYAUTH_PASSWORD']):
                logs = logs.replace(value, '<redacted>')
            print(logs)
            raise
        finally:
            await asyncio.to_thread(compose, 'down', '--remove-orphans', check=False)
            # Test-created bind files belong to container root; remove only this temporary tree.
            command('docker', 'run', '--rm', '--user', '0:0', '-v', f'{state}:/cleanup:Z', '--entrypoint', 'sh',
                    IMAGE, '-ec', 'find /cleanup -mindepth 1 -delete')


if __name__ == '__main__':
    asyncio.run(main())
