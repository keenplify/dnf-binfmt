#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Generic filtered host-session bridge for muvm/FEX applications.

Only loopback TCP is exposed; each launch has a fresh 16-byte private nonce.
The nonce authenticates transport; GLib ANONYMOUS is translated to the relay's
EXTERNAL UID on the filtered UNIX proxy. No application names are hardcoded.
"""
import hmac
import os
from pathlib import Path
import select
import secrets
import socket
import stat
import subprocess
import sys
import tempfile
import threading
import time
from urllib.parse import quote

TALK = (
    "org.freedesktop.secrets",
    "org.kde.StatusNotifierWatcher",
    "org.freedesktop.Notifications",
    "org.freedesktop.portal.Desktop",
)
OWN = ("org.kde.StatusNotifierItem-*",)
MAX_AUTH = 32768


class AuthLines:
    """Incremental authentication framing; binary bytes are never rewritten."""
    def __init__(self, client, uid):
        self.client = client
        self.identity = str(uid).encode().hex().encode()
        self.pending = b""
        self.active = True

    def feed(self, data):
        if not self.active:
            return data
        self.pending += data
        output = bytearray()
        while b"\r\n" in self.pending:
            line, self.pending = self.pending.split(b"\r\n", 1)
            if len(line) > MAX_AUTH:
                raise ValueError("D-Bus authentication line too long")
            prefix = b"\0" if line.startswith(b"\0") else b""
            command = line[len(prefix):]
            if self.client and (command == b"AUTH ANONYMOUS" or command.startswith(b"AUTH ANONYMOUS ")):
                line = prefix + b"AUTH EXTERNAL " + self.identity
            elif not self.client and command.startswith(b"REJECTED ") and b"EXTERNAL" in command.split():
                line = b"REJECTED ANONYMOUS"
            output.extend(line + b"\r\n")
            # Upstream enters raw forwarding after OK. Client enters it at BEGIN.
            if (self.client and command == b"BEGIN") or (not self.client and command.startswith(b"OK ")):
                self.active = False
                output.extend(self.pending)
                self.pending = b""
                break
        if len(self.pending) > MAX_AUTH:
            raise ValueError("D-Bus authentication line too long")
        return bytes(output)


def relay(client, path, nonce, stopped=None):
    upstream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    stopped = stopped or threading.Event()
    try:
        client.settimeout(5)
        received = b""
        while len(received) < len(nonce):
            chunk = client.recv(len(nonce) - len(received))
            if not chunk:
                return
            received += chunk
        if not hmac.compare_digest(received, nonce):
            return
        client.settimeout(None)
        upstream.settimeout(5)
        upstream.connect(path)
        upstream.settimeout(None)
        client_parser = AuthLines(True, os.getuid())
        server_parser = AuthLines(False, os.getuid())
        deadline = time.monotonic() + 10
        while not stopped.is_set():
            if client_parser.active and time.monotonic() > deadline:
                return
            readable, _, _ = select.select([client, upstream], [], [], 0.25)
            for source in readable:
                data = source.recv(65536)
                if not data:
                    return
                parser = client_parser if source is client else server_parser
                data = parser.feed(data)
                if data:
                    destination = upstream if source is client else client
                    destination.settimeout(5)
                    destination.sendall(data)
                    destination.settimeout(None)
    except (OSError, ValueError):
        # Authentication failures never log credentials or application traffic.
        pass
    finally:
        client.close()
        upstream.close()


def guest_command(command, port, nonce_file):
    if not command or command[0] != "/usr/bin/muvm" or "--" not in command:
        raise ValueError("expected an explicit muvm command")
    boundary = command.index("--")
    script = '''set -eu
port=$1
nonce_file=$2
shift 2
gateway=$(/usr/sbin/ip -4 route show default | /usr/bin/awk 'NR==1 {print $3}')
[ -n "$gateway" ] || { echo 'No muvm default gateway found' >&2; exit 1; }
export DBUS_SESSION_BUS_ADDRESS="nonce-tcp:host=$gateway,port=$port,noncefile=$nonce_file"
exec "$@"
'''
    return command[:boundary + 1] + ["/usr/bin/bash", "-c", script, "dnf-binfmt", str(port), quote(str(nonce_file), safe="/")] + command[boundary + 1:]


def main(args):
    identities = []
    while args[:1] == ["--app-id"]:
        if len(args) < 2 or not valid_identity(args[1]):
            raise ValueError("invalid application bus identity")
        identities.append(args[1])
        args = args[2:]
    if args[:1] == ["--"]:
        args = args[1:]
    if os.geteuid() == 0:
        raise RuntimeError("run applications as your desktop user")
    runtime = Path(os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}"))
    bus = runtime / "bus"
    address = os.environ.get("DBUS_SESSION_BUS_ADDRESS") or f"unix:path={quote(str(bus), safe='/')}"
    # Keep the nonce on the shared host filesystem, readable by the guest user.
    cache = Path(os.environ.get("XDG_CACHE_HOME", str(Path.home() / ".cache"))) / "dnf-binfmt/bridges"
    if not cache.is_absolute():
        raise ValueError("XDG_CACHE_HOME must be absolute")
    cache.mkdir(parents=True, exist_ok=True, mode=0o700)
    with tempfile.TemporaryDirectory(prefix="bus-", dir=cache) as folder:
        folder = Path(folder)
        folder.chmod(0o700)
        nonce = secrets.token_bytes(16)
        nonce_file = folder / "nonce"
        descriptor = os.open(nonce_file, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        with os.fdopen(descriptor, "wb") as file:
            file.write(nonce)
        proxy_socket = folder / "bus"
        proxy_args = ["/usr/bin/xdg-dbus-proxy", address, str(proxy_socket), "--filter"]
        proxy_args += [f"--talk={name}" for name in TALK]
        proxy_args += [f"--own={name}" for name in OWN]
        proxy_args += [f"--own={name}" for name in identities]
        proxy = subprocess.Popen(proxy_args)
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        stopped = threading.Event()
        workers = set()
        workers_lock = threading.Lock()
        acceptor = None
        app = None
        try:
            for _ in range(100):
                try:
                    if stat.S_ISSOCK(proxy_socket.stat().st_mode):
                        break
                except FileNotFoundError:
                    pass
                if proxy.poll() is not None:
                    raise RuntimeError("host session-bus proxy failed to start; use --session-bus off for headless tools")
                time.sleep(0.02)
            else:
                raise RuntimeError("timed out waiting for the filtered session-bus proxy")
            listener.bind(("127.0.0.1", 0))
            listener.listen(8)
            listener.settimeout(0.25)
            port = listener.getsockname()[1]

            def worker(client):
                try:
                    relay(client, str(proxy_socket), nonce, stopped)
                finally:
                    with workers_lock:
                        workers.discard(threading.current_thread())

            def accept():
                while not stopped.is_set():
                    try:
                        client, _ = listener.accept()
                    except socket.timeout:
                        continue
                    except OSError:
                        return
                    with workers_lock:
                        if len(workers) >= 64:
                            client.close()
                            continue
                        thread = threading.Thread(target=worker, args=(client,), daemon=True)
                        workers.add(thread)
                    thread.start()

            acceptor = threading.Thread(target=accept, daemon=True)
            acceptor.start()
            app = subprocess.Popen(guest_command(args, port, nonce_file))
            while app.poll() is None:
                if proxy.poll() is not None:
                    raise RuntimeError("filtered session-bus proxy exited while the application was running")
                time.sleep(0.1)
            return app.returncode
        finally:
            stopped.set()
            listener.close()
            if acceptor:
                acceptor.join(timeout=1)
            for process in (app, proxy):
                if process and process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
            with workers_lock:
                pending = list(workers)
            for thread in pending:
                thread.join(timeout=0.3)


def valid_identity(value):
    parts = value.split(".")
    return len(parts) >= 2 and all(part and not part[0].isdigit() and all(ch.isascii() and (ch.isalnum() or ch in "_-") for ch in part) for part in parts)


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except (OSError, RuntimeError, ValueError) as error:
        print(f"dnf-binfmt session bridge: {error}", file=sys.stderr)
        sys.exit(1)
