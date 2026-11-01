"""Disposable Git fixtures and a stand-in toolchain for the build orchestration tests.

The stand-in `cargo` is a small Python program, never product code: it "compiles" a fixture
workspace by checking that every symbol an example declares with `// requires: NAME` is a
`pub fn NAME` of its package and that every `#[path = ...]` module exists, and writes the
inputs it read into the output binary. The AF binary embeds the commit `git rev-parse
--short=12 HEAD` reports in the package directory, the same command crates/af/build.rs runs.
The fixture repositories are real Git repositories with real detached worktrees.
"""
import json
import os
import sys

from perflab import contracts, planning
from perflab import suite as suite_module
from perflab.values import sha256_bytes
from tests.support import SUITE, Repo

FAKE_RUSTC = r'''
import os, sys
args = sys.argv[1:]
if args == ["-vV"]:
    print("rustc 1.88.0 (fixture 2025-06-23)\nbinary: rustc\nhost: %s\nrelease: 1.88.0" % HOST)
elif args[:2] == ["--print", "target-libdir"]:
    print(os.path.dirname(os.path.abspath(sys.argv[0])))
elif args == ["--print", "sysroot"]:
    print(os.path.dirname(os.path.dirname(os.path.abspath(sys.argv[0]))))
else:
    sys.exit(2)
'''

FAKE_CARGO = r'''
import os, re, subprocess, sys
args = sys.argv[1:]
if args == ["-V"]:
    print("cargo 1.88.0 (fixture)"); sys.exit(0)
if args[0] == "fetch":
    assert "--locked" in args and os.path.isfile("Cargo.lock"); sys.exit(0)
assert args[0] == "build" and "--locked" in args and "--offline" in args, args
assert os.environ.get("CARGO_NET_OFFLINE") == "true" and "GITHUB_TOKEN" not in os.environ
leaked = [key for key in os.environ if key not in ALLOWED]
if leaked:
    print("unexpected build variables " + ", ".join(sorted(leaked))); sys.exit(101)
target_dir = args[args.index("--target-dir") + 1]
target = args[args.index("--target") + 1]
package = args[args.index("-p") + 1]
release = os.path.join(target_dir, target, "release")
def read(path):
    with open(path, "rb") as stream:
        return stream.read()
if "--bin" in args:
    source = read("crates/af/src/main.rs")
    if b"COMPILE_ERROR" in source:
        print("error: af does not compile"); sys.exit(101)
    commit = subprocess.run(["git", "rev-parse", "--short=12", "HEAD"], cwd="crates/af",
                            stdout=subprocess.PIPE).stdout.strip() or b"unknown"
    if b"NO_COMMIT" in source:
        commit = b"unknown"
    if b"TOUCH_SOURCE" in source:
        with open("README", "ab") as stream:
            stream.write(b"written by a build script\n")
    os.makedirs(release, exist_ok=True)
    # Hostile outputs a measured build can leave in its own target directory (ADR-0172): its
    # binary a symlink to a path outside it, its release directory a symlink to another
    # directory, or a sparse binary larger than the artifact budget.
    escape = re.search(rb"SYMLINK_OUTPUT:(\S+)", source)
    if escape:
        os.symlink(escape.group(1).decode(), os.path.join(release, "af"))
        sys.exit(0)
    escape = re.search(rb"SYMLINK_RELEASE:(\S+)", source)
    if escape:
        os.rmdir(release)
        os.symlink(escape.group(1).decode(), release)
        sys.exit(0)
    with open(os.path.join(release, "af"), "wb") as stream:
        stream.write(b"FIXTURE-AF commit=" + commit + b"\n" + source)
        if b"SPARSE_OUTPUT" in source:
            stream.truncate((4 << 30) + 1)
    sys.exit(0)
name = args[args.index("--example") + 1]
root = "crates/%s" % package
library = read(os.path.join(root, "src/lib.rs")).decode()
if "MUTATE_OVERLAY" in library:
    # A measured source's build script rewriting the authority closure it is compiled with.
    for current, _, names in os.walk(os.path.join(root, "examples")):
        for entry in names:
            path = os.path.join(current, entry)
            os.chmod(path, 0o644)
            with open(path, "ab") as stream:
                stream.write(b"// MUTATED by a build script\n")
    if "MUTATE_OVERLAY_AND_FAIL" in library:
        print("error: the mutated example does not compile"); sys.exit(101)
pending = [os.path.join(root, "examples", name + ".rs")]
body = b""
while pending:
    path = pending.pop()
    if not os.path.isfile(path):
        print("error: %s not found" % path); sys.exit(101)
    text = read(path)
    body += text
    for symbol in re.findall(r"// requires: (\w+)", text.decode()):
        if "pub fn %s(" % symbol not in library:
            print("error: cannot find function %s in %s" % (symbol, package)); sys.exit(101)
    for module in re.findall(r'#\[path = "([^"]+)"\]', text.decode()):
        pending.append(os.path.join(os.path.dirname(path), module))
os.makedirs(os.path.join(release, "examples"), exist_ok=True)
escape = re.search(r"SYMLINK_EXAMPLE:(\S+)", library)
if escape:
    # A measured build leaving its compiled probe as a symlink out of its target (ADR-0172).
    os.symlink(escape.group(1), os.path.join(release, "examples", name))
    sys.exit(0)
with open(os.path.join(release, "examples", name), "wb") as stream:
    stream.write(b"FIXTURE-EXAMPLE " + name.encode() + b"\n" + body)
'''

