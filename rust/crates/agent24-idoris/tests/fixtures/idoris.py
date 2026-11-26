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
import uuid
from pathlib import Path

root = Path(os.environ['IDORIS_COMPONENTS_DIR'])
scenario = json.loads((root / 'scenario.json').read_text()) if (root / 'scenario.json').exists() else {}
mode = scenario.get('mode', 'healthy')

def terminate_during_startup(*_):
    sys.exit(0)

signal.signal(signal.SIGTERM, terminate_during_startup)
if mode == 'no_read':
    time.sleep(60)
    sys.exit(1)
if mode == 'exit':
    sys.exit(scenario['code'])
assert sys.argv[1:] == ['serve', '--launcher-stdin']
frame = sys.stdin.buffer.read()
launcher = json.loads(frame)
secret = launcher['admin_token']
discovery_path = Path(launcher['discovery_path'])
assert launcher['v'] == 1 and frame.endswith(b'\n') and frame.count(b'\n') == 1
assert discovery_path.is_absolute()
assert launcher['orphan_timeout_s'] == (900 if 'register_socket' in launcher else 0)
key = os.environ.get('IDORIS_OMLX_API_KEY', '')
report = {
    'pid': os.getpid(), 'group': os.getpgrp(), 'session': os.getsid(0),
    'argv': sys.argv[1:], 'length': len(frame),
    'valid_frame': re.fullmatch('[0-9a-f]{64}', secret) is not None,
    'token_hash': hashlib.sha256(secret.encode()).hexdigest(),
    'token_in_env': secret in os.environ.values(),
    'env_names': sorted(os.environ),
    'budget': os.environ['IDORIS_ALLOCATED_BUDGET_BYTES'],
    'key_hash': hashlib.sha256(key.encode()).hexdigest(),
    'db': os.environ['IDORIS_DB_PATH'],
}
state = Path(os.environ['IDORIS_STATE_DIR'])
with (state / 'attempts.jsonl').open('a') as output:
    output.write(json.dumps(report) + '\n')
# The supervisor must discard child output, even when it contains a secret.
secret_bytes = secret.encode()
os.write(1, b'admin=' + secret_bytes[:23])
os.write(1, secret_bytes[23:] + b' key=' + key.encode() + b' idk_fixture-data-key\n')
os.write(2, b'error idk_stderr-key ' + secret_bytes + b'\n')
if mode == 'flood':
    os.write(1, b'x' * 20000 + secret_bytes + b'\n')
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

instance_id = str(uuid.uuid4())
health = {
    'status': 'ok', 'service': 'idoris', 'version': '0.6.1',
    'phase': 'ready', 'ready': True,
    'upstreams': [{'id': 'fixture-upstream', 'engine_kind': 'omlx', 'state': 'ready',
                   'managed': False, 'owned': False}],
    'contract_version': scenario.get('contract', '1.3.0'),
    'instance_id': instance_id, 'dev_no_key_enabled': False,
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
            authorized = hmac.compare_digest(self.headers.get('Authorization', ''), 'Bearer ' + secret)
            status = scenario.get('admin_status', 200) if authorized else 401
            payload = b'{}'
        else:
            assert self.path == '/health'
            if (state / 'hang-health').exists():
                time.sleep(60)
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
        payload = json.dumps({'model': scenario.get('verify_model', 'real-model'), 'choices': [{'message': {'role': 'assistant', 'content': 'Hi'}, 'finish_reason': 'stop'}], 'usage': {'prompt_tokens': 1, 'completion_tokens': 1}}).encode()
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

# The launcher uses ephemeral ports and publishes discovery before readiness.
assert os.environ['IDORIS_PORT'] == os.environ['IDORIS_ADMIN_PORT'] == '0'
server = Server(('127.0.0.1', 0), Handler)
server.is_admin = False
admin = Server(('127.0.0.1', 0), Handler)
admin.is_admin = True
record = {
    'schema': 1, 'service': 'idoris', 'pid': os.getpid(),
    'instance_nonce': os.urandom(32).hex(), 'instance_id': instance_id,
    'data_port': server.server_port,
    'admin_port': scenario.get('discovery_admin_port', admin.server_port),
    'started_at_ms': int(time.time() * 1000), 'version': '0.6.1',
    'contract_version': '1.3.0',
    'launch': {'db_path': os.environ['IDORIS_DB_PATH'], 'state_dir': str(state),
               'components_dir': str(root), 'allocated_budget_bytes': int(report['budget'])},
}
temporary = discovery_path.with_name(f'.discovery-{os.getpid()}.json')
with os.fdopen(os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as out:
    json.dump(record, out)
temporary.replace(discovery_path)

def terminate(*_):
    discovery_path.unlink(missing_ok=True)
    sys.exit(0)

if mode != 'ignore_term':
    signal.signal(signal.SIGTERM, terminate)
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
