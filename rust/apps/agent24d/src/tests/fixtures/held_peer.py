# Synchronize a real connector, an inherited socket holder, and exec without sleeps.
import json
import os
import socket
import sys

peer = socket.socket(socket.AF_UNIX)
peer.settimeout(5)
peer.connect(sys.argv[1])
operation = sys.argv[2]
print(json.dumps({'stage': 'connected'}), flush=True)
if operation != 'hello':
    peer.sendall(b'{"v":1,"op":"hello","generation":"g"}\n')
    reader = peer.makefile('rb')
    print(reader.readline().decode().strip(), flush=True)
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
        if operation == 'malformed':
            frame = b'not json\n'
        else:
            frame = json.dumps({'v': 1, 'op': operation, 'generation': 'g'}).encode() + b'\n'
        peer.sendall(frame)
        reader = peer.makefile('rb')
        while response := reader.readline():
            print(response.decode().strip(), flush=True)
            if json.loads(response).get('op') in ('enrolled', 'error'):
                break
    os._exit(0)
os.close(read_fd)
if operation == 'hello':
    # The new image stays alive and reaps its holder when the test releases it.
    os.execl(sys.executable, sys.executable, '-c', 'import os; os.wait()')
os.close(write_fd)
os.waitpid(holder, 0)