ALLOWED = ("PATH", "HOME", "TMPDIR", "CARGO_HOME", "RUSTC", "LC_ALL", "TZ", "GIT_CONFIG_NOSYSTEM",
           "GIT_CONFIG_GLOBAL", "GIT_TERMINAL_PROMPT", "CARGO_TERM_COLOR", "CARGO_NET_OFFLINE",
           "CARGO_ENCODED_RUSTFLAGS", "CARGO_PROFILE_RELEASE_DEBUG", "__CF_USER_TEXT_ENCODING")


FAKE_CARGO_ZIGBUILD = r'''
import os, sys
if sys.argv[1:] == ["--version"]:
    print("cargo-zigbuild %s" % VERSION); sys.exit(0)
assert sys.argv[1] == "zigbuild", sys.argv
zig = os.environ["PATH"].split(":")[0]
assert os.access(os.path.join(zig, "zig"), os.X_OK), "zig comes first on PATH"
cargo = os.path.join(os.path.dirname(os.environ["RUSTC"]), "cargo")
os.execv(cargo, [cargo, "build"] + sys.argv[2:])
'''


def fake_zigbuild(directory, cargo_zigbuild="0.23.4", zig="0.15.2"):
    """A bin directory holding stand-in cargo-zigbuild and zig executables."""
    os.makedirs(directory)
    for name, body in (("cargo-zigbuild", "VERSION = %r\n" % cargo_zigbuild
                        + FAKE_CARGO_ZIGBUILD),
                       ("zig", "print(%r)\n" % zig)):
        path = os.path.join(directory, name)
        with open(path, "w") as stream:
            stream.write("#!%s -I\n%s" % (sys.executable, body))
        os.chmod(path, 0o755)
    return directory


def fake_toolchain(directory, host="aarch64-apple-darwin"):
    """A bin directory holding the stand-in rustc, cargo and ld."""
    os.makedirs(directory)
    for name, body in (("rustc", "HOST = %r\n" % host + FAKE_RUSTC),
                       ("cargo", "ALLOWED = %r\n" % (ALLOWED,) + FAKE_CARGO),
                       ("ld", "print('@(#)PROGRAM:ld PROJECT:fixture')\n")):
        path = os.path.join(directory, name)
        with open(path, "w") as stream:
            stream.write("#!%s -I\n%s" % (sys.executable, body))
        os.chmod(path, 0o755)
    return directory


PROBE_V2 = ('// authority probe v2\n// requires: answer_v2\n'
            '#[path = "perf-probe/helper.rs"]\nmod helper;\n')
HELPER = '// authority helper module\n'
PROBE_V1 = '// authority probe v1, the older public API\n// requires: answer\n'

ADAPTERS = '''schema = "af.perf-adapters/2"
description = "Fixture adapters."

[adapters.fixture_probe_v2]
kind = "native_probe"
description = "The current public API."
priority = 1
equivalence_group = "fixture_group_v2"
min_version = "0.12.0-rc.3"
package = "review-pipeline"
example = "perf-probe"
contracts = ["sentinel_canonical_v1"]
closure = ["crates/review-pipeline/examples/perf-probe.rs", "crates/review-pipeline/examples/perf-probe/helper.rs"]

[adapters.fixture_probe_v1]
kind = "native_probe"
description = "An older public API bridge."
priority = 2
equivalence_group = "fixture_group_v1"
min_version = "0.12.0-rc.3"
package = "review-pipeline"
example = "perf-probe-v1"
contracts = ["sentinel_canonical_v1"]
closure = ["crates/review-pipeline/examples/perf-probe-v1.rs"]
'''

