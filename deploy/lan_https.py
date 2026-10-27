#!/usr/bin/env python3
"""Apply a pinned, user-level LAN HTTPS profile without rebuilding DSH.

The application release and data stay in place. Only this project's managed
proxy, backend drop-in and public trust certificate are installed. CA keys stay
in the private persistent proxy storage. No machine-wide trust store is edited.
"""
import argparse
import datetime
import hashlib
import ipaddress
import json
import os
import pathlib
import platform
import re
import shutil
import subprocess
import tarfile
import tempfile
import time
import urllib.request

ROOT = pathlib.Path('/home/campanus/apps/dsh')
HOST = '192.168.189.191'
PORT = 8086
NETWORK = '192.168.189.0/24'
ORIGIN = f'https://{HOST}:{PORT}'
MARKER = '# Managed by DSH deploy/lan_https.py\n'


def caddyfile(root=ROOT, host=HOST, port=PORT, network=NETWORK, upstream=8419):
    root = root.as_posix() if isinstance(root, pathlib.PurePath) else str(root)
    address = ipaddress.IPv4Address(host)
    subnet = ipaddress.IPv4Network(network)
    if (not address.is_private or address.is_loopback or address not in subnet
            or not subnet.is_private or not 1024 <= port <= 65535
            or not 1024 <= upstream <= 65535):
        raise ValueError('invalid explicit private LAN endpoint')
    if re.search(r'[\s{}"\\]', str(root)):
        raise ValueError('unsupported proxy storage path')
    return MARKER + f'''{{
    admin off
    auto_https disable_redirects
    skip_install_trust
    storage file_system {{
        root {root}/shared/https/storage
    }}
    servers {{
        protocols h1 h2
    }}
}}

https://{host}:{port} {{
    bind {host}
    @outside not remote_ip {subnet}
    respond @outside "LAN access only" 403
    tls internal
    reverse_proxy 127.0.0.1:{upstream}
}}

https://:{port} {{
    bind {host}
    respond "Invalid management host" 403
}}
'''


def backend_dropin(root=ROOT):
    root = root.as_posix() if isinstance(root, pathlib.PurePath) else str(root)
    return MARKER + f'''[Unit]
Wants=dsh-https.service

[Service]
ExecStart=
ExecStart={root}/current/bin/dshd serve --data-dir {root}/shared/data --listen 127.0.0.1:8419 --origin {ORIGIN} --secure-cookie
'''


def proxy_unit(binary, root=ROOT):
    root = root.as_posix() if isinstance(root, pathlib.PurePath) else str(root)
    return MARKER + f'''[Unit]
Description=DSH LAN HTTPS on {HOST}:{PORT}
After=network-online.target dsh.service
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory={root}/shared/https
Environment=XDG_DATA_HOME={root}/shared/https/data
Environment=XDG_CONFIG_HOME={root}/shared/https/config
ExecStart={binary} run --config {root}/shared/config/Caddyfile --adapter caddyfile
Restart=on-failure
RestartSec=5
TimeoutStopSec=15
UMask=0077
NoNewPrivileges=true
PrivateTmp=true
MemoryHigh=96M
MemoryMax=192M
TasksMax=64

[Install]
WantedBy=default.target
'''


def command(args, **kwargs):
    return subprocess.run([str(v) for v in args], check=True, **kwargs)


def private_dir(path):
    created = not path.exists()
    path.mkdir(parents=True, exist_ok=True, mode=0o700)
    if path.is_symlink() or path.stat().st_uid != os.getuid():
        raise RuntimeError('unowned or symlinked managed directory')
    if created:
        path.chmod(0o700)


def atomic_write(path, data):
    private_dir(path.parent)
    if path.is_symlink():
        raise RuntimeError('refusing to replace a symlink')
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as out:
        temporary = pathlib.Path(out.name)
        out.write(data)
        out.flush()
        os.fsync(out.fileno())
    temporary.chmod(0o600)
    temporary.replace(path)


