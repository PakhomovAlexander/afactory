"""Bounded non-secret Linux compute identity; unavailable evidence never compares equal."""
import os
from pathlib import Path
import resource
import re

SCHEMA = "af-durability/compute-identity@2"
LIMIT = 4 * 1024 * 1024
# Linux reserves PROC_CGROUP_INIT_INO (include/linux/proc_ns.h) for the initial
# cgroup namespace; dynamically allocated private namespaces cannot use it.
INITIAL_CGROUP_NAMESPACE = "cgroup:[4026531835]"
ANCESTRY = {"namespace": INITIAL_CGROUP_NAMESPACE, "mount_root": "/",
            "visibility": "unambiguous-full-hierarchy"}
CPU_FIELDS = {"vendor_id", "cpu family", "model", "model name", "stepping",
              "physical id", "core id", "siblings", "cpu cores", "flags"}
CGROUP_FIELDS = {"cpu.max", "cpu.weight", "cpuset.cpus.effective", "cpuset.mems.effective",
                 "memory.max", "memory.high", "memory.swap.max"}


def bounded_text(path):
    with path.open("rb") as stream:
        data = stream.read(LIMIT + 1)
    if len(data) > LIMIT:
        raise ValueError("compute identity exceeds read bound")
    return data.decode("ascii").strip()


def cgroup_limits(proc, cgroup):
    # Only a fully visible unified hierarchy proves all ancestor constraints.
    # A legacy, hybrid or namespace-hidden hierarchy is explicitly unavailable.
    # Mount roots and membership paths are namespace-relative, even when both
    # report "/". Require the kernel's known initial namespace, independently of
    # PID namespaces: matching /proc/1/ns/cgroup could merely match a container's
    # init. readlink is one bounded, read-only procfs operation; missing/unknown
    # evidence fails closed, including kernels without the recognized identity.
    if os.readlink(proc / "self/ns/cgroup") != INITIAL_CGROUP_NAMESPACE:
        raise ValueError("initial cgroup namespace is not proven")
    membership = bounded_text(proc / "self/cgroup").splitlines()
    if len(membership) != 1 or not membership[0].startswith("0::/"):
        raise ValueError("requires a unified cgroup hierarchy")
    # Reject covered mounts and submounts, irrespective of mountinfo order. A
    # matching row alone may describe a hierarchy hidden by another mount.
    if cgroup.resolve() != cgroup:
        raise ValueError("cgroup root is not a resolved path")

    def decode(value):
        return re.sub(r"\\([0-7]{3})", lambda match: chr(int(match[1], 8)), value)

    mounts = {}
    for line in bounded_text(proc / "self/mountinfo").splitlines():
        row = line.split()
        separator = row.index("-")
        if separator < 6 or len(row) != separator + 4 or row[0] in mounts:
            raise ValueError("invalid mount evidence")
        mounts[row[0]] = (row[1], decode(row[3]), Path(decode(row[4])), row[separator + 1])
    relevant = {key: mount for key, mount in mounts.items()
                if cgroup.is_relative_to(mount[2]) or mount[2].is_relative_to(cgroup)}
    roots = [key for key, mount in relevant.items() if mount[2] == cgroup]
    if (len(roots) != 1 or relevant[roots[0]][1] != "/"
            or relevant[roots[0]][3] != "cgroup2"):
        raise ValueError("cgroup ancestors are not fully visible")
    chain = set()
    current = roots[0]
    while current in mounts:
        if current in chain:
            raise ValueError("cyclic mount evidence")
        chain.add(current)
        current = mounts[current][0]
    if not relevant.keys() <= chain or len({m[2] for m in relevant.values()}) != len(relevant):
        raise ValueError("covered or ambiguous cgroup hierarchy")
    relative = Path(membership[0][4:])
    if relative.is_absolute() or ".." in relative.parts or len(relative.parts) > 32:
        raise ValueError("invalid or excessive cgroup depth")
    leaf = cgroup / relative
    limits = []
    for directory in (leaf, *leaf.parents):
        if not directory.is_relative_to(cgroup):
            break
        values = {}
        for name in sorted(CGROUP_FIELDS):
            path = directory / name
            controller = name.split(".", 1)[0]
            if path.exists():
                values[name] = bounded_text(path)
            elif directory == cgroup:
                supported = bounded_text(cgroup / "cgroup.controllers").split()
                if controller not in supported:
                    values[name] = "controller-disabled"
                elif name.startswith("cpuset."):
                    raise ValueError("missing root cpuset capacity")
                else:
                    # The real hierarchy root has no quotas or memory ceilings.
                    values[name] = "unbounded-root"
            elif controller not in bounded_text(directory.parent / "cgroup.subtree_control").split():
                # A missing control is known to be inapplicable only when its parent
                # explicitly proves that controller was not enabled for this child.
                values[name] = "controller-disabled"
            else:
                raise ValueError("missing enabled cgroup capacity control")
        if not all(values.values()):
            raise ValueError("empty cgroup capacity evidence")
        limits.append(values)
    return limits


