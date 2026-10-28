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

def fork_worker():
    """A TERM-ignoring worker in the leader's group, recorded by PID."""
    read_fd, write_fd = os.pipe()
    worker = os.fork()
    if worker == 0:
        os.close(read_fd)
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        # Keep inherited listeners (as a real runtime worker would), but not
        # the log pipes, so a surviving worker cannot stall log draining.
        null = os.open(os.devnull, os.O_RDWR)
        for fd in (0, 1, 2):
            os.dup2(null, fd)
        os.write(write_fd, b'1')
        os.close(write_fd)
        while True:
            time.sleep(1)
    os.close(write_fd)
    os.read(read_fd, 1)
    os.close(read_fd)
    with (state / 'workers.jsonl').open('a') as output:
        output.write(json.dumps({'leader': os.getpid(), 'pid': worker}) + '\n')

if mode == 'worker_then_crash_before_ready':
    fork_worker()
    os.kill(os.getpid(), signal.SIGKILL)

health = {
    'status': 'ok', 'service': 'idoris', 'version': '0.4.0',
    'contract_version': scenario.get('contract', '1.3.0'),
    'instance_id': 'fixture-instance', 'dev_no_key_enabled': False,
}
health.update(scenario.get('health', {}))
for field in scenario.get('missing', []):
    health.pop(field, None)

# Set once the supervisor's authorized Admin probe got its 200, i.e. readiness.
ready = threading.Event()

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
        if self.server.is_admin and authorized and status == 200:
            ready.set()

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        assert self.path == '/v1/chat/completions'
        assert body['model'] == 'idoris/daily'
        assert body['max_tokens'] == 1
        if scenario.get('verify_wait'):
            time.sleep(3)
        payload = json.dumps({'model': 'real-model', 'choices': [{'message': {'role': 'assistant', 'content': 'Hi'}, 'finish_reason': 'stop'}], 'usage': {'prompt_tokens': 1, 'completion_tokens': 1}}).encode()
        self.send_response(scenario.get('verify_status', 200))
        self.send_header('Content-Length', str(len(payload)))
        self.send_header('X-iDoris-Served-Locality', 'loopback')
        self.send_header('X-iDoris-Record-Id', 'fixture-chat')
        self.end_headers()
        try:
            self.wfile.write(payload)
        except BrokenPipeError:
            pass
    def log_message(self, *_):
        pass

class Server(http.server.HTTPServer):
    allow_reuse_address = True

# Bind the configured ports directly, as the real serve does: the supervisor
# must find this process group holding both listeners before readiness.
server = Server(('127.0.0.1', int(os.environ['IDORIS_PORT'])), Handler)
server.is_admin = False
admin = Server(('127.0.0.1', int(os.environ['IDORIS_ADMIN_PORT'])), Handler)
admin.is_admin = True
threading.Thread(target=admin.serve_forever, daemon=True).start()

def exit_when_requested(action):
    # Timed exits count from readiness, not process start, so a slower
    # readiness check cannot turn an after-ready crash into a startup one.
    ready.wait()
    if scenario.get('wait_for_exit'):
        while not (state / 'release-exit').exists():
            time.sleep(0.005)
    time.sleep(scenario.get('after', 0.15))
    action()

def late_worker_then_crash():
    # Consume the release so the next generation waits for a new one.
    (state / 'release-exit').unlink()
    fork_worker()
    os.kill(os.getpid(), signal.SIGKILL)

if mode == 'late_worker':
    threading.Thread(target=exit_when_requested, args=(late_worker_then_crash,), daemon=True).start()
if mode == 'crash':
    threading.Thread(target=exit_when_requested, args=(lambda: os.kill(os.getpid(), signal.SIGKILL),), daemon=True).start()
if mode == 'exit_after_ready':
    threading.Thread(target=exit_when_requested, args=(lambda: os._exit(scenario['code']),), daemon=True).start()
server.serve_forever()
