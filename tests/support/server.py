#!/usr/bin/env python3
"""Dependency-free HTTP server used by integration tests."""
import socket, argparse, ssl, base64, hashlib, struct, os, os
parser = argparse.ArgumentParser(); parser.add_argument('--https', action='store_true'); parser.add_argument('--ws', action='store_true'); parser.add_argument('--wss', action='store_true'); parser.add_argument('--active-close', action='store_true')
args = parser.parse_args()
if args.ws or args.wss:
    if args.active_close: os.environ["WS_ACTIVE_CLOSE"] = "1"
    if args.wss:
        os.environ["WS_TLS"] = "1"
    #!/usr/bin/env python3
    """Dependency-free WebSocket echo test service."""
    import socket,sys,base64,hashlib,struct,os,ssl,time
    s=socket.socket(); s.bind(('127.0.0.1',0)); s.listen(1); print(s.getsockname()[1],flush=True)
    def frame(op,d,fin=True):
     b=bytes([op|(0x80 if fin else 0)]); n=len(d)
     if n<126: return b+bytes([n])+d
     if n<65536: return b+b'~'+struct.pack('!H',n)+d
     return b+b'\x7f'+struct.pack('!Q',n)+d
    def readn(n):
     out=b''
     while len(out)<n:
      x=c.recv(n-len(out))
      if not x: return out
      out+=x
     return out
    c,_=s.accept()
    if os.getenv('WS_TLS'):
     ctx=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER); ctx.load_cert_chain('tests/support/cert.pem','tests/support/key.pem'); c=ctx.wrap_socket(c,server_side=True)
    req=c.recv(4096).decode(); key=next(x.split(':',1)[1].strip() for x in req.split('\r\n') if x.lower().startswith('sec-websocket-key:'))
    acc=base64.b64encode(hashlib.sha1((key+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
    response=('HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: '+acc+'\r\n\r\n').encode()
    if os.getenv('WS_ACTIVE_CLOSE'):
     # Send the upgrade and first frame together to exercise libcurl read-ahead.
     response+=frame(8, struct.pack('!H', 1001) + b'going away')
    c.sendall(response)
    if os.getenv('WS_CLOSE_AFTER_HANDSHAKE'):
     c.close(); s.close(); sys.exit(0)
    while True:
     if os.getenv('WS_SLOW_READ'): time.sleep(0.2)
     h=c.recv(2)
     if not h: break
     n=h[1]&127
     if n==126:n=struct.unpack('!H',c.recv(2))[0]
     elif n==127:n=struct.unpack('!Q',c.recv(8))[0]
     mask=readn(4); d=bytearray(readn(n));
     for i in range(n): d[i]^=mask[i%4]
     op=h[0]&15
     if op==8:
      if os.getenv('WS_IGNORE_CLOSE'): continue
      c.sendall(bytes([0x88,len(d)])+d); break
     if op==9:
      c.sendall(bytes([0x8a,len(d)])+d); continue
     if op==10:
      continue
     if os.getenv('WS_FRAGMENT') and op in (1,2) and len(d) >= 2:
      n=max(2,int(os.getenv('WS_FRAGMENT'))); parts=[d[i:i+((len(d)+n-1)//n)] for i in range(0,len(d),((len(d)+n-1)//n))]
      for i,p in enumerate(parts):
       fin=0x80 if i==len(parts)-1 else 0; first=op if i==0 else 0
       c.sendall(frame(first,p,i==len(parts)-1))
     else: c.sendall(frame(op,d))

    raise SystemExit
if args.ws or args.wss:
    # WebSocket mode shares this single test-service entry point; the
    # protocol implementation remains dependency-free in ws-server.py.
    import runpy
    if args.wss:
        os.environ["WS_TLS"] = "1"
    runpy.run_path("tests/support/ws-server.py", run_name="__main__")
    raise SystemExit
with socket.socket() as listener:
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    print(listener.getsockname()[1], flush=True)
    conn, _ = listener.accept()
    with conn:
        if args.https or args.wss:
            ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            ctx.load_cert_chain('tests/support/cert.pem', 'tests/support/key.pem')
            conn = ctx.wrap_socket(conn, server_side=True)
        data = b""
        while b"\r\n\r\n" not in data:
            chunk = conn.recv(4096)
            if not chunk:
                break
            data += chunk
        if args.ws or args.wss:
            key = next(x.split(':',1)[1].strip() for x in data.decode().split('\r\n') if x.lower().startswith('sec-websocket-key:'))
            acc = base64.b64encode(hashlib.sha1((key+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
            conn.sendall(('HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: '+acc+'\r\n\r\n').encode())
            while True:
                h = conn.recv(2)
                if not h: break
                n = h[1] & 127
                if n == 126: n = struct.unpack('!H', conn.recv(2))[0]
                mask = conn.recv(4); payload = bytearray(conn.recv(n))
                for i in range(n): payload[i] ^= mask[i%4]
                op = h[0] & 15
                if op == 8: conn.sendall(bytes([0x88, len(payload)]) + payload); break
                if op == 9: conn.sendall(bytes([0x8a, len(payload)]) + payload); continue
                conn.sendall(bytes([0x80|op, len(payload)]) + payload)
        else:
            conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
        if args.https:
            try:
                conn = conn.unwrap()
            except (ssl.SSLError, OSError):
                pass
        try:
            conn.shutdown(socket.SHUT_RDWR if args.https else socket.SHUT_WR)
        except OSError:
            pass
