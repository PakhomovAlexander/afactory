"""The identity and interference observations of the host that runs a shard (ADR-0168).

Every field is the value read from the operating system, or explicitly unknown with the reason
it could not be read; nothing unsupported becomes an invented zero, and an unknown observation
is never a reason to discard a sample. Identity (`identity`) is fixed for a shard and hashed into
its `host_id`; interference (`load`) is observed at the start and the end of its schedule.
"""
import os
import platform
import subprocess

from . import contracts

TOOL_TIMEOUT_SECONDS = 10
VALUE_MAX = 1024


def known(value):
    text = str(value).strip()
    if not text:
        return unknown("the source reported nothing")
    return {"value": text[:VALUE_MAX]}


def unknown(reason):
    return {"unknown": str(reason)[:VALUE_MAX] or "unknown"}


class Source(object):
    """The reads an observation makes; tests replace them with fixed answers."""

    def __init__(self, system=None):
        self.system = system or platform.system()

    def read(self, path):
        with open(path, "rb") as stream:
            return stream.read(1 << 20).decode("utf-8", "replace")

    def run(self, argv):
        result = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                timeout=TOOL_TIMEOUT_SECONDS,
                                env={"PATH": "/usr/sbin:/usr/bin:/bin:/sbin", "LC_ALL": "C"})
        if result.returncode != 0:
            raise OSError("%s exited %d" % (" ".join(argv), result.returncode))
        return result.stdout.decode("utf-8", "replace")

    def listdir(self, path):
        return os.listdir(path)

    def affinity(self):
        return sorted(os.sched_getaffinity(0))

    def loadavg(self):
        return os.getloadavg()


def _attempt(function, what):
    try:
        return function()
    except (OSError, ValueError, KeyError, IndexError, subprocess.SubprocessError,
            AttributeError) as error:
        return unknown("%s: %s" % (what, error))


def _sysctl(source, name):
    return known(source.run(["sysctl", "-n", name]))


def _cpuinfo(source):
    fields = {}
    for line in source.read("/proc/cpuinfo").splitlines():
        if ":" in line:
            key, value = line.split(":", 1)
            fields.setdefault(key.strip().lower(), value.strip())
    return fields


def _linux_cpu_model(source):
    fields = _cpuinfo(source)
    for key in ("model name", "hardware", "cpu model"):
        if fields.get(key):
            return known(fields[key])
    if fields.get("cpu implementer") or fields.get("cpu part"):
        return known("implementer %s part %s variant %s revision %s" % (
            fields.get("cpu implementer", "?"), fields.get("cpu part", "?"),
            fields.get("cpu variant", "?"), fields.get("cpu revision", "?")))
    return unknown("/proc/cpuinfo names no CPU model")


def _linux_topology(source):
    root = "/sys/devices/system/cpu"
    cores, packages, logical = set(), set(), 0
    for name in source.listdir(root):
        if not (name.startswith("cpu") and name[3:].isdigit()):
            continue
        topology = os.path.join(root, name, "topology")
        try:
            package = source.read(os.path.join(topology, "physical_package_id")).strip()
            core = source.read(os.path.join(topology, "core_id")).strip()
        except OSError:
            continue
        logical += 1
        packages.add(package)
        cores.add((package, core))
    if not logical:
        return unknown("%s exposes no CPU topology" % root)
    return known("%d logical CPUs on %d physical cores in %d packages"
                 % (logical, len(cores), len(packages)))


def _linux_cache(source):
    root = "/sys/devices/system/cpu/cpu0/cache"
    entries = []
    for name in sorted(source.listdir(root)):
        if not name.startswith("index"):
            continue
        base = os.path.join(root, name)
        entries.append("L%s %s %s" % (source.read(os.path.join(base, "level")).strip(),
                                     source.read(os.path.join(base, "type")).strip(),
                                     source.read(os.path.join(base, "size")).strip()))
    return known(", ".join(entries)) if entries else unknown("%s lists no cache" % root)


def _linux_memory(source):
    for line in source.read("/proc/meminfo").splitlines():
        if line.startswith("MemTotal:"):
            fields = line.split()
            if len(fields) >= 3 and fields[2] == "kB":
                return known(int(fields[1]) * 1024)
    return unknown("/proc/meminfo has no MemTotal in kB")


def _linux_os_image(source):
    for line in source.read("/etc/os-release").splitlines():
        if line.startswith("PRETTY_NAME="):
            return known(line.split("=", 1)[1].strip().strip('"'))
    return unknown("/etc/os-release has no PRETTY_NAME")


def _linux_virtualization(source):
    flags = _cpuinfo(source).get("flags", "").split()
    parts = []
    for name in ("sys_vendor", "product_name"):
        try:
            text = source.read("/sys/class/dmi/id/" + name).strip()
        except OSError:
            text = ""
        if text:
            parts.append(text)
    hypervisor = "hypervisor flag set" if "hypervisor" in flags else (
        "no hypervisor flag" if flags else "no CPU flags exposed")
    return known("; ".join(parts + [hypervisor]))


def _linux_microcode(source):
    value = _cpuinfo(source).get("microcode")
    return known(value) if value else unknown("/proc/cpuinfo exposes no microcode revision")


def _own_cgroup(source):
    for line in source.read("/proc/self/cgroup").splitlines():
        if line.startswith("0::"):
            return "/sys/fs/cgroup" + line[3:].strip().rstrip("/")
    raise OSError("not in a cgroup v2 hierarchy")


