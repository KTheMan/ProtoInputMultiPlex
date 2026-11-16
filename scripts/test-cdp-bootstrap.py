"""Inert fake-socket/clock regressions. Hosted execution only; no GUI or network."""
import base64
import hashlib
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("pim_cdp_bootstrap_test_base",
                                             Path(__file__).with_name("qualify-gui-pair.py"))
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
KEY = base64.b64encode(bytes(16)).decode()
ACCEPT = base64.b64encode(hashlib.sha1(
    (KEY + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest())
HEADERS = b"HTTP/1.1 101 Switching Protocols\r\nSec-WebSocket-Accept: " + ACCEPT + b"\r\n\r\n"


class Clock:
    def __init__(self):
        self.now = 0.0

    def monotonic(self):
        return self.now

    def advance(self, seconds):
        self.now += seconds


class FakeSocket:
    def __init__(self, clock, chunks, send_delay=0):
        self.clock = clock
        self.chunks = list(chunks)
        self.send_delay = send_delay
        self.timeouts = []
        self.timeout = None
        self.close_count = 0
        self.recv_count = 0
        self.sent = []

    def settimeout(self, timeout):
        if timeout <= 0:
            raise AssertionError("nonpositive timeout")
        self.timeout = timeout
        self.timeouts.append(timeout)

    def delay(self, seconds):
        if seconds > self.timeout:
            self.clock.advance(self.timeout)
            raise TimeoutError()
        self.clock.advance(seconds)

    def sendall(self, value):
        self.sent.append(value)
        self.delay(self.send_delay)

    def recv(self, size):
        self.recv_count += 1
        if not self.chunks:
            raise AssertionError("unexpected extra recv/retry")
        delay, value = self.chunks.pop(0)
        self.delay(delay)
        if len(value) > size:
            raise AssertionError("fake chunk exceeds recv bound")
        return value

    def close(self):
        self.close_count += 1


class Harness:
    def __init__(self, chunks, connect_delay=0, send_delay=0):
        self.clock = Clock()
        self.sock = FakeSocket(self.clock, chunks, send_delay)
        self.connect_delay = connect_delay
        self.attempts = 0
        self.connect_timeouts = []

    def connect(self, address, timeout):
        self.attempts += 1
        self.connect_timeouts.append(timeout)
        if address != ("127.0.0.1", 12345):
            raise AssertionError("unexpected address")
        if self.connect_delay > timeout:
            self.clock.advance(timeout)
            raise TimeoutError()
        self.clock.advance(self.connect_delay)
        return self.sock

    def open(self, deadline=None):
        with patch.object(base.time, "monotonic", self.clock.monotonic), \
                patch.object(base.socket, "create_connection", self.connect), \
                patch.object(base.os, "urandom", lambda size: bytes(size)):
            return base.CDP("ws://127.0.0.1:12345/devtools/browser/fixed", deadline=deadline)


class BootstrapTests(unittest.TestCase):
    def failure(self, harness, expected_type, phase, deadline=None, owned=True):
        with self.assertRaises(expected_type) as caught:
            harness.open(deadline)
        self.assertEqual(base.cdp_bootstrap_failure(caught.exception), phase)
        self.assertEqual(harness.sock.close_count, 1 if owned else 0)
        self.assertLessEqual(harness.attempts, 1)
        return caught.exception

    def test_partial_valid_handshake_after_two_seconds_preserves_frame_tail(self):
        harness = Harness([(2.2, HEADERS[:20]), (1.5, HEADERS[20:] + b"\x81\x00")],
                          connect_delay=.2, send_delay=.1)
        client = harness.open()
        self.assertAlmostEqual(harness.clock.now, 4.0)
        self.assertEqual(harness.attempts, 1)
        self.assertEqual(harness.sock.close_count, 0)
        self.assertEqual(client.buffer, b"\x81\x00")
        self.assertEqual(client.ident, 0)
        self.assertEqual(harness.sock.timeouts[-1], 2)
        self.assertTrue(all(0 < value <= 5 for value in harness.sock.timeouts))
        self.assertEqual(len(harness.sock.sent), 1)
        client.sock.close()

    def test_partial_header_then_eof_fails_without_busy_loop(self):
        harness = Harness([(.1, HEADERS[:10]), (0, b"")])
        self.failure(harness, ConnectionError, "upgrade_read")
        self.assertEqual(harness.sock.recv_count, 2)
        self.assertAlmostEqual(harness.clock.now, .1)

    def test_tcp_timeout_has_phase_and_no_owned_socket(self):
        harness = Harness([], connect_delay=6)
        self.failure(harness, TimeoutError, "tcp_connect", owned=False)
        self.assertEqual(harness.attempts, 1)
        self.assertAlmostEqual(harness.clock.now, 5)

    def test_upgrade_send_timeout_closes_owned_socket(self):
        harness = Harness([], connect_delay=.1, send_delay=6)
        self.failure(harness, TimeoutError, "upgrade_send")
        self.assertAlmostEqual(harness.clock.now, 5)
        self.assertEqual(harness.sock.recv_count, 0)

    def test_header_drip_cannot_restart_absolute_deadline(self):
        harness = Harness([(2, b"H"), (2, b"T"), (2, b"T")], connect_delay=.1)
        self.failure(harness, TimeoutError, "upgrade_read")
        self.assertAlmostEqual(harness.clock.now, 5)
        self.assertEqual(harness.sock.recv_count, 3)

    def test_remaining_fixture_deadline_is_stronger_than_bootstrap_budget(self):
        harness = Harness([(2, HEADERS)], connect_delay=.1)
        self.failure(harness, TimeoutError, "upgrade_read", deadline=1.5)
        self.assertAlmostEqual(harness.clock.now, 1.5)
        self.assertEqual(harness.connect_timeouts, [1.5])

    def test_expired_fixture_does_not_attempt_connection(self):
        harness = Harness([])
        self.failure(harness, TimeoutError, "tcp_connect", deadline=0, owned=False)
        self.assertEqual(harness.attempts, 0)

    def test_complete_header_at_deadline_cannot_escape_parse_fence(self):
        harness = Harness([(5, HEADERS)])
        self.failure(harness, TimeoutError, "upgrade_parse")
        self.assertAlmostEqual(harness.clock.now, 5)

    def test_invalid_upgrade_or_accept_closes_owned_socket(self):
        for headers in [HEADERS.replace(b"101", b"200", 1),
                        HEADERS.replace(ACCEPT, b"invalid")]:
            with self.subTest(headers_valid=False):
                harness = Harness([(0, headers)])
                self.failure(harness, base.Rejected, "upgrade_parse")

    def test_header_size_bound_is_unchanged(self):
        harness = Harness([(0, b"X" * 4096)] * 16)
        self.failure(harness, base.Rejected, "upgrade_read")
        self.assertEqual(harness.sock.recv_count, 16)

    def test_public_diagnostic_never_projects_exception_text_or_unknown_value(self):
        error = RuntimeError("private endpoint and arbitrary peer text")
        for value in [None, "private endpoint", {"private": "value"}]:
            error.pim_cdp_bootstrap_stage = value
            self.assertEqual(base.cdp_bootstrap_failure(error), "none")


if __name__ == "__main__":
    unittest.main()
