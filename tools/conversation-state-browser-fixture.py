#!/usr/bin/env python3
"""Use the existing isolated browser fixture with ordinary text-only replies.

Reuses skills-browser-fixture's fresh data directory, bootstrap, static Rust
server, private metadata and exact-child cleanup. Only its loopback provider is
replaced: ordinary natural commitment inputs must not select a Skill/HTML mock
because an instruction mentions drawing. This is UI evidence, not model quality.
"""
import http.server
import importlib.util
import json
import pathlib
import sys

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    'conversation_browser_fixture', pathlib.Path(__file__).with_name('skills-browser-fixture.py'))
fixture = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fixture)
original_load = fixture.load_smoke


class TextProvider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        size = int(self.headers.get('Content-Length', '0'))
        if not 0 < size <= 1_048_576:
            self.send_error(413)
            return
        payload = json.loads(self.rfile.read(size))
        with self.server.mock.lock:
            self.server.mock.requests.append((False, payload))
        text = json.dumps([{'type': 'text', 'text': '合成会话状态回复。', 'delay_ms': 0}], ensure_ascii=False)
        raw = json.dumps({'choices': [{'message': {'role': 'assistant', 'content': text}}],
                          'usage': {'prompt_tokens': 8, 'completion_tokens': 8}}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(raw)))
        self.end_headers()
        try:
            self.wfile.write(raw)
        except (BrokenPipeError, ConnectionResetError):
            pass


def load_text_fixture():
    smoke = original_load()
    smoke.Provider = TextProvider
    return smoke


if __name__ == '__main__':
    fixture.load_smoke = load_text_fixture
    fixture.main()
