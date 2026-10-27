"""Test-only, no-redirect TLS relay. Reads profile/key, never production chats."""
import hashlib
import http.client
import ipaddress
import json
import pathlib
import re
import socket
import sqlite3
import ssl
import threading
import urllib.parse


class Relay:
    def __init__(self, source, limit=12):
        root = pathlib.Path(source).resolve(strict=True)
        with sqlite3.connect((root / 'dsh.db').as_uri() + '?mode=ro', uri=True) as db:
            raw = db.execute("SELECT c.config_json FROM config_revisions c JOIN settings s ON s.key='active_revision' AND c.revision=CAST(s.value AS INTEGER)").fetchone()[0]
        model = json.loads(raw)['model']
        self.endpoint = urllib.parse.urlsplit(model['base_url'])
        if self.endpoint.scheme != 'https' or not self.endpoint.hostname or self.endpoint.port not in (None, 443) or self.endpoint.username or self.endpoint.query or self.endpoint.fragment:
            raise ValueError('public_https_profile_required')
        addresses = socket.getaddrinfo(self.endpoint.hostname, 443, type=socket.SOCK_STREAM)
        if not addresses or any(not ipaddress.ip_address(row[4][0]).is_global for row in addresses):
            raise ValueError('public_provider_required')
        self.address = addresses[0][4][0]
        reference = model['credential_ref']
        if not re.fullmatch(r'[0-9a-fA-F-]{36}', reference):
            raise ValueError('invalid_credential_reference')
        key_file = (root / 'secrets' / reference).resolve(strict=True)
        if not key_file.is_relative_to(root / 'secrets'):
            raise ValueError('invalid_credential_path')
        self.key, self.model = key_file.read_text(), model['model']
        self.limit, self.records, self.peak_active = limit, [], 0
        self.lock = threading.Lock()
        self.gate = threading.Semaphore(1)

    def forward(self, payload):
        if len(json.dumps(payload).encode()) > 4 * 1024 * 1024:
            raise ValueError('request_size_limit')
        with self.gate, self.lock:
            if len(self.records) >= self.limit:
                raise ValueError('real_request_budget_exhausted')
            system = payload['messages'][0].get('content', '')
            kind = 'vision' if str(system).startswith('DSH_CONVERSATION_IMAGE_DESCRIPTION_V1') else 'plan_check' if str(system).startswith('DSH_SELF_PLAN_COMMIT_V2') else 'native_or_notice'
            outgoing = dict(payload)
            outgoing['model'] = 'deepseek-flash' if kind == 'vision' else self.model
            raw = json.dumps(outgoing, ensure_ascii=False).encode()
            record = {'kind': kind, 'request_sha256': hashlib.sha256(raw).hexdigest(), 'status': 'started'}
            self.records.append(record)
            self.peak_active = 1
            host, address = self.endpoint.hostname, self.address

            class Pinned(http.client.HTTPSConnection):
                def connect(connection):
                    sock = socket.create_connection((address, 443), 18)
                    connection.transport_socket = sock
                    connection.sock = ssl.create_default_context().wrap_socket(sock, server_hostname=host)
                    connection.transport_socket = connection.sock

            connection = Pinned(host, timeout=18)

            def interrupt():
                active = getattr(connection, 'transport_socket', None) or connection.sock
                if active:
                    try:
                        active.shutdown(socket.SHUT_RDWR)
                    except OSError:
                        pass
                connection.close()

            timer = threading.Timer(18, interrupt)
            timer.daemon = True
            timer.start()
            try:
                path = self.endpoint.path.rstrip('/')
                if not path.endswith('/chat/completions'):
                    path += '/chat/completions'
                connection.request('POST', path, raw, {'Content-Type': 'application/json', 'Authorization': 'Bearer ' + self.key})
                response = connection.getresponse()
                body = response.read(131073)
                if len(body) > 131072:
                    raise ValueError('response_size_limit')
                record.update(status='finished', http_status=response.status, response_sha256=hashlib.sha256(body).hexdigest())
                if response.status == 200:
                    completion=json.loads(body).get('choices',[{}])[0]
                    record['finish_reason']=completion.get('finish_reason')
                    record['synthetic_content']=str(completion.get('message',{}).get('content',''))[:16000]
                return response.status, body
            except Exception as error:
                record.update(status='failed', error_type=type(error).__name__)
                raise RuntimeError('provider_request_failed') from None
            finally:
                timer.cancel()
                connection.close()