def install_runtime(source):
    spec = json.loads((source / 'deploy/caddy-runtime.json').read_text())
    cache = ROOT / 'cache/https'
    private_dir(cache)
    archive = cache / f'caddy-{spec["version"]}-linux-arm64.tar.gz'
    if not archive.exists():
        partial = archive.with_suffix('.download')
        if partial.exists():
            raise RuntimeError('an earlier runtime download needs inspection')
        command(['curl', '--proto', '=https', '--proto-redir', '=https', '--tlsv1.2',
                 '-fLsS', '--max-time', '180', spec['url'], '-o', partial])
        if hashlib.sha512(partial.read_bytes()).hexdigest() != spec['sha512']:
            raise RuntimeError('Caddy download digest mismatch; partial retained')
        partial.replace(archive)
    if archive.is_symlink() or hashlib.sha512(archive.read_bytes()).hexdigest() != spec['sha512']:
        raise RuntimeError('cached Caddy archive digest mismatch')
    with tarfile.open(archive, 'r:gz') as package:
        member = package.getmember('caddy')
        if not member.isfile() or not 0 < member.size <= 128 * 1024 * 1024:
            raise RuntimeError('invalid Caddy archive executable')
        data = package.extractfile(member).read()
    binary = ROOT / f'shared/https/bin/caddy-{spec["version"]}'
    if binary.exists() and (binary.is_symlink() or binary.read_bytes() != data):
        raise RuntimeError('managed Caddy executable changed unexpectedly')
    if not binary.exists():
        atomic_write(binary, data)
        binary.chmod(0o700)
    version = command([binary, 'version'], capture_output=True, text=True).stdout.strip()
    if not version.startswith('v' + spec['version'] + ' '):
        raise RuntimeError('Caddy version mismatch')
    return binary, dict(version=version, sha256=hashlib.sha256(data).hexdigest(),
                        archive_sha512=spec['sha512'])


def wait_ready(ca, expected_sha):
    import ssl
    client = urllib.request.build_opener(urllib.request.ProxyHandler({}),
        urllib.request.HTTPSHandler(context=ssl.create_default_context(cafile=str(ca))))
    for _ in range(40):
        try:
            with client.open(ORIGIN + '/health/ready', timeout=2) as response:
                if json.load(response)['commit'] == expected_sha:
                    return
        except (OSError, ValueError):
            pass
        time.sleep(.5)
    raise RuntimeError('LAN HTTPS endpoint did not become ready')


