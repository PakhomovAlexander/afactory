"""The frozen schedule: shard assignment, seeded AB/BA blocks and every expected sample slot.

Planning writes these values, a plan reader recomputes and compares them, and comparison
enumerates the same slots to match each record exactly once. One definition serves all three.
"""
import hashlib

PHASES = ("sentinel_start", "warmup", "measured", "sentinel_end")
ORDERS = ("ab", "ba")
BASELINE = "baseline"
CANDIDATE = "candidate"


def assign_shards(instance_ids, shards):
    """Sorted instances dealt round-robin to the frozen shard count."""
    ordered = sorted(instance_ids)
    out = [[] for _ in range(shards)]
    for index, ident in enumerate(ordered):
        out[index % shards].append(ident)
    return out


def _block_bit(seed, instance_id, phase, block):
    material = "af.perf-order/1\x00%d\x00%s\x00%s\x00%d" % (seed, instance_id, phase, block)
    return hashlib.sha256(material.encode("utf-8")).digest()[0] & 1


def block_orders(seed, instance_id, phase, pairs):
    """Two-pair blocks, each a seeded choice between AB,BA and BA,AB."""
    out = []
    for block in range(pairs // 2):
        out.extend(("ab", "ba") if _block_bit(seed, instance_id, phase, block) == 0
                   else ("ba", "ab"))
    return out


def side_at(order, position):
    if order == "aa":
        return BASELINE
    first = BASELINE if order == "ab" else CANDIDATE
    if position == 1:
        return first
    return CANDIDATE if first == BASELINE else BASELINE


def expected_slots(plan, shard):
    """Every planned record of `shard` in execution order, sentinels included."""
    health = plan["health"]
    cases = dict((case["instance_id"], case) for case in plan["cases"])
    slots = []

    def add(phase, instance_id, pair, order):
        for position in (1, 2):
            slots.append({"sequence": len(slots), "phase": phase, "instance_id": instance_id,
                          "pair": pair, "position": position,
                          "side": side_at(order, position)})

    for pair in range(1, health["start_pairs"] + 1):
        add("sentinel_start", health["workload"], pair, "aa")
    for instance_id in plan["shards"][shard]["instances"]:
        case = cases[instance_id]
        for pair, order in enumerate(case["warmup_orders"], 1):
            add("warmup", instance_id, pair, order)
        for pair, order in enumerate(case["measured_orders"], 1):
            add("measured", instance_id, pair, order)
    for pair in range(1, health["end_pairs"] + 1):
        add("sentinel_end", health["workload"], pair, "aa")
    return slots


def slot_key(record):
    return (record["phase"], record["instance_id"], record["pair"], record["position"])
