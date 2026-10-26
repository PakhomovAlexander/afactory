"""Real independent flock descriptions across caught API and constructor failures."""
import fcntl
import os
import signal
import time
from unittest import mock

from perflab import budget, hostlock, runner, telemetrylab
from tests import telemetry_fixtures as fx
from tests.support import TempDir


class Handoff(TempDir):
    def setUp(self):
        super().setUp()
        self.binaries = fx.binaries(self)
        self.executor = fx.StandIn(self.binaries)

    def settings(self, **kwargs):
        return fx.settings(self, self.executor, experiments=["overhead"], rounds=1,
                           lock_directory=self.tmp, **kwargs)

    def independent_holder(self):
        fd = os.open(os.path.join(self.tmp, hostlock.MACHINE_LOCK_NAME), os.O_RDONLY)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        finally:
            os.close(fd)

    def test_exit_time_expiry_after_actual_acquisition_releases_before_api_returns(self):
        now = [0.0]
        original = runner.machine_lock
        def acquire(*args, **kwargs):
            held = original(*args, **kwargs)
            now[0] = 240 * 60
            return held
        with mock.patch.object(runner, "machine_lock", acquire), \
                mock.patch.object(telemetrylab, "_executor") as executor:
            with self.assertRaises(budget.Expired):
                telemetrylab.run(self.settings(clock=lambda: now[0]))
            executor.assert_not_called()
        self.independent_holder()
        self.assertEqual(self.executor.calls, [])

    def test_real_alarm_at_return_handoff_releases_without_masking_the_wait(self):
        offset = [0.0]
        original = runner.machine_lock
        def acquire(*args, **kwargs):
            held = original(*args, **kwargs)
            self.assertGreater(signal.getitimer(signal.ITIMER_REAL)[0], 0)
            time.sleep(30)  # An actual alarm between construction and caller assignment.
            return held
        real_sync = telemetrylab._sync
        def sync(allowance, step, function, *args, **kwargs):
            if step == "acquiring telemetry machine lock":
                offset[0] = allowance.deadline - allowance.reserve - time.monotonic() - .08
            return real_sync(allowance, step, function, *args, **kwargs)
        started = time.monotonic()
        with mock.patch.object(runner, "machine_lock", acquire), \
                mock.patch.object(telemetrylab, "_sync", sync), \
                mock.patch.object(telemetrylab, "_executor") as executor:
            with self.assertRaises(budget.Interrupted):
                telemetrylab.run(self.settings(clock=lambda: time.monotonic() + offset[0]))
            executor.assert_not_called()
        self.assertLess(time.monotonic() - started, 1)
        self.independent_holder()

    def test_constructor_cleans_validation_and_post_flock_interruptions(self):
        for boundary in ("validation", "post_flock", "open_handoff"):
            with self.subTest(boundary=boundary):
                original_open, original_flock = os.open, fcntl.flock
                held = []
                def opened(*args, **kwargs):
                    fd = original_open(*args, **kwargs)
                    if boundary == "open_handoff": signal.raise_signal(signal.SIGALRM)
                    return fd
                def flock(*args):
                    result = original_flock(*args)
                    if boundary == "post_flock": raise RuntimeError("after flock")
                    return result
                def validation(lock):
                    raise RuntimeError("validation failed")
                with mock.patch.object(os, "open", opened), \
                        mock.patch.object(fcntl, "flock", flock), \
                        mock.patch.object(hostlock.MachineLock, "_check_file", validation) \
                            if boundary == "validation" else mock.patch.object(
                                hostlock, "LOCK_MODE", hostlock.LOCK_MODE):
                    with self.assertRaises((RuntimeError, budget.Interrupted)):
                        with budget.Budget(time.monotonic() + 1, time.monotonic, 0).enforce("lock"):
                            hostlock.MachineLock(self.tmp, register=held.append)
                self.assertIsNone(held[0].fd)
                self.independent_holder()

    def test_normal_run_releases_and_a_subsequent_run_remains_usable(self):
        for output in ("one", "two"):
            result = telemetrylab.run(self.settings(output=output))
            self.assertEqual(result["status"], "complete")
            self.independent_holder()
