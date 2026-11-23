#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""LumiOS 升级服务器（在宿主机上跑）。

用法：
    python tools/update-server.py            # 监听 8080
    python tools/update-server.py 9000       # 换端口

它会提供：
    GET /version       -> 当前版本号与镜像校验和（纯文本）
    GET /image.iso     -> 直接下发 out/LumiKernel-0.1.iso

虚拟机侧：需要在 VBox 上做端口转发，让 guest 的 10.0.2.2:8080 指到宿主机。
    VBoxManage controlvm <vm> natpf1 "upd,tcp,,8080,,8080"

客户端（LumiOS 终端）：输入 update-lumios
"""
import http.server
import hashlib
import os
import socketserver
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ISO = os.path.join(ROOT, 'out', 'LumiKernel-0.1.iso')
VERSION = '0.1.0'


def sha256_of(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(1 << 20), b''):
            h.update(chunk)
    return h.hexdigest()


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.0'   # 我们内核里的客户端只支持 HTTP/1.0 语义

    def do_GET(self):
        if self.path in ('/version', '/'):
            exists = os.path.exists(ISO)
            size = os.path.getsize(ISO) if exists else 0
            digest = sha256_of(ISO) if exists else '(no image)'
            body = (
                'LumiOS %s\n'
                'image: %s\n'
                'size: %d\n'
                'sha256: %s\n'
                % (VERSION, os.path.basename(ISO), size, digest)
            ).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'text/plain; charset=utf-8')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            print('[update] /version ->', VERSION)
            return

        if self.path == '/image.iso':
            if not os.path.exists(ISO):
                self.send_error(404, 'no image')
                return
            size = os.path.getsize(ISO)
            self.send_response(200)
            self.send_header('Content-Type', 'application/octet-stream')
            self.send_header('Content-Length', str(size))
            self.end_headers()
            with open(ISO, 'rb') as f:
                while True:
                    chunk = f.read(1 << 16)
                    if not chunk:
                        break
                    self.wfile.write(chunk)
            print('[update] /image.iso -> %d bytes' % size)
            return

        self.send_error(404, 'not found')

    def log_message(self, *args):
        pass


class UdpHandler(socketserver.BaseRequestHandler):
    """极简 UDP 应答：收到 `version` 就回一行版本信息。"""
    def handle(self):
        data, sock = self.request
        msg = data.decode('utf-8', 'replace').strip()
        print('[udp] <-', repr(msg))
        if msg == 'version':
            exists = os.path.exists(ISO)
            reply = 'LumiOS %s size=%d' % (VERSION, os.path.getsize(ISO) if exists else 0)
        else:
            reply = 'unknown'
        sock.sendto(reply.encode(), self.client_address)


if __name__ == '__main__':
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8080
    socketserver.TCPServer.allow_reuse_address = True
    import threading
    udp = socketserver.UDPServer(('0.0.0.0', 9000), UdpHandler)
    threading.Thread(target=udp.serve_forever, daemon=True).start()
    print('UDP server on 0.0.0.0:9000')
    with socketserver.TCPServer(('0.0.0.0', port), Handler) as httpd:
        print('LumiOS update server on 0.0.0.0:%d' % port)
        print('  image:', ISO, '(exists)' if os.path.exists(ISO) else '(MISSING)')
        httpd.serve_forever()
