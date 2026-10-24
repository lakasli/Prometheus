#!/usr/bin/env python3
"""Two synthetic images against the existing DeepSeek account, no user images.

Read only the active provider and its credential. Never change configuration,
connect a channel, or write source content/credentials to the metadata report.
This capability probe is not a delivered image conversation or attachment API.
"""
import argparse
import base64
import binascii
from contextlib import closing
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import secrets
import sqlite3
import stat
import struct
import urllib.error
import urllib.request
import uuid
import zlib


def png(shape, color, count):
    colors = {'red': (240, 20, 20), 'blue': (20, 20, 240), 'green': (20, 180, 20)}
    width, height = 240, 100
    raw = bytearray()
    for y in range(height):
        raw.append(0)
        for x in range(width):
            active = False
            for index in range(count):
                dx, dy = x - (40 + 80 * index), y - 50
                active |= dx * dx + dy * dy <= 24 * 24 if shape == 'circle' else abs(dx) <= 24 and abs(dy) <= 24
            raw.extend(colors[color] if active else (255, 255, 255))

    def chunk(kind, payload):
        return struct.pack('!I', len(payload)) + kind + payload + struct.pack('!I', binascii.crc32(kind + payload) & 0xffffffff)

    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('!2I5B', width, height, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(raw)) + chunk(b'IEND', b'')


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_args, **_kwargs):
        return None


def configured_key(source):
    with closing(sqlite3.connect((source / 'dsh.db').as_uri() + '?mode=ro', uri=True)) as db:
        config = db.execute("SELECT c.config_json FROM config_revisions c JOIN settings s ON s.key='active_revision' AND c.revision=CAST(s.value AS INTEGER)").fetchone()
        model = json.loads(config[0])['model']
        if model['base_url'].rstrip('/') not in ('https://api.deepseek.com', 'https://api.deepseek.com/v1'):
            raise ValueError('existing provider is not the expected official HTTPS endpoint')
        reference = str(uuid.UUID(model['credential_ref']))
        if not db.execute("SELECT 1 FROM secrets WHERE id=? AND kind='model_api_key'", (reference,)).fetchone():
            raise ValueError('existing model credential metadata unavailable')
    path = source / 'secrets' / reference
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or path.resolve().parent != (source / 'secrets').resolve():
        raise ValueError('invalid private credential file')
    if os.name != 'nt' and (metadata.st_mode & 0o077 or metadata.st_uid != os.getuid()):
        raise ValueError('credential is not owner-only')
    key = path.read_text(encoding='utf-8')
    if not key or len(key) > 8192 or not all(33 <= ord(c) <= 126 for c in key):
        raise ValueError('invalid model credential')
    return model['base_url'].rstrip('/'), key


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-data-dir', required=True)
    parser.add_argument('--model', choices=['deepseek-flash'], default='deepseek-flash')
    parser.add_argument('--report', required=True)
    args = parser.parse_args()
    report = {'kind': 'synthetic vision capability preflight', 'started_at': datetime.now(timezone.utc).isoformat(),
              'model': args.model, 'status': 'running', 'requests': 0, 'max_requests': 2,
              'production_model_configuration_read': True, 'production_database_modified': False,
              'real_user_content_accessed': False, 'user_images_transmitted': False,
              'image_chat_delivered': False, 'checks': [], 'probe_source_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
    exit_code = 1
    try:
        endpoint, key = configured_key(Path(args.source_data_dir).resolve(strict=True))
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        # Opposite shape and different colour/count: no answer is in the text.
        first = secrets.choice(['circle', 'square'])
        colors = secrets.SystemRandom().sample(['red', 'blue', 'green'], 2)
        for index, shape in enumerate([first, 'square' if first == 'circle' else 'circle']):
            count = secrets.choice([1, 2, 3])
            expected = {'shape': shape, 'color': colors[index], 'count': count}
            picture = png(**expected)
            body = json.dumps({'model': args.model, 'stream': False, 'max_tokens': 300,
                'thinking': {'type': 'disabled'}, 'response_format': {'type': 'json_object'},
                'messages': [{'role': 'user', 'content': [
                    {'type': 'text', 'text': 'Inspect this synthetic image. Return only JSON with shape (circle or square), color (red, blue or green), and count (integer). Ignore the white background. Do not add other fields.'},
                    {'type': 'image_url', 'image_url': {'url': 'data:image/png;base64,' + base64.b64encode(picture).decode(), 'detail': 'low'}}]}]}).encode()
            request = urllib.request.Request(endpoint + '/chat/completions', data=body,
                headers={'Authorization': 'Bearer ' + key, 'Content-Type': 'application/json'}, method='POST')
            report['requests'] += 1
            try:
                with opener.open(request, timeout=45) as response:
                    raw = response.read(262145)
                    if len(raw) > 262144:
                        raise ValueError('response limit')
                envelope = json.loads(raw)
                output = json.loads(envelope['choices'][0]['message']['content'])
                matched = isinstance(output, dict) and output == expected and type(output.get('count')) is int
                report['checks'].append({'case': index + 1, 'response_matches_synthetic_pixels': matched})
                if not matched:
                    report['status'] = 'inconclusive'
                    exit_code = 2
                    break
            except urllib.error.HTTPError as error:
                report['http_status'] = error.code
                report['status'] = 'unavailable'
                exit_code = 2
                break
        else:
            report['status'] = 'passed'
            exit_code = 0
        key = None
    except Exception as error:
        report['status'] = 'failed'
        report['error_type'] = type(error).__name__
    report['finished_at'] = datetime.now(timezone.utc).isoformat()
    path = Path(args.report)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
    print(json.dumps({'status': report['status'], 'requests': report['requests'], 'image_chat_delivered': False}))
    return exit_code


if __name__ == '__main__':
    raise SystemExit(main())
