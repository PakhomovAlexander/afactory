"""Exact arithmetic of paired_median_tolerance_v2 and of the sentinel health rule."""
from fractions import Fraction
from math import comb
import hashlib
import unittest

from perflab import stats
from perflab.values import ContractError

ALPHA = Fraction(1, 20)


def brute_rank(pairs, family, alpha):
    best = None
    for k in range(1, (pairs + 1) // 2 + 1):
        if Fraction(2 * sum(comb(pairs, j) for j in range(k)), 2 ** pairs) <= alpha / family:
            best = k
    return best


class Ranks(unittest.TestCase):
    def test_rank_is_the_largest_admissible_order_statistic(self):
        # N=12, K=1: k=3 has 2*79/4096 = 0.0386 <= 0.05; k=4 has 0.146.
        self.assertEqual(stats.critical_rank(12, 1, ALPHA), 3)
        self.assertEqual(stats.tail_probability(12, 3), Fraction(158, 4096))
        for pairs in (12, 20, 40, 200):
            for family in (1, 2, 28, 64):
                self.assertEqual(stats.critical_rank(pairs, family, ALPHA),
                                 brute_rank(pairs, family, ALPHA), (pairs, family))

    def test_boundary_equality_of_the_tail_is_admissible(self):
        # N=6, k=1: 2/64 = 1/32; alpha/K exactly 1/32 admits k = 1, anything smaller does not.
        self.assertEqual(stats.critical_rank(6, 1, Fraction(1, 32)), 1)
        self.assertIsNone(stats.critical_rank(6, 1, Fraction(1, 33)))

    def test_impossible_n_for_k_is_refused(self):
        self.assertIsNone(stats.critical_rank(4, 1, ALPHA))
        self.assertIsNone(stats.critical_rank(12, 200, ALPHA))
        self.assertIsNotNone(stats.critical_rank(12, 64, ALPHA))
        with self.assertRaises(ContractError):
            stats.critical_rank(0, 1, ALPHA)
        with self.assertRaises(ContractError):
            stats.critical_rank(12, 0, ALPHA)
        with self.assertRaises(ContractError):
            stats.critical_rank(12, 1, Fraction(1))


class Losses(unittest.TestCase):
    def test_tolerance_uses_the_absolute_floor_and_handles_zero_baseline(self):
        r, a = Fraction(5, 100), Fraction(1)
        self.assertEqual(stats.tolerance(Fraction(0), r, a), 1)
        self.assertEqual(stats.tolerance(Fraction(10), r, a), 1)
        self.assertEqual(stats.tolerance(Fraction(100), r, a), 5)
        losses = stats.paired_losses([Fraction(0), Fraction(100)], [Fraction(3), Fraction(90)],
                                     r, a, "lower_is_better")
        self.assertEqual(losses, [Fraction(3), Fraction(-2)])
        higher = stats.paired_losses([Fraction(100)], [Fraction(90)], r, a, "higher_is_better")
        self.assertEqual(higher, [Fraction(2)])
        with self.assertRaises(ContractError):
            stats.paired_losses([Fraction(1)], [Fraction(1)], r, Fraction(0), "lower_is_better")

    def test_ties_are_kept(self):
        self.assertEqual(stats.order_interval([Fraction(1)] * 12, 3), (1, 1))
        self.assertEqual(stats.order_interval([0, 0, 5, 5, 5, 5, 5, 5, 5, 5, 9, 9], 3), (5, 5))


class Classification(unittest.TestCase):
    def interval(self, lower, upper):
        return stats.classify(Fraction(lower), Fraction(upper))

    def test_the_design_examples(self):
        self.assertEqual(self.interval("-2", "1/2"), stats.NON_REGRESSION)
        self.assertIn(self.interval("-2", "1/2"), stats.ELIGIBLE)
        self.assertEqual(self.interval("-1/5", "7/5"), stats.INCONCLUSIVE)
        self.assertNotIn(self.interval("-1/5", "7/5"), stats.ELIGIBLE)

    def test_boundary_equality(self):
        self.assertEqual(self.interval(1, 1), stats.WITHIN_TOLERANCE)
        self.assertEqual(self.interval(1, 3), stats.INCONCLUSIVE)
        self.assertEqual(self.interval("1000001/1000000", 3), stats.REGRESSION)
        self.assertEqual(self.interval(-1, 1), stats.WITHIN_TOLERANCE)
        self.assertEqual(self.interval(-3, -1), stats.NON_REGRESSION)
        self.assertEqual(self.interval(-3, "-1000001/1000000"), stats.IMPROVEMENT)
        self.assertEqual(self.interval(-3, 2), stats.INCONCLUSIVE)

    def test_every_interval_has_exactly_one_class(self):
        grid = [Fraction(n, 2) for n in range(-6, 7)]
        for lower in grid:
            for upper in grid:
                if lower <= upper:
                    self.assertIn(self.interval(lower, upper),
                                  (stats.REGRESSION, stats.IMPROVEMENT, stats.WITHIN_TOLERANCE,
                                   stats.NON_REGRESSION, stats.INCONCLUSIVE))


class Descriptive(unittest.TestCase):
    def test_median_and_nearest_rank(self):
        self.assertEqual(stats.median([3, 1, 2]), 2)
        self.assertEqual(stats.median([4, 1, 2, 3]), Fraction(5, 2))
        values = list(range(1, 21))
        self.assertEqual(stats.nearest_rank(values, 95), 19)
        self.assertEqual(stats.nearest_rank(list(range(1, 41)), 95), 38)
        self.assertEqual(stats.nearest_rank([7], 95), 7)


class Health(unittest.TestCase):
    def test_thresholds_are_exclusive_and_floored(self):
        limit = Fraction(1, 5)
        floor = Fraction(1)
        same = [Fraction(10)] * 10
        self.assertEqual(stats.sentinel_health(same, [Fraction(12)] * 10, limit, limit,
                                               floor)["flags"], [])
        self.assertEqual(stats.sentinel_health(same, [Fraction(121, 10)] * 10, limit, limit,
                                               floor)["flags"], ["median_shift"])
        # Sub-millisecond medians are judged against the 1-ms floor.
        tiny = stats.sentinel_health([Fraction(1, 10)] * 10, [Fraction(3, 10)] * 10, limit,
                                     limit, floor)
        self.assertEqual(tiny["shift"], Fraction(1, 5))
        self.assertEqual(tiny["flags"], [])
        spread = [Fraction(v) for v in (5, 5, 5, 10, 10, 10, 10, 15, 15, 15)]
        self.assertIn("start_dispersion", stats.sentinel_health(spread, same, limit, limit,
                                                                floor)["flags"])


class SyntheticNoise(unittest.TestCase):
    """SYNTHETIC: deterministic pseudo-random A/A trials, not measurements of anything."""

    def draws(self, trial, count):
        out = []
        for index in range(count):
            digest = hashlib.sha256(b"af.perf-synthetic-noise/1:%d:%d" % (trial, index)).digest()
            out.append(Fraction(20000 + int.from_bytes(digest[:2], "big") % 4000, 1000))
        return out

    def test_synthetic_a_a_trials_never_report_a_regression_or_improvement(self):
        pairs, family = 40, 28
        rank = stats.critical_rank(pairs, family, ALPHA)
        seen = set()
        for trial in range(200):
            draws = self.draws(trial, 2 * pairs)
            losses = stats.paired_losses(draws[:pairs], draws[pairs:], Fraction(5, 100),
                                         Fraction(1), "lower_is_better")
            seen.add(stats.classify(*stats.order_interval(losses, rank)))
        self.assertTrue(seen <= {stats.WITHIN_TOLERANCE, stats.NON_REGRESSION,
                                 stats.INCONCLUSIVE}, seen)

    def test_synthetic_large_shift_is_detected(self):
        pairs, family = 40, 28
        rank = stats.critical_rank(pairs, family, ALPHA)
        draws = self.draws(999, 2 * pairs)
        slowed = [value * Fraction(13, 10) for value in draws[pairs:]]
        losses = stats.paired_losses(draws[:pairs], slowed, Fraction(5, 100), Fraction(1),
                                     "lower_is_better")
        self.assertEqual(stats.classify(*stats.order_interval(losses, rank)), stats.REGRESSION)


if __name__ == "__main__":
    unittest.main()