BASE_FILES = {
    ".gitignore": "/target\n",
    "Cargo.toml": '[workspace]\nmembers = ["crates/*"]\n',
    "Cargo.lock": "# fixture lock\nversion = 4\n",
    "README": "fixture product\n",
    "crates/af/src/main.rs": "fn main() { /* product v1 */ }\n",
    "crates/review-process/src/lib.rs": "pub fn supervise() {}\n",
    "crates/review-process/examples/perf-supervisor.rs": "// supervisor v1\n",
    "crates/review-pipeline/src/lib.rs": "pub fn answer() -> u32 { 42 }\n",
    "crates/review-pipeline/examples/perf-probe.rs": "// the source's own old probe\n",
}


def fixture_repo(path):
    """(repo, base, head, authority): head adds `answer_v2` and edits its own probe copy; the
    authority, a descendant of head, holds the reviewed adapters."""
    repo = Repo(path)
    base = repo.commit("base", BASE_FILES)
    head = repo.commit("head", {
        "crates/review-pipeline/src/lib.rs": ("pub fn answer() -> u32 { 42 }\n"
                                              "pub fn answer_v2() -> u32 { 43 }\n"),
        "crates/review-pipeline/examples/perf-probe.rs": "// CANDIDATE-EDITED probe\n",
        "crates/af/src/main.rs": "fn main() { /* product v2 */ }\n"})
    authority = repo.commit("authority", {
        "crates/review-pipeline/examples/perf-probe.rs": PROBE_V2,
        "crates/review-pipeline/examples/perf-probe/helper.rs": HELPER,
        "crates/review-pipeline/examples/perf-probe-v1.rs": PROBE_V1,
        "fixtures/performance/adapters.toml": ADAPTERS})
    return repo, base, head, authority


def lock_digest(repo, sha):
    return sha256_bytes(repo.git("show", "%s:Cargo.lock" % sha).encode("utf-8") + b"\n")


def plan_for(repo, baseline, candidate, authority, run_id="fixture_build_run",
             recipe_edit=None, tier="nightly", environment="macos_arm64_local",
             build_image=None, trusted_host_build=False, diagnostic_recipe=None):
    """A frozen plan over real fixture commits, by default on the macOS local profile; a
    Linux recipe without its own pin is frozen with `build_image` or `trusted_host_build`."""
    loaded = suite_module.load(SUITE)
    profile = [item for item in loaded.doc["tiers"][tier]["profiles"]
               if item["environment"] == environment][0]
    if recipe_edit is not None:
        recipe_edit(loaded.doc["recipes"][profile["recipe"]])
    scope = "partial_scope" if diagnostic_recipe is None else "diagnostic"
    request = planning.Request(trigger="manual", tier=tier, scope=scope,
                               base=baseline, head=candidate, run_id=run_id, seed=3,
                               cases=["canonical_serialize_small" if diagnostic_recipe is None
                                      else "canonical_serialize_small__package_small_envelope"],
                               environment=environment,
                               build_image=build_image, trusted_host_build=trusted_host_build)
    ids = planning.Identities()
    ids.baseline, ids.candidate, ids.control = baseline, candidate, baseline
    ids.trigger = {"kind": "manual", "remeasure_of": None}
    digests = {"suite_sha256": loaded.raw_sha256, "policy_digest": loaded.policy_digest,
               "instrument_digest": sha256_bytes(b"fixture instrument"),
               "fixture_digest": sha256_bytes(b"fixture fixtures")}
    inputs = {"authority": {"cargo_lock_sha256": lock_digest(repo, authority)},
              "baseline": {"cargo_lock_sha256": lock_digest(repo, baseline)},
              "candidate": {"cargo_lock_sha256": lock_digest(repo, candidate)}}
    # A diagnostic child (ADR-0174) of a parent this fixture names; its recipe is frozen too,
    # `(recipe_id, suite-shaped recipe, frozen identity)` (ADR-0179).
    diagnostic = None if diagnostic_recipe is None else {
        "parent_run_id": "fixture_parent_run", "parent_plan_digest": sha256_bytes(b"parent"),
        "duration_ms": 10000, "frequency_hz": 997, "recipe": diagnostic_recipe[2]}
    plan = planning.freeze(loaded, request, ids, "control", authority, None, digests, [],
                           inputs, diagnostic=diagnostic,
                           recipe=None if diagnostic_recipe is None else diagnostic_recipe[:2])
    return contracts.validate_run(plan)


