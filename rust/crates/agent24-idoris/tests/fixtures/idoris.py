#!/usr/bin/python3
"""Isolated launcher contract fixture. Never persists raw credentials."""
import hashlib
import http.server
import json
import os
import re
import signal
import hmac
import sys
import threading
import time
from pathlib import Path

root = Path(os.environ['IDORIS_COMPONENTS_DIR'])
scenario = json.loads((root / 'scenario.json').read_text())
mode = scenario.get('mode', 'healthy')
if mode == 'no_read':
    time.sleep(60)
    sys.exit(1)
if mode == 'exit':
    sys.exit(scenario['code'])
frame = sys.stdin.buffer.read()
key = os.environ.get('IDORIS_OMLX_API_KEY', '')
report = {
    'pid': os.getpid(), 'group': os.getpgrp(),
    'argv': sys.argv[1:], 'length': len(frame),
    'valid_frame': re.fullmatch(b'[0-9a-f]{64}\n', frame) is not None,
    'token_hash': hashlib.sha256(frame).hexdigest(),
    'token_in_env': frame[:-1].decode() in os.environ.values(),
    'env_names': sorted(os.environ),
    'budget': os.environ['IDORIS_ALLOCATED_BUDGET_BYTES'],
    'key_hash': hashlib.sha256(key.encode()).hexdigest(),
    'db': os.environ['IDORIS_DB_PATH'],
}
state = Path(os.environ['IDORIS_STATE_DIR'])
with (state / 'attempts.jsonl').open('a') as output:
    output.write(json.dumps(report) + '\n')
# Deliberately split a known secret across pipe writes to catch chunk redaction.
secret = frame[:-1]
os.write(1, b'admin=' + secret[:23])
os.write(1, secret[23:] + b' key=' + key.encode() + b' idk_fixture-data-key\n')
os.write(2, b'error idk_stderr-key ' + secret + b'\n')
if mode == 'flood':
    os.write(1, b'x' * 20000 + secret + b'\n')
if mode == 'ignore_term':
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
if mode == 'crash_before_ready':
    os.kill(os.getpid(), signal.SIGKILL)
if mode == 'stubborn_descendant':
    read_fd, write_fd = os.pipe()
    descendant = os.fork()
    if descendant == 0:
        os.close(read_fd)
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        os.write(write_fd, b'1')
        os.close(write_fd)
        while True:
            time.sleep(1)
    os.close(write_fd)
    os.read(read_fd, 1)
    os.close(read_fd)
    (state / 'descendant.json').write_text(json.dumps({'pid': descendant}))

health = {
    'status': 'ok', 'service': 'idoris', 'version': '0.4.0',
    'contract_version': scenario.get('contract', '1.3.0'),
    'instance_id': 'fixture-instance', 'dev_no_key_enabled': False,
}
health.update(scenario.get('health', {}))
for field in scenario.get('missing', []):
    health.pop(field, None)

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.server.is_admin:
            assert self.path == '/admin/api/v1/status'
            authorized = hmac.compare_digest(self.headers.get('Authorization', ''), 'Bearer ' + secret.decode())
            status = scenario.get('admin_status', 200) if authorized else 401
            payload = b'{}'
        else:
            assert self.path == '/health'
            status = scenario.get('http_status', 200)
            payload = json.dumps(health).encode()
        self.send_response(status)
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)
    def log_message(self, *_):
        pass

class Server(http.server.HTTPServer):
    allow_reuse_address = True

# Bind ephemeral ports directly. Rust keeps stable proxy listeners reserved
# throughout each fixture's lifetime, forwarding only to these reported ports.
server = Server(('127.0.0.1', 0), Handler)
server.is_admin = False
admin = Server(('127.0.0.1', 0), Handler)
admin.is_admin = True
ports = state / 'ports.tmp'
ports.write_text(json.dumps({'data': server.server_port, 'admin': admin.server_port}))
ports.replace(state / f'ports-{os.getpid()}.json')
threading.Thread(target=admin.serve_forever, daemon=True).start()

def exit_when_requested(action):
    if scenario.get('wait_for_exit'):
        while not (state / 'release-exit').exists():
            time.sleep(0.005)
    time.sleep(scenario.get('after', 0.15))
    action()

if mode == 'crash':
    threading.Thread(target=exit_when_requested, args=(lambda: os.kill(os.getpid(), signal.SIGKILL),), daemon=True).start()
if mode == 'exit_after_ready':
    threading.Thread(target=exit_when_requested, args=(lambda: os._exit(scenario['code']),), daemon=True).start()
server.serve_forever()
