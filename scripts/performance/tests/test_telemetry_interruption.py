"""Production transport/lock interruption boundaries; no external network or host state."""
import errno
import fcntl
import os
import signal
import threading
import time
from unittest import mock

from perflab import budget, telemetrylab, transport
from perflab.values import ContractError
from tests.support import TempDir


class Interruption(TempDir):
    def test_active_response_is_cancelled_and_all_published_resources_are_closed(self):
        reading, shutdown = threading.Event(), threading.Event()
        actions, sockets, responses, connections, exchanges = [], [], [], [], []
        original_run = transport._Exchange.run

        class Socket:
            def __init__(self, *args):
                self.closed = False
                sockets.append(self)

            def settimeout(self, timeout):
                pass

            def connect(self, address):
                actions.append("connect")

            def do_handshake(self):
                actions.append("TLS")

            def shutdown(self, how):
                shutdown.set()

            def close(self):
                self.closed = True

        class Context:
            def wrap_socket(self, raw, **kwargs):
                assert not kwargs["do_handshake_on_connect"]
                raw.close()  # descriptor transferred, as in the real ssl wrapper
                return Socket()

        class Response:
            status = 200

            def __init__(self):
                self.closed = False
                responses.append(self)

            def read(self, size):
                actions.append("read")
                reading.set()
                os.kill(os.getpid(), signal.SIGALRM)
                if not shutdown.wait(3):
                    raise AssertionError("active response was never shut down")
                return b"x"  # even buffered data cannot authorize another read after abort

            def close(self):
                self.closed = True

        class Connection:
            def __init__(self, *args, **kwargs):
                self.closed = False
                connections.append(self)

            def request(self, *args, **kwargs):
                actions.append("GET")

            def getresponse(self):
                return Response()

            def close(self):
                self.closed = True

        def running(exchange):
            exchanges.append(exchange)
            original_run(exchange)

        started = time.monotonic()
        with mock.patch.object(transport.socket, "socket", Socket), \
                mock.patch.object(transport.socket, "getaddrinfo", return_value=[
                    (2, 1, 6, "", ("127.0.0.1", 443))]), \
                mock.patch.object(transport.http.client, "HTTPSConnection", Connection), \
                mock.patch.object(transport._Exchange, "run", running):
            with self.assertRaises(budget.Interrupted):
                with budget.Budget(started + 2, time.monotonic, 0).enforce("response"):
                    transport.HttpsTransport(context=Context()).request(
                        "https://api.example.invalid/", {"Authorization": "synthetic"}, 100, 10)
        self.assertTrue(reading.is_set())
        self.assertLess(time.monotonic() - started, 1.5)
        self.assertTrue(exchanges[0].aborted and exchanges[0].done.is_set())
        self.assertIsNone(exchanges[0].sock)
        self.assertTrue(all(item.closed for item in sockets + responses + connections))
        self.assertEqual(actions, ["connect", "TLS", "GET", "read"])

    def test_blocked_resolver_is_revoked_before_terminal_accounting(self):
        entered, release, exchanges = threading.Event(), threading.Event(), []
        original_run = transport._Exchange.run

        def resolve(*args):
            entered.set()
            os.kill(os.getpid(), signal.SIGALRM)
            if not release.wait(4):
                raise AssertionError("test did not release its resolver")
            return [(2, 1, 6, "", ("127.0.0.1", 443))]

        def running(exchange):
            exchanges.append(exchange)
            original_run(exchange)

        started = time.monotonic()
        with mock.patch.object(transport.socket, "getaddrinfo", resolve), \
                mock.patch.object(transport.socket, "socket") as sockets, \
                mock.patch.object(transport._Exchange, "run", running):
            try:
                with self.assertRaises(budget.Interrupted) as caught:
                    with budget.Budget(started + 2, time.monotonic, 0).enforce("blocked DNS"):
                        transport.HttpsTransport(context=mock.Mock()).request(
                            "https://api.example.invalid/", {"Authorization": "synthetic"}, 100, 10)
                self.assertTrue(entered.is_set())
                self.assertLess(time.monotonic() - started, 1.5)
                self.assertTrue(exchanges[0].aborted)
                self.assertFalse(exchanges[0].done.is_set(), "DNS is still blocked, never stopped")
                self.assertIn("cancelled worker may remain", "; ".join(caught.exception.__notes__))
            finally:
                release.set()
                if exchanges:
                    self.assertTrue(exchanges[0].done.wait(1))
            sockets.assert_not_called()  # no connect, TLS or authenticated request after DNS
            self.assertIsNone(exchanges[0].sock)

    def test_any_exception_in_the_caller_wait_revokes_the_exchange(self):
        original_wait = threading.Event.wait
        for error in (RuntimeError("wait failed"), KeyboardInterrupt("cancelled")):
            with self.subTest(error=type(error).__name__):
                release, exchanges, injected = threading.Event(), [], []
                original_run = transport._Exchange.run

                def running(exchange):
                    exchanges.append(exchange)
                    original_run(exchange)

                def resolve(*args):
                    original_wait(release, 4)
                    return [(2, 1, 6, "", ("127.0.0.1", 443))]

                def wait(event, timeout=None):
                    if exchanges and event is exchanges[0].done and not injected:
                        injected.append(True)
                        raise error
                    return original_wait(event, timeout)

                with mock.patch.object(transport.socket, "getaddrinfo", resolve), \
                        mock.patch.object(transport.socket, "socket") as sockets, \
                        mock.patch.object(transport._Exchange, "run", running), \
                        mock.patch.object(threading.Event, "wait", wait):
                    try:
                        with self.assertRaises(type(error)):
                            transport.HttpsTransport(context=mock.Mock()).request(
                                "https://api.example.invalid/", {}, 100, 10)
                    finally:
                        release.set()
                    self.assertTrue(original_wait(exchanges[0].done, 1))
                    self.assertTrue(exchanges[0].aborted)
                    sockets.assert_not_called()

    def test_interruption_after_open_or_flock_releases_real_owned_descriptor(self):
        original_open, original_flock = os.open, fcntl.flock
        for boundary in ("open", "flock"):
            with self.subTest(boundary=boundary):
                root = self.path(boundary)
                kept = telemetrylab.HistoryRoot(root, sweep=False)
                owned = []
                previous_handler = signal.getsignal(signal.SIGALRM)
                previous_mask = signal.pthread_sigmask(signal.SIG_BLOCK, set())
                previous_timer = signal.getitimer(signal.ITIMER_REAL)

                def opened(path, *args, **kwargs):
                    fd = original_open(path, *args, **kwargs)
                    if str(path) == os.path.join(root, "lock"):
                        owned.append(fd)
                        if boundary == "open":
                            # Pending while the tiny ownership transfer masks SIGALRM;
                            # delivered immediately at restoration, before flock.
                            signal.raise_signal(signal.SIGALRM)
                    return fd

                def locked(fd, flags):
                    original_flock(fd, flags)
                    if boundary == "flock":
                        signal.raise_signal(signal.SIGALRM)

                with mock.patch.object(os, "open", opened), \
                        mock.patch.object(fcntl, "flock", locked):
                    with self.assertRaises(budget.Interrupted):
                        with budget.Budget(time.monotonic() + 2, time.monotonic, 0).enforce(boundary):
                            kept.__enter__()
                kept.__exit__()
                self.assertIsNone(kept.fd)
                self.assertEqual(len(owned), 1)
                with self.assertRaises(OSError) as caught:
                    os.fstat(owned[0])
                self.assertEqual(caught.exception.errno, errno.EBADF)
                # An independent open description must now acquire a real flock.
                with telemetrylab.HistoryRoot(root, sweep=False):
                    pass
                self.assertEqual(signal.getsignal(signal.SIGALRM), previous_handler)
                self.assertEqual(signal.getitimer(signal.ITIMER_REAL), previous_timer)
                self.assertEqual(signal.pthread_sigmask(signal.SIG_BLOCK, set()), previous_mask)

    def test_failed_acquisition_never_unlocks_the_other_owner(self):
        root = self.path("history")
        with telemetrylab.HistoryRoot(root, sweep=False) as owner:
            other = telemetrylab.HistoryRoot(root, sweep=False)
            with self.assertRaises(ContractError):
                other.__enter__()
            other.__exit__()
            self.assertIsNone(other.fd)
            os.fstat(owner.fd)
            with self.assertRaises(ContractError):
                telemetrylab.HistoryRoot(root, sweep=False).__enter__()
        with telemetrylab.HistoryRoot(root, sweep=False):
            pass

    def test_descriptor_transfer_preserves_the_outer_mask_and_flock_is_unmasked(self):
        previous = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGUSR1})
        original_flock = fcntl.flock
        observed = []

        def locked(fd, flags):
            mask = signal.pthread_sigmask(signal.SIG_BLOCK, set())
            self.assertIn(signal.SIGUSR1, mask)
            self.assertNotIn(signal.SIGALRM, mask)
            observed.append(True)
            return original_flock(fd, flags)

        try:
            with mock.patch.object(fcntl, "flock", locked):
                with telemetrylab.HistoryRoot(self.path("masked"), sweep=False):
                    pass
            self.assertTrue(observed)
            self.assertEqual(signal.pthread_sigmask(signal.SIG_BLOCK, set()),
                             previous | {signal.SIGUSR1})
        finally:
            signal.pthread_sigmask(signal.SIG_SETMASK, previous)