def apply(source, expected_sha):
    if platform.system() != 'Linux' or platform.machine() != 'aarch64':
        raise RuntimeError('apply requires the authorized Linux aarch64 host')
    import fcntl
    import pwd
    os.umask(0o077)
    if pwd.getpwuid(os.getuid()).pw_name != 'campanus':
        raise RuntimeError('apply as campanus')
    if not re.fullmatch('[0-9a-f]{40}', expected_sha):
        raise ValueError('a full script source commit is required')
    for arguments, expected in [(['rev-parse', 'HEAD'], expected_sha),
                                (['status', '--porcelain'], ''),
                                (['remote', 'get-url', 'origin'], 'http://8.136.201.30:8418/Campanus/DSH.git')]:
        if command(['git', '-C', source, *arguments], capture_output=True, text=True).stdout.strip() != expected:
            raise RuntimeError('script checkout identity or cleanliness mismatch')
    if ROOT.is_symlink() or ROOT.stat().st_uid != os.getuid():
        raise RuntimeError('unexpected deployment root ownership')
    with (ROOT / '.deployment.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        release = (ROOT / 'current').resolve(strict=True)
        if release.parent != ROOT / 'releases':
            raise RuntimeError('current release is outside the managed root')
        program_sha = command(['git', '-C', release / 'source', 'rev-parse', 'HEAD'],
                              capture_output=True, text=True).stdout.strip()
        if json.loads((release / 'web/release.json').read_text())['commit'] != program_sha:
            raise RuntimeError('application/frontend release mismatch')
        directory = ROOT / 'shared/https'
        private_dir(directory)
        binary, runtime = install_runtime(source)
        report_path = ROOT / f'shared/logs/lan-https-{expected_sha}-preflight.json'
        command(['python3', source / 'tools/lan-https-smoke.py', '--source', source,
                 '--binary', release / 'bin/dshd', '--web-dir', release / 'web',
                 '--caddy', binary, '--root', ROOT, '--expected-commit', program_sha,
                 '--report', report_path])
        units = pathlib.Path.home() / '.config/systemd/user'
        paths = {ROOT / 'shared/config/Caddyfile': caddyfile(),
                 units / 'dsh-https.service': proxy_unit(binary),
                 units / 'dsh.service.d/10-lan-https.conf': backend_dropin()}
        originals = {}
        stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
        backup = ROOT / f'shared/backups/lan-https-{expected_sha}-{stamp}'
        private_dir(backup)
        for number, (path, content) in enumerate(paths.items()):
            old = path.read_bytes() if path.exists() else None
            if path.is_symlink() or old is not None and not old.startswith(MARKER.encode()):
                raise RuntimeError('an unmanaged proxy file already exists')
            originals[path] = old
            if old is not None:
                atomic_write(backup / str(number), old)
        old_enabled = subprocess.run(['systemctl', '--user', 'is-enabled', 'dsh-https.service'],
                                     capture_output=True, text=True).returncode == 0
        old_active = subprocess.run(['systemctl', '--user', 'is-active', '--quiet', 'dsh-https.service']).returncode == 0
        atomic_write(backup / 'manifest.json', json.dumps(dict(
            files=[dict(path=str(path),backup=str(number) if old is not None else None)
                   for number,(path,old) in enumerate(originals.items())],
            previous_proxy_enabled=old_enabled, previous_proxy_active=old_active,
            application_release=str(release)), indent=2).encode())
        try:
            for path, content in paths.items():
                atomic_write(path, content.encode())
            command([binary, 'validate', '--config', ROOT / 'shared/config/Caddyfile', '--adapter', 'caddyfile'])
            command(['systemctl', '--user', 'daemon-reload'])
            command(['systemctl', '--user', 'enable', 'dsh-https.service'])
            command(['systemctl', '--user', 'restart', 'dsh.service'])
            command(['systemctl', '--user', 'restart', 'dsh-https.service'])
            ca = directory / 'storage/pki/authorities/local/root.crt'
            wait_ready(ca, program_sha)
            command(['openssl', 'x509', '-in', ca, '-outform', 'DER', '-out', directory / 'DSH-LAN-Root-CA.cer'])
            profile = dict(script_commit=expected_sha, program_commit=program_sha,
                           applied_at=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                           origin=ORIGIN, allowed_network=NETWORK, backend='127.0.0.1:8419',
                           runtime=runtime, public_ca=str(directory / 'DSH-LAN-Root-CA.cer'),
                           preflight_report=str(report_path), backup_manifest=str(backup / 'manifest.json'),
                           machine_trust_store_modified=False)
            atomic_write(ROOT / 'shared/config/lan-https.json', json.dumps(profile, indent=2).encode())
            print(json.dumps(profile), flush=True)
        except BaseException:
            subprocess.run(['systemctl', '--user', 'stop', 'dsh-https.service'], check=False)
            if not old_enabled:
                subprocess.run(['systemctl', '--user', 'disable', 'dsh-https.service'], check=False)
            for path, old in originals.items():
                if old is None:
                    path.unlink(missing_ok=True)
                else:
                    atomic_write(path, old)
            command(['systemctl', '--user', 'daemon-reload'])
            command(['systemctl', '--user', 'restart', 'dsh.service'])
            if old_active:
                command(['systemctl', '--user', 'restart', 'dsh-https.service'])
            raise


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--expected-script-commit', required=True)
    args = parser.parse_args()
    apply(pathlib.Path(__file__).resolve().parents[1], args.expected_script_commit)
