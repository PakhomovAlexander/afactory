"""Exact statistics of `paired_median_tolerance_v2` and the sentinel health rule.

All arithmetic is on `fractions.Fraction`; no float ever enters a bound, a classification or
a health flag.
"""
from fractions import Fraction
from math import comb

from .values import ContractError

ALGORITHM = "paired_median_tolerance_v2"
HEALTH_RULE = "sentinel_start_end_v1"

REGRESSION = "regression"
IMPROVEMENT = "improvement"
WITHIN_TOLERANCE = "within_tolerance"
NON_REGRESSION = "non_regression"
INCONCLUSIVE = "inconclusive"
ELIGIBLE = (IMPROVEMENT, WITHIN_TOLERANCE, NON_REGRESSION)


def critical_rank(pairs, family_size, alpha):
    """The largest one-based rank k of the two-sided order-statistic interval.

    k satisfies 1 <= k <= floor((N+1)/2) and 2 * sum(comb(N, j), j < k) / 2^N <= alpha / K.
    Returns None when no k exists: the plan is impossible and refused before execution.
    """
    if type(pairs) is not int or pairs < 1:
        raise ContractError("pairs must be a positive integer")
    if type(family_size) is not int or family_size < 1:
        raise ContractError("the family size K must be a positive integer")
    alpha = Fraction(alpha)
    if not 0 < alpha < 1:
        raise ContractError("alpha must lie strictly between 0 and 1")
    limit = alpha / family_size
    total = 2 ** pairs
    best = None
    tail = 0
    for k in range(1, (pairs + 1) // 2 + 1):
        tail += comb(pairs, k - 1)
        if Fraction(2 * tail, total) <= limit:
            best = k
        else:
            break
    return best


def tail_probability(pairs, rank):
    """The exact two-sided miss probability 2 * sum(comb(N, j), j < k) / 2^N of rank k."""
    return Fraction(2 * sum(comb(pairs, j) for j in range(rank)), 2 ** pairs)


def tolerance(baseline, relative, absolute):
    """T_i = max(r * A_i, a); `a > 0` makes a zero baseline well defined."""
    return max(relative * baseline, absolute)


def paired_losses(baseline, candidate, relative, absolute, direction):
    """Baseline-normalized paired losses z_i; positive means the candidate is worse."""
    if len(baseline) != len(candidate):
        raise ContractError("paired losses need equal baseline and candidate counts")
    if absolute <= 0:
        raise ContractError("the absolute tolerance must be strictly positive")
    losses = []
    for a_value, b_value in zip(baseline, candidate):
        scale = tolerance(a_value, relative, absolute)
        if direction == "lower_is_better":
            losses.append((b_value - a_value) / scale)
        elif direction == "higher_is_better":
            losses.append((a_value - b_value) / scale)
        else:
            raise ContractError("unknown metric direction " + repr(direction))
    return losses


def order_interval(losses, rank):
    """[L, U] = [sorted_z[k], sorted_z[N-k+1]] with one-based order statistics; ties kept."""
    ordered = sorted(losses)
    count = len(ordered)
    if not 1 <= rank <= (count + 1) // 2:
        raise ContractError("rank %d is outside 1..floor((N+1)/2) for N=%d" % (rank, count))
    return ordered[rank - 1], ordered[count - rank]


def classify(lower, upper):
    """Map an interval onto the relative gate; boundary equality never fails or improves."""
    one = Fraction(1)
    if lower > one:
        return REGRESSION
    if upper < -one:
        return IMPROVEMENT
    if upper <= one and lower >= -one:
        return WITHIN_TOLERANCE
    if upper <= one:
        return NON_REGRESSION
    return INCONCLUSIVE


def median(values):
    ordered = sorted(values)
    count = len(ordered)
    if not count:
        raise ContractError("median of no values")
    middle = count // 2
    if count % 2:
        return Fraction(ordered[middle])
    return (Fraction(ordered[middle - 1]) + Fraction(ordered[middle])) / 2


def nearest_rank(values, percent):
    """The nearest-rank percentile: the ceil(p/100 * n)-th smallest value."""
    ordered = sorted(values)
    count = len(ordered)
    if not count:
        raise ContractError("percentile of no values")
    numerator = Fraction(percent) / 100 * count
    rank = -(-numerator.numerator // numerator.denominator)
    return Fraction(ordered[max(1, rank) - 1])


def median_absolute_deviation(values):
    centre = median(values)
    return median([abs(value - centre) for value in values])


def sentinel_health(start, end, max_shift, max_mad_ratio, floor):
    """Evaluate one shard's start and end sentinel times under the declared health rule.

    Flags an absolute start/end median shift above `max_shift`, or a within-phase median
    absolute deviation above `max_mad_ratio` of that phase's median, each over a denominator
    of at least `floor`. Thresholds are exclusive: equality is healthy.
    """
    start_median = median(start)
    end_median = median(end)
    shift = abs(end_median - start_median) / max(start_median, floor)
    start_mad = median_absolute_deviation(start) / max(start_median, floor)
    end_mad = median_absolute_deviation(end) / max(end_median, floor)
    flags = []
    if shift > max_shift:
        flags.append("median_shift")
    if start_mad > max_mad_ratio:
        flags.append("start_dispersion")
    if end_mad > max_mad_ratio:
        flags.append("end_dispersion")
    return {"start_median": start_median, "end_median": end_median, "shift": shift,
            "start_mad_ratio": start_mad, "end_mad_ratio": end_mad, "flags": flags}
