# A real connecting process whose child can retain its socket across exec.
import json
import os
import socket
import sys

peer = socket.socket(socket.AF_UNIX)
peer.connect(sys.argv[1])
mode = sys.argv[2]
if mode in ('renew', 'elevate'):
    # Server first: read the challenge, echo its nonce in hello.
    reader = peer.makefile('rb')
    challenge = reader.readline()
    print(challenge.decode().strip(), flush=True)
    nonce = json.loads(challenge)['nonce']
    peer.sendall(json.dumps({'v': 1, 'op': 'hello', 'generation': 'g', 'nonce': nonce}).encode() + b'\n')
    print(reader.readline().decode().strip(), flush=True)
assert sys.stdin.buffer.read(1) == b'e'
if mode in ('fork', 'renew', 'elevate'):
    read_fd, write_fd = os.pipe()
    if os.fork() == 0:
        os.close(write_fd)
        assert os.read(read_fd, 1) == b''  # CLOEXEC makes this an exec barrier.
        if mode == 'fork':
            peer.sendall(b'{"v":1,"op":"hello","generation":"g"}\n')
        elif mode == 'renew':
            peer.sendall(b'{"v":1,"op":"renew"}\n')
        else:
            peer.sendall(b'{"v":1,"op":"elevate","method":"POST","path":"/api/v1/attached","body_sha256":"' + b'0' * 64 + b'"}\n')
        reader = peer.makefile('rb')
        print(reader.readline().decode().strip(), flush=True)
        if mode == 'fork':
            print(reader.readline().decode().strip(), flush=True)
        os._exit(0)
    os.close(read_fd)
os.execl('/bin/sleep', 'sleep', '30')
