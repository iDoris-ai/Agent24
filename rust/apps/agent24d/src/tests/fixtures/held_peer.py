# Synchronize a real connector, an inherited socket holder, and exec without sleeps.
# The endpoint speaks first: a client reads `challenge` and echoes its nonce in `hello`.
import json
import os
import socket
import sys


def hello(reader):
    """Print the server's first frame; return a hello echoing its nonce (or None)."""
    first = reader.readline()
    print(first.decode().strip(), flush=True)
    if json.loads(first).get('op') != 'challenge':
        return None
    nonce = json.loads(first)['nonce']
    return json.dumps({'v': 1, 'op': 'hello', 'generation': 'g', 'nonce': nonce}).encode() + b'\n'


peer = socket.socket(socket.AF_UNIX)
peer.settimeout(5)
peer.connect(sys.argv[1])
operation = sys.argv[2]
if operation == 'prequeue':
    # Queued before the daemon accepts, so no nonce can be known yet.
    peer.sendall(b'{"v":1,"op":"hello","generation":"g"}\n')
print(json.dumps({'stage': 'connected'}), flush=True)
if operation not in ('hello', 'prequeue'):
    reader = peer.makefile('rb')
    peer.sendall(hello(reader))
    print(reader.readline().decode().strip(), flush=True)
    if operation == 'elevate_wait':
        # Leave an elevation pending; the holder then writes during the wait.
        peer.sendall(b'{"v":1,"op":"elevate","method":"POST","path":"/api/v1/attached","body_sha256":"' + b'0' * 64 + b'"}\n')
        print(reader.readline().decode().strip(), flush=True)
assert sys.stdin.buffer.read(1) in (b'e', b'f')
read_fd, write_fd = os.pipe()
holder = os.fork()
if holder == 0:
    os.close(write_fd)
    assert os.read(read_fd, 1) == b''
    os.close(read_fd)
    print(json.dumps({'stage': 'holder_ready', 'pid': os.getpid()}), flush=True)
    if sys.stdin.buffer.read(1) == b'h':
        reader = peer.makefile('rb')
        frame = None
        if operation == 'hello':
            # The strongest holder: it reads the challenge and echoes the nonce.
            frame = hello(reader)
        elif operation == 'malformed':
            frame = b'not json\n'
        elif operation == 'partial':
            frame = b'{"v":1'  # never finished: the daemon times out on it
        elif operation == 'elevate_wait':
            frame = b'{"v":1,"op":"renew"}\n'
        elif operation != 'prequeue':  # a prequeue holder only listens
            frame = json.dumps({'v': 1, 'op': operation, 'generation': 'g'}).encode() + b'\n'
        if frame is not None:
            peer.sendall(frame)
        peer.settimeout(15)  # outlast the daemon's 5 s frame deadline
        while response := reader.readline():
            print(response.decode().strip(), flush=True)
            if json.loads(response).get('op') in ('enrolled', 'error'):
                break
    os._exit(0)
os.close(read_fd)
if operation in ('hello', 'prequeue'):
    # The new image stays alive and reaps its holder when the test releases it.
    os.execl(sys.executable, sys.executable, '-c', 'import os; os.wait()')
os.close(write_fd)
os.waitpid(holder, 0)
