import importlib.util
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import threading
import time
import unittest

spec = importlib.util.spec_from_file_location("session_bridge", Path(__file__).resolve().parents[1] / "helpers/session_bus.py")
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)


class AuthenticationTests(unittest.TestCase):
    def test_fragmented_glib_authentication(self):
        client = bridge.AuthLines(True, 1000)
        self.assertEqual(client.feed(b"\0AUTH ANON"), b"")
        self.assertEqual(client.feed(b"YMOUS trace\r\n"), b"\0AUTH EXTERNAL 31303030\r\n")
        server = bridge.AuthLines(False, 1000)
        self.assertEqual(server.feed(b"REJECTED EXT"), b"")
        self.assertEqual(server.feed(b"ERNAL\r\n"), b"REJECTED ANONYMOUS\r\n")

    def test_binary_payload_is_never_auth_rewritten(self):
        client = bridge.AuthLines(True, 1000)
        binary = b"l\x01\0\0AUTH ANONYMOUS\r\n"
        self.assertEqual(client.feed(b"BEGIN\r\n" + binary), b"BEGIN\r\n" + binary)
        server = bridge.AuthLines(False, 1000)
        self.assertEqual(server.feed(b"OK abc\r\nREJECTED EXTERNAL\r\n"), b"OK abc\r\nREJECTED EXTERNAL\r\n")

    def test_authentication_is_bounded(self):
        with self.assertRaises(ValueError):
            bridge.AuthLines(True, 1000).feed(b"x" * (bridge.MAX_AUTH + 1))

    def test_wrong_nonce_never_connects_to_proxy(self):
        client, accepted = socket.socketpair()
        thread = threading.Thread(target=bridge.relay, args=(accepted, "/this-path-must-never-be-opened", b"a" * 16))
        thread.start()
        try:
            client.sendall(b"b" * 16)
            thread.join(timeout=2)
            self.assertFalse(thread.is_alive())
            client.settimeout(1)
            self.assertEqual(client.recv(1), b"")
        finally:
            client.close()
            thread.join(timeout=6)

    def test_guest_command_keeps_arguments_literal_and_encodes_nonce_path(self):
        original = ["/usr/bin/muvm", "-f", "/image.erofs", "--", "/usr/bin/FEXBash", "-c", 'exec "$@"', "example", "/usr/bin/app", "$(touch nope)"]
        guest = bridge.guest_command(original, 1234, "/home/user/with,comma/nonce")
        self.assertEqual(guest[-len(original[4:]):], original[4:])
        self.assertIn("/home/user/with%2Ccomma/nonce", guest)
        self.assertNotIn("/home/user/with,comma/nonce", guest)

    def test_only_specific_application_names_can_be_owned(self):
        self.assertTrue(bridge.valid_identity("io.example.Auth"))
        for identity in ["*", "org.kde.*", "--talk=*", "io..app", "io.1app"]:
            self.assertFalse(bridge.valid_identity(identity))


class RealGLibBridgeTest(unittest.TestCase):
    def test_glib_nonce_tcp_to_isolated_filtered_dbus(self):
        """A private daemon/proxy is used; no host session/keyring is contacted."""
        with tempfile.TemporaryDirectory(prefix="dnf-binfmt-private-bus-") as folder:
            folder = Path(folder)
            host_socket, proxy_socket = folder / "host", folder / "proxy"
            nonce = secrets.token_bytes(16)
            nonce_file = folder / "nonce"
            descriptor = os.open(nonce_file, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(descriptor, "wb") as file:
                file.write(nonce)
            listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            daemon = subprocess.Popen(["/usr/bin/dbus-daemon", "--session", "--nofork", f"--address=unix:path={host_socket}"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            proxy = None
            stopped = threading.Event()
            workers = []
            acceptor = None
            try:
                for _ in range(100):
                    if host_socket.exists():
                        break
                    time.sleep(0.01)
                self.assertTrue(host_socket.exists())
                proxy = subprocess.Popen(["/usr/bin/xdg-dbus-proxy", f"unix:path={host_socket}", str(proxy_socket), "--filter", "--talk=org.freedesktop.secrets"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                for _ in range(100):
                    if proxy_socket.exists():
                        break
                    time.sleep(0.01)
                self.assertTrue(proxy_socket.exists())
                listener.bind(("127.0.0.1", 0))
                listener.listen(4)
                listener.settimeout(0.1)

                def accept():
                    while not stopped.is_set():
                        try:
                            client, _ = listener.accept()
                        except socket.timeout:
                            continue
                        except OSError:
                            return
                        thread = threading.Thread(target=bridge.relay, args=(client, str(proxy_socket), nonce, stopped), daemon=True)
                        workers.append(thread)
                        thread.start()

                acceptor = threading.Thread(target=accept)
                acceptor.start()
                address = f"nonce-tcp:host=127.0.0.1,port={listener.getsockname()[1]},noncefile={nonce_file}"
                result = subprocess.run(["/usr/bin/gdbus", "call", "--address", address, "--dest", "org.freedesktop.DBus", "--object-path", "/org/freedesktop/DBus", "--method", "org.freedesktop.DBus.ListNames"], capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("org.freedesktop.DBus", result.stdout)
                denied = subprocess.run(["/usr/bin/gdbus", "call", "--address", address, "--dest", "org.example.Private", "--object-path", "/org/example/Private", "--method", "org.example.Private.Ping"], capture_output=True, text=True, timeout=10)
                self.assertNotEqual(denied.returncode, 0)
                self.assertNotIn("org.example.Private", result.stdout)
            finally:
                stopped.set()
                listener.close()
                if acceptor:
                    acceptor.join(timeout=1)
                for process in (proxy, daemon):
                    if process:
                        process.terminate()
                        process.wait(timeout=3)
                for worker in workers:
                    worker.join(timeout=1)


if __name__ == "__main__":
    unittest.main()
