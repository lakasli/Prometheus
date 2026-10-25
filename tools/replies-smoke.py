#!/usr/bin/env python3
"""Real daemon / synthetic provider checks for structured replies and scoped media.
No production data or credentials. Not evidence of real QQ delivery.
"""
import argparse
import base64
import hashlib
import http.server
import importlib.util
import json
import pathlib
import platform
import struct
import sys
import tempfile
import threading
import uuid
import zlib
import urllib.request

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('reply_helpers', pathlib.Path(__file__).with_name('memory-smoke.py'))
helpers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helpers)
P = helpers.PREFIX


def png():
    def chunk(kind, data):
        return struct.pack('!I', len(data)) + kind + data + struct.pack('!I', zlib.crc32(kind + data))
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('!IIBBBBB', 1, 1, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(b'\x00\x10\x20\x30')) + chunk(b'IEND', b'')


class Provider(http.server.BaseHTTPRequestHandler):
    output = ''
    received = []

    def log_message(self, *_):
        pass

    def do_POST(self):
        value = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.received.append(value)
        payload = json.dumps({'choices': [{'message': {'content': self.output}}]}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--web-dir', required=True)
    parser.add_argument('--report', required=True)
    args = parser.parse_args()
    report = {'kind': 'structured replies and scoped media', 'commit': None, 'started_at': helpers.utc_now(),
              'architecture': platform.machine(), 'production_data_accessed': False, 'real_qq_tested': False,
              'real_external_model_tested': False, 'status': 'running', 'checks': []}

    def check(name, condition=True):
        if not condition:
            raise AssertionError(name)
        report['checks'].append(name)
        print('PASS ' + name, flush=True)

    code, stage = 0, 'initialize'
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='dsh-reply-') as temp:
            h = helpers.Harness(str(pathlib.Path(args.binary).resolve()), str(pathlib.Path(args.web_dir).resolve()), temp, helpers.MockState())
            try:
                h.start()
                h.bootstrap()
                report['commit'] = h.commit
                h.configure(server.server_port)
                c = h.conversation('Synthetic reply acceptance')

                def turn(output):
                    Provider.output = output
                    submitted = h.submit(c, 'Synthetic question ' + uuid.uuid4().hex)
                    return h.wait_run(submitted)

                stage = 'structured model pipeline'
                first = turn(json.dumps([{'type': 'text', 'text': '你好。', 'delay_ms': 0}, {'type': 'text', 'text': '我在这里。', 'delay_ms': 1}], ensure_ascii=False))
                check('one-sentence JSON items and enforced interval persist', first['status'] == 'completed' and first['response'] == '你好。\n我在这里。' and first['reply_plan']['items'][1]['delay_ms'] == 1500)
                check('host protocol reaches actual provider request', '宿主回复协议' in Provider.received[-1]['messages'][0]['content'])
                messages = h.request('GET', P + '/conversations/' + c + '/messages')[0]
                check('message history returns presentation objects without raw JSON text', messages['items'][-1]['reply_plan']['items'][0]['text'] == '你好。')
                check('multiple sentences in one object are rejected', turn('[{"type":"text","text":"一句。两句。"}]')['status'] == 'failed')
                check('more than four channel messages are rejected', turn(json.dumps([{'type':'text','text':'一句。'}] * 5))['status'] == 'failed')
                check('model URLs cannot become media actions', turn('[{"type":"image","text":"图片。","url":"http://127.0.0.1"}]')['status'] == 'failed')
                fallback = turn('[{"type":"voice","text":"保留这句话。"}]')
                check('disabled speech degrades to readable text', fallback['status'] == 'completed' and fallback['reply_plan']['items'][0]['type'] == 'text')
                stage = 'media ownership and invalidation'
                root = P + '/conversations/' + c + '/media'
                h.request('GET', root, expected=401, authenticated=False)
                h.request('POST', root, {'kind': 'image', 'data_base64': base64.b64encode(png()).decode()}, expected=403, with_csrf=False)
                asset = h.request('POST', root, {'kind': 'image', 'data_base64': base64.b64encode(png()).decode()}, expected=201)[0]
                with h.client.open(urllib.request.Request(h.base + root + '/' + asset['id'], headers={'Origin':h.base})) as response:
                    data, headers = response.read(), response.headers
                check('private canonical PNG download matches content hash', hashlib.sha256(data).hexdigest() == asset['sha256'] and headers['X-Content-Type-Options'] == 'nosniff')
                h.request('POST', root, {'kind': 'video', 'data_base64': base64.b64encode(b'fake mp4').decode()}, expected=400)
                check('fake video and missing authentication or CSRF are rejected')
                allowed = turn(json.dumps([{'type':'image','text':'这是图片。','asset_id':asset['id']}], ensure_ascii=False))
                check('only scoped known media can be referenced', allowed['status'] == 'completed' and allowed['reply_plan']['items'][0]['asset_id'] == asset['id'])
                other = h.conversation('Other synthetic scope')
                h.request('GET', P + '/conversations/' + other + '/media/' + asset['id'], expected=404)
                with h.db() as db:
                    db.execute('UPDATE conversation_media_assets SET owner_id=? WHERE id=?', ('foreign-owner', asset['id']))
                h.request('GET', root + '/' + asset['id'], expected=404)
                check('cross-conversation and cross-owner reads fail closed')
                expiry = h.request('POST', root, {'kind':'image','data_base64':base64.b64encode(png()).decode()}, expected=201)[0]
                with h.db() as db:
                    db.execute('UPDATE conversation_media_assets SET expires_at=0 WHERE id=?', (expiry['id'],))
                h.request('GET', root + '/' + expiry['id'], expected=404)
                check('expired media is inaccessible and reclaimed')
                stage = 'restart durability'
                h.stop()
                h.start()
                h.login()
                reread = h.request('GET', P + '/runs/' + first['id'])[0]
                check('reply plan survives process restart without model replay', reread['reply_plan'] == first['reply_plan'])
                report['status'] = 'passed'
            finally:
                h.close()
    except Exception as error:
        code = 1
        report.update(status='failed', failed_phase=stage, error_type=type(error).__name__)
        print('FAIL ' + stage + ': ' + type(error).__name__, file=sys.stderr)
    finally:
        server.shutdown()
        report['finished_at'] = helpers.utc_now()
        path = pathlib.Path(args.report)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    return code


if __name__ == '__main__':
    raise SystemExit(main())