FAKE_DOCKER = r'''
import json, os, subprocess, sys
# A stand-in docker client for build containers: `image inspect` answers for IMAGES, `run`
# executes the entrypoint on this host with exactly the --env variables in --workdir, and every
# invocation is logged beside this file. `ps` lists the names in `lingering`, so a test can make
# a removal unconfirmed.
here = os.path.dirname(os.path.abspath(sys.argv[0]))
args = sys.argv[1:]
with open(os.path.join(here, "docker-calls.jsonl"), "a") as log:
    log.write(json.dumps(args) + "\n")
if args[:2] == ["image", "inspect"]:
    image = args[-1]
    if image not in IMAGES:
        sys.stderr.write("Error: No such image: %s\n" % image)
        sys.exit(1)
    if args[args.index("--format") + 1] == "{{json .Config.Volumes}}":
        volumes = os.path.join(here, "volumes")
        print(open(volumes).read() if os.path.exists(volumes) else "null")
        sys.exit(0)
    identity = os.path.join(here, "identity")
    if os.path.exists(identity):
        sys.stdout.buffer.write(open(identity, "rb").read())
        sys.exit(0)
    identity, env = IMAGES[image][:2]
    digests = IMAGES[image][2] if len(IMAGES[image]) > 2 else []
    print("\t".join(json.dumps(value) for value in (identity, "linux", "arm64", digests, env)))
    sys.exit(0)
if args[0] == "rm":
    sys.exit(0)
if args[0] == "ps":
    name = args[-1][len("name=^/"):-1]
    lingering = os.path.join(here, "lingering")
    if os.path.exists(lingering) and name.startswith(open(lingering).read().strip()):
        print("0123456789ab")
    sys.exit(0)
assert args[0] == "run", args
env, mounts, cwd, index = {}, [], "/", 1
while index < len(args):
    flag = args[index]
    if flag == "--entrypoint":
        program = args[index + 1]
        image = args[index + 2]
        rest = args[index + 3:]
        break
    if flag in ("--read-only", "--rm", "-i"):
        index += 1
        continue
    value = args[index + 1]
    if flag == "--env":
        key, _, item = value.partition("=")
        env[key] = item
    elif flag == "--workdir":
        cwd = value
    index += 2
image_env = dict(item.split("=", 1) for item in IMAGES[image][1])
for key, value in image_env.items():
    env.setdefault(key, value)
if "AF_PERF_SAMPLE_PROBE=1" in rest:
    # The sample image's runtime and containment probe: what a contained probe of a usable
    # image answers, or the answer (exit code, then stdout) the test wrote in `sample-probe`.
    answer = os.path.join(here, "sample-probe")
    if os.path.exists(answer):
        code, _, text = open(answer).read().partition("\n")
        sys.stdout.write(text)
        sys.exit(int(code))
    print(json.dumps({"uid": os.getuid(), "gid": os.getgid(), "cap_eff": "0000000000000000",
                      "no_new_privs": "1", "networks": ["lo"], "cgroup2": True,
                      "root_writable": False, "modules": True, "shell": True,
                      "python": "3.12.3"}))
    sys.exit(0)
os.chdir(cwd)
if "/" not in program:
    for directory in env.get("PATH", "").split(":"):
        if os.access(os.path.join(directory, program), os.X_OK):
            program = os.path.join(directory, program)
            break
os.execve(program, [program] + rest, env)
'''


def fake_docker(directory, images, lingering=None, volumes=None, identity=None,
                sample_probe=None):
    """A bin directory whose `docker` answers for `images` ({reference: (id, [env]) or
    (id, [env], [repository digests])}); `volumes` is the stdout of every image's volume
    inspection (`null` when None) and `identity` (text, or bytes written exactly), when given,
    of every image's identity inspection. `sample_probe` (exit code, stdout), when given, is
    the sample image's runtime and containment probe's answer; a usable image's otherwise."""
    os.makedirs(directory)
    if sample_probe is not None:
        with open(os.path.join(directory, "sample-probe"), "w") as stream:
            stream.write("%d\n%s" % sample_probe)
    path = os.path.join(directory, "docker")
    with open(path, "w") as stream:
        stream.write("#!%s -I\nIMAGES = %r\n%s" % (sys.executable, images, FAKE_DOCKER))
    os.chmod(path, 0o755)
    if volumes is not None:
        with open(os.path.join(directory, "volumes"), "w") as stream:
            stream.write(volumes)
    if identity is not None:
        with open(os.path.join(directory, "identity"), "wb") as stream:
            stream.write(identity if isinstance(identity, bytes) else identity.encode())
    if lingering is not None:
        with open(os.path.join(directory, "lingering"), "w") as stream:
            stream.write(lingering)
    return path


def docker_calls(directory):
    """Every logged invocation of the stand-in docker client in `directory`."""
    path = os.path.join(directory, "docker-calls.jsonl")
    if not os.path.exists(path):
        return []
    with open(path) as stream:
        return [json.loads(line) for line in stream]