def _linux_cgroup_file(source, name):
    return known(" ".join(source.read(os.path.join(_own_cgroup(source), name)).split()))


def _libc():
    name, version = platform.libc_ver()
    return known("%s %s" % (name, version)) if name else unknown(
        "the C library did not identify itself to platform.libc_ver")


def identity(container_runtime=None, source=None):
    """This host's identity: a value or a reasoned unknown for every declared key."""
    source = source or Source()
    linux = source.system == "Linux"
    mac = source.system == "Darwin"

    def either(on_linux, on_mac, what):
        if linux:
            return _attempt(lambda: on_linux(), what)
        if mac:
            return _attempt(lambda: on_mac(), what)
        return unknown("%s is not read on %s" % (what, source.system or "this system"))

    doc = {
        "cpu_model": either(lambda: _linux_cpu_model(source),
                            lambda: _sysctl(source, "machdep.cpu.brand_string"), "cpu model"),
        "cpu_topology": either(lambda: _linux_topology(source), lambda: known(
            "%s logical CPUs on %s physical cores in %s packages" % (
                source.run(["sysctl", "-n", "hw.logicalcpu"]).strip(),
                source.run(["sysctl", "-n", "hw.physicalcpu"]).strip(),
                source.run(["sysctl", "-n", "hw.packages"]).strip())), "cpu topology"),
        "logical_cpus": known(os.cpu_count()) if os.cpu_count() else unknown(
            "os.cpu_count reported nothing"),
        "kernel": known("%s %s %s" % (platform.system(), platform.release(), platform.version())),
        "os_image": either(lambda: _linux_os_image(source), lambda: known("macOS %s (%s)" % (
            source.run(["sw_vers", "-productVersion"]).strip(),
            source.run(["sw_vers", "-buildVersion"]).strip())), "os image"),
        "libc": _libc() if linux else unknown(
            "macOS has no separately versioned C library; it ships in the OS image")
        if mac else unknown("not read on this system"),
        "memory_bytes": either(lambda: _linux_memory(source),
                               lambda: _sysctl(source, "hw.memsize"), "memory"),
        "virtualization": either(lambda: _linux_virtualization(source), lambda: known(
            "kern.hv_vmm_present=%s" % source.run(["sysctl", "-n", "kern.hv_vmm_present"])
            .strip()), "virtualization"),
        "microcode": either(lambda: _linux_microcode(source), lambda: unknown(
            "macOS exposes no CPU microcode revision"), "microcode"),
        "container_runtime": known(container_runtime) if container_runtime else unknown(
            "no container runtime: samples run in a trusted local process group"),
        "affinity": either(lambda: known(",".join(str(cpu) for cpu in source.affinity())),
                           lambda: unknown("macOS exposes no CPU affinity"), "affinity"),
        "cpu_quota": either(lambda: _linux_cgroup_file(source, "cpu.max"),
                            lambda: unknown("macOS has no cgroup CPU quota"), "cpu quota"),
        "cache": either(lambda: _linux_cache(source), lambda: _mac_cache(source), "cache"),
        "aslr": either(lambda: known("randomize_va_space=" + source.read(
            "/proc/sys/kernel/randomize_va_space").strip()), lambda: unknown(
            "macOS exposes no system-wide ASLR setting"), "aslr"),
        "python": known(platform.python_version()),
    }
    contracts.HOST.validate(doc, "host")
    return doc


def _mac_cache(source):
    """Each cache level macOS reports, and which it does not."""
    parts = []
    for label, name in (("L1d", "hw.l1dcachesize"), ("L2", "hw.l2cachesize"),
                        ("L3", "hw.l3cachesize")):
        try:
            value = source.run(["sysctl", "-n", name]).strip()
        except OSError:
            value = ""
        parts.append("%s %s bytes" % (label, value) if value else "%s not exposed" % label)
    return known(", ".join(parts))


def load(source=None):
    """Interference observed now: load, steal, pressure, throttling and cgroup memory events."""
    source = source or Source()
    linux = source.system == "Linux"

    def linux_only(function, what):
        if linux:
            return _attempt(function, what)
        return unknown("%s is a Linux observation; this host is %s" % (
            what, source.system or "unknown"))

    def steal():
        fields = source.read("/proc/stat").splitlines()[0].split()
        if fields[0] != "cpu" or len(fields) < 9:
            raise ValueError("/proc/stat has no steal column")
        return known("steal_jiffies=%s" % fields[8])

    def pressure(name):
        lines = source.read("/proc/pressure/" + name).splitlines()
        return known(" | ".join(line.strip() for line in lines))

    doc = {
        "load_average": _attempt(lambda: known(" ".join("%.2f" % value
                                                        for value in source.loadavg())),
                                 "load average"),
        "cpu_steal": linux_only(steal, "cpu steal"),
        "cpu_pressure": linux_only(lambda: pressure("cpu"), "cpu pressure"),
        "memory_pressure": linux_only(lambda: pressure("memory"), "memory pressure"),
        "io_pressure": linux_only(lambda: pressure("io"), "io pressure"),
        "cpu_throttling": linux_only(lambda: _linux_cgroup_file(source, "cpu.stat"),
                                     "cpu throttling"),
        "memory_events": linux_only(lambda: _linux_cgroup_file(source, "memory.events"),
                                    "memory events"),
    }
    contracts.LOAD.validate(doc, "load")
    return doc