def compute_identity(proc=Path("/proc"), cgroup=Path("/sys/fs/cgroup"), affinity=None):
    """Return stable capacity, or None without changing native case outcomes/receipts."""
    try:
        cpus = sorted(os.sched_getaffinity(0) if affinity is None else affinity)
        if not cpus or len(cpus) > 4096:
            raise ValueError("missing or excessive CPU affinity")
        processors = {}
        for block in bounded_text(proc / "cpuinfo").split("\n\n"):
            row = dict(line.split(":", 1) for line in block.splitlines() if ":" in line)
            row = {key.strip(): value.strip() for key, value in row.items()}
            if "processor" not in row:
                continue
            number = int(row["processor"])
            if number not in cpus:
                continue
            # These x86 fields identify hardware and topology, without MHz, serials or IDs
            # that change with a process or boot. Unknown architectures fail closed.
            identity = {name: row[name] for name in CPU_FIELDS}
            if not all(identity.values()) or number in processors:
                raise ValueError("incomplete CPU identity")
            processors[number] = identity
        if set(processors) != set(cpus):
            raise ValueError("CPU identity missing for usable CPUs")
        memory = dict(line.split(":", 1) for line in bounded_text(proc / "meminfo").splitlines())
        total, unit = memory["MemTotal"].split()
        if unit != "kB" or int(total) <= 0:
            raise ValueError("missing physical memory capacity")
        return {"schema": SCHEMA, "cpus": [[cpu, processors[cpu]] for cpu in cpus],
                "memory_bytes": int(total) * 1024, "ancestry": dict(ANCESTRY),
                "cgroups": cgroup_limits(proc, cgroup),
                "rlimits": {name: list(resource.getrlimit(getattr(resource, name)))
                            for name in sorted(dir(resource)) if name.startswith("RLIMIT_")}}
    except (OSError, ValueError, KeyError, IndexError, UnicodeError):
        return None


def complete_identity(value):
    if (not isinstance(value, dict) or set(value) != {"schema", "cpus", "memory_bytes", "cgroups", "rlimits", "ancestry"}
            or value["ancestry"] != ANCESTRY or value["schema"] != SCHEMA or type(value["memory_bytes"]) is not int
            or value["memory_bytes"] <= 0):
        return False
    cpus, groups, limits = value["cpus"], value["cgroups"], value["rlimits"]
    if (not isinstance(cpus, list) or not 1 <= len(cpus) <= 4096
            or not isinstance(groups, list) or not 1 <= len(groups) <= 33
            or not isinstance(limits, dict)
            or not {"RLIMIT_AS", "RLIMIT_CPU", "RLIMIT_DATA", "RLIMIT_RSS", "RLIMIT_NPROC"} <= limits.keys()):
        return False
    seen = set()
    for row in cpus:
        if (not isinstance(row, list) or len(row) != 2 or type(row[0]) is not int or row[0] < 0
                or row[0] in seen or not isinstance(row[1], dict) or set(row[1]) != CPU_FIELDS
                or not all(isinstance(v, str) and v for v in row[1].values())):
            return False
        seen.add(row[0])
    for group in groups:
        if (not isinstance(group, dict) or set(group) != CGROUP_FIELDS
                or not all(isinstance(v, str) and v for v in group.values())):
            return False
    return all(isinstance(name, str) and name.startswith("RLIMIT_") and isinstance(pair, list)
               and len(pair) == 2 and all(type(n) is int and n >= -1 for n in pair)
               for name, pair in limits.items())
