#!/usr/bin/env python3
"""Run a command in a fresh network namespace with `lo` up (no `ip` needed).
usage: unshare -n python3 nsrun.py CMD ARGS...   (see soak.py / README.md)"""
import fcntl, os, socket, struct, sys
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
fl = struct.unpack('16sH14s', fcntl.ioctl(s, 0x8913, struct.pack('16sH14s', b'lo', 0, b'')))[1]
fcntl.ioctl(s, 0x8914, struct.pack('16sH14s', b'lo', fl | 1, b''))
os.execvp(sys.argv[1], sys.argv[1:])
