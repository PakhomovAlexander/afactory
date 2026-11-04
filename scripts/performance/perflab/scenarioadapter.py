"""Trusted, token-free bridge to the native executor; this module never launches compute.

The stdin entry point only reserves an immutable intent. An independently supervised service
calls `submit_pending`; the ordinary executor service calls Journal.cycle. No detached child,
shell, environment grant, alternate benchmark executor, machine lock or fence lives here.
"""
import contextlib
import fcntl
import os
import stat
import time

from . import contracts, executor, researchevidence, scenariosource
from .scenariocontracts import CAPTURE_RESERVE, MAX_REQUEST
from .values import ContractError, canonical_digest, document_bytes, loads_strict, require



def load_config(path):
    info = os.lstat(path)
    require(stat.S_ISREG(info.st_mode) and info.st_uid in (0, os.geteuid())
            and not info.st_mode & 0o022, "unsafe installed Scenario adapter configuration")
    return contracts.read_json(executor.read(path), "scenario-adapter")


class Adapter:
    def __init__(self, config):
        contracts.CONTRACTS["scenario-adapter"][2](config)
        self.config, self.root = config, config["root"]
        executor.private_directory(self.root)
        self.native_config = executor.load_config(config["executor"])
        require(config["max_bytes"] >= CAPTURE_RESERVE + MAX_REQUEST,
                "Scenario adapter allowance cannot fit its native source capture reservation")
        native_root = self.native_config["root"]
        executor.private_directory(native_root)
        for root in (self.root, config["evidence_root"]):
            executor.private_directory(root)
            require(os.path.commonpath([root, native_root]) == native_root and root != native_root,
                    "Scenario spool and evidence must be below the existing native journal budget")
            ancestor = root
            while True:
                require(not os.path.lexists(os.path.join(ancestor, ".git"))
                        and not (os.path.isfile(os.path.join(ancestor, "HEAD"))
                                 and os.path.isdir(os.path.join(ancestor, "objects"))),
                        "Scenario spool/evidence must be outside every repository")
                parent = os.path.dirname(ancestor)
                if parent == ancestor:
                    break
                ancestor = parent
        require(os.path.commonpath([self.root, config["evidence_root"]]) not in (self.root, config["evidence_root"]),
                "Scenario spool and evidence roots must be disjoint")

    def capacity(self, reserve):
        require(executor.tree_usage(self.native_config["root"]) + reserve <= self.native_config["max_bytes"],
                "native journal byte capacity exhausted; retain Scenario receipts")
        require(executor.tree_usage(self.root) + executor.tree_usage(self.config["evidence_root"])
                + reserve <= self.config["max_bytes"], "Scenario spool/evidence byte capacity exhausted")

    @contextlib.contextmanager
    def writer(self):
        fd = os.open(os.path.join(self.root, "adapter.lock"),
                     os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
        try:
            info = os.fstat(fd)
            require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1
                    and info.st_uid == os.geteuid() and not info.st_mode & 0o077,
                    "unsafe Scenario submission lock")
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as error:
                raise executor.JournalBusy("Scenario submission service busy; reconcile later") from error
            yield
        finally:
            os.close(fd)

    def path(self, intent):
        contracts.DIGEST.validate(intent, "intent")
        return os.path.join(self.root, intent[7:])

    def names(self):
        names = sorted(n for n in os.listdir(self.root) if n != "adapter.lock")
        require(len(names) <= self.config["max_intents"], "Scenario submission capacity exceeded")
        for name in names:
            contracts.DIGEST.validate("sha256:" + name, "intent directory")
            executor.private_directory(os.path.join(self.root, name))
        return names

    def held(self, ident):
        path = self.path(ident)
        executor.private_directory(path)
        # A torn directory is uncertain, never absent and never an invitation to re-execute.
        require(not any(n.endswith(".pending") for n in os.listdir(path)),
                "torn Scenario submission; operator inspection required")
        doc = contracts.read_json(executor.read(os.path.join(path, "intent.json")), "scenario-intent")
        require(doc["intent_id"] == ident and doc["destination"] == self.config["destination"],
                "Scenario submission identity or destination changed")
        return doc

    def request(self, request):
        require(isinstance(request, dict) and set(request) == {
            "schema", "method", "intent_id", "input_digest", "operation", "destination", "inputs"},
            "invalid Scenario adapter request fields")
        require(request["schema"] == "af/scenario-adapter-request@2"
                and request["destination"] == self.config["destination"],
                "unsupported Scenario adapter version or destination")
        for key in ("intent_id", "input_digest"):
            contracts.DIGEST.validate(request[key], key)
        response = dict(schema="af/scenario-adapter-response@1", intent_id=request["intent_id"],
                        input_digest=request["input_digest"], outcome="pending",
                        evidence="submission requires reconciliation", observation=None)
        method, inputs = request["method"], request["inputs"]
        require(isinstance(inputs, dict), "inputs must be typed")

        def text(key):
            item = inputs[key]
            require(isinstance(item, dict) and set(item) == {"type", "value"}
                    and item["type"] == "text" and isinstance(item["value"], str),
                    "expected typed text " + key)
            return item["value"]

        if method == "observe":
            try:
                with self.writer():
                    return self.observation_request(request, response, text)
            except executor.JournalBusy:
                response.update(evidence="native submission/verification service busy; observe later")
                return response
            except FileNotFoundError:
                response.update(evidence="native receipt unavailable; no completion claim")
                return response
            except ContractError as error:
                if "being published" in str(error) or "torn Scenario submission" in str(error):
                    response.update(evidence=str(error)[:4096])
                else:
                    response.update(outcome="failed", evidence=str(error)[:4096])
                return response

        require(method in ("reconcile", "execute") and request["operation"] == "experiment_submit_v1"
                and set(inputs) == {"subject", "revision", "body", "candidate"}, "unauthorized lab operation")
        experiment = contracts.read_json(text("body").encode(), "scenario-experiment")
        native_config = executor.load_config(self.config["executor"])
        require(canonical_digest(native_config) == experiment["configuration_digest"],
                "experiment configuration differs from installed authority")
        try:
            source_request = scenariosource.create_request(inputs["candidate"], text("revision"), text("subject"), native_config)
        except ContractError as error:
            # A pure unsupported-source refusal with no prior intent has no external effect.
            # A conflicting key that may already own work remains uncertain instead.
            if os.path.lexists(self.path(request["intent_id"])):
                raise
            response.update(outcome="failed", evidence=str(error)[:4096])
            return response
        intent = dict(schema="af.perf-scenario-intent/1", intent_id=request["intent_id"],
                      input_digest=request["input_digest"], destination=request["destination"],
                      subject=text("subject"), revision=text("revision"),
                      source_request_digest=canonical_digest(source_request),
                      created_unix_ms=int(time.time() * 1000), experiment=experiment)
        contracts.CONTRACTS["scenario-intent"][2](intent)
        with self.writer():
            path = self.path(intent["intent_id"])
            if os.path.lexists(path):
                held = self.held(intent["intent_id"])
                require(all(held[k] == intent[k] for k in intent if k != "created_unix_ms"),
                        "submission intent reused with changed input, source or configuration")
            elif method == "reconcile":
                response.update(outcome="absent", evidence="no durable submission for this exact intent")
                return response
            else:
                require(len(self.names()) < self.config["max_intents"], "Scenario submission queue is full")
                require(executor.tree_usage(self.root) + MAX_REQUEST <= self.config["max_bytes"],
                        "Scenario submission byte capacity exhausted; retain all receipts")
                self.capacity(MAX_REQUEST)
                os.mkdir(path, 0o700)
                executor.sync_dir(self.root)
                scenariosource.save_request(os.path.join(path, "source-request.json"), source_request)
                executor.atomic(os.path.join(path, "intent.json"), intent, "scenario-intent")
        # Done means submitted durably, never measured. Only a separate typed Wait can complete.
        response.update(outcome="done", evidence="durable experiment submission " + intent["intent_id"])
        return response

    def observation_request(self, request, response, text):
        inputs = request["inputs"]
        require(request["operation"] == "observe" and set(inputs) == {"subject", "revision", "condition"}
                and text("condition") in ("native_verified", "bounded_evidence"), "unauthorized lab observation")
        subject, revision = text("subject"), text("revision")
        if text("condition") == "bounded_evidence":
            index = contracts.read_json(executor.read(self.config["evidence_index"]), "evidence-index")
            require(canonical_digest(index) == self.config["evidence_index_digest"] == subject,
                    "research history index differs from pinned source selection")
            now = int(time.time() * 1000)
            packet = researchevidence.packet(index, now, now, revision,
                self.config["evidence_max_age_ms"], self.config["evidence_max_items"],
                self.config["evidence_max_bytes"])
            researchevidence.authenticate(packet, self.config)
            self.capacity(self.config["evidence_max_bytes"])
            researchevidence.retain(self.config["evidence_root"], packet, "evidence-packet")
            response.update(outcome="done", evidence="retained bounded research packet " + canonical_digest(packet),
                observation=dict(subject=subject, revision=revision, condition="bounded_evidence",
                                 satisfied=True, evidence=document_bytes(packet).decode()))
            return response
        intent = self.held(subject)
        require(intent["revision"] == revision, "changed experiment source; observation is stale")
        state, detail = self.observe(intent)
        response.update(outcome=state, evidence=detail,
            observation=dict(subject=subject, revision=revision, condition="native_verified",
                             satisfied=state == "done", evidence=detail))
        return response


    def submit_pending(self, capture=scenariosource.capture):
        """One bounded pass. Native preflight and locking are unchanged; no compute is run.

        The native submission key contains the durable intent AND exact input digest. A crash
        after Journal.submit but before our receipt reconciles the same native original.
        """
        with self.writer():
            for name in self.names():
                intent = self.held("sha256:" + name)
                path = self.path(intent["intent_id"])
                if os.path.exists(os.path.join(path, "receipt.json")):
                    continue
                status_path = os.path.join(path, "status.json")
                if os.path.exists(status_path):
                    status = contracts.read_json(executor.read(status_path), "scenario-status")
                    require(status["intent_digest"] == canonical_digest(intent), "stale service status")
                    if status["state"] == "refused":
                        continue
                try:
                    self.submit_one(intent, path, capture)
                except executor.JournalBusy:
                    return "busy"
                except ContractError as error:
                    config = executor.load_config(self.config["executor"])
                    key = intent["intent_id"] + ":" + intent["input_digest"]
                    ident = "exp_" + canonical_digest({"submission": {"kind": "manual", "key": key},
                                                       "work": intent["experiment"]["work"]})[7:55]
                    uncertain = (isinstance(error, scenariosource.UncertainCapture)
                                 or os.path.lexists(os.path.join(config["root"], "requests", ident)))
                    executor.atomic(status_path, dict(schema="af.perf-scenario-status/1",
                        intent_digest=canonical_digest(intent), state="uncertain" if uncertain else "refused",
                        detail=str(error)[:4096]), "scenario-status")
                    return "uncertain" if uncertain else "refused"
                return "submitted"
        return "idle"

    def submit_one(self, intent, path, capture):
        experiment = intent["experiment"]
        config = executor.load_config(self.config["executor"])
        require(config["backend"] == "local", "Scenario adapter requires the native local backend")
        require(canonical_digest(config) == experiment["configuration_digest"],
                "experiment configuration changed before native submission")
        with executor.Journal(config) as journal:
            self.capacity(CAPTURE_RESERVE)
            journal.capacity(CAPTURE_RESERVE, "accepted Snapshot capture")
            # Use the native exclusion/fence, including for heavy source materialization.
            # A killed capture leaves the same fence as other uncertain native preparation.
            def handoff():
                try:
                    return capture(self.config, path, intent["source_request_digest"]), None, False
                except scenariosource.CleanCaptureRefusal as error:
                    return None, str(error), False
            try:
                source, refusal = journal.exclusive("accepted Snapshot handoff", handoff)
            except executor.JournalBusy:
                raise
            except (ContractError, OSError) as error:
                raise scenariosource.UncertainCapture(str(error)) from error
            require(refusal is None, "native Snapshot handoff refused: " + str(refusal))
            self.capacity(0)
            config = dict(config, repository_path=source["repository"])
            journal.config = config
            native = journal.submit(experiment["work"], source["baseline_sha"],
                source["candidate_sha"], intent["intent_id"] + ":" + intent["input_digest"],
                bind_configuration=True, expected_producer=experiment["producer_digest"])
            require(native["producer_digest"] == experiment["producer_digest"],
                    "native producer differs from pinned experiment producer")
            self.check_native(intent, native, source, config)
            receipt = dict(schema="af.perf-scenario-receipt/1", intent_digest=canonical_digest(intent),
                           request_id=native["request_id"], request_digest=canonical_digest(native))
            executor.atomic(os.path.join(path, "receipt.json"), receipt, "scenario-receipt")

    @staticmethod
    def check_native(intent, native, source, config):
        experiment = intent["experiment"]
        require(native["backend"] == "local"
                and source["request_digest"] == intent["source_request_digest"]
                and source["candidate_snapshot_id"] == intent["revision"]
                and native["sources"]["baseline_sha"] == source["baseline_sha"]
                and native["sources"]["candidate_sha"] == source["candidate_sha"]
                and native["configuration_digest"] == canonical_digest(config)
                and native["producer_digest"] == experiment["producer_digest"]
                and native["work"] == experiment["work"]
                and native["submission"] == {"kind": "manual",
                    "key": intent["intent_id"] + ":" + intent["input_digest"]},
                "native request does not match exact experiment authority")

    def observe(self, intent):
        path = self.path(intent["intent_id"])
        status_path = os.path.join(path, "status.json")
        if os.path.exists(status_path):
            status = contracts.read_json(executor.read(status_path), "scenario-status")
            require(status["intent_digest"] == canonical_digest(intent), "stale experiment service status")
            if status["state"] == "refused":
                return "failed", status["detail"]
        receipt_path = os.path.join(path, "receipt.json")
        if not os.path.exists(receipt_path):
            return "pending", "durable submission awaits native preflight; no measurement claim"
        receipt = contracts.read_json(executor.read(receipt_path), "scenario-receipt")
        require(receipt["intent_digest"] == canonical_digest(intent), "stale native submission receipt")
        config = executor.load_config(self.config["executor"])
        require(canonical_digest(config) == intent["experiment"]["configuration_digest"],
                "installed executor configuration changed; evidence not reused")
        source = loads_strict(executor.read(os.path.join(path, "source/source.json")), "native source receipt", 65536)
        config = dict(config, repository_path=source["repository"])
        with executor.Journal(config, readonly=True) as journal:
            native = journal.request(receipt["request_id"])
            self.check_native(intent, native, source, config)
            require(canonical_digest(native) == receipt["request_digest"], "native request changed")
            state = journal.state(receipt["request_id"])
            if state["state"] in ("failed", "interrupted", "expired"):
                return "failed", "native experiment " + state["state"] + "; " + state["detail"][:2048]
            if state["state"] != "complete":
                return "pending", "native experiment " + state["state"] + "; never replay a claim"
            # Verification is performed by the separately supervised service, not the bounded
            # observer. Bind its retained certificate to the native result's current digest.
            verified = os.path.join(path, "verification.json")
            if not os.path.exists(verified):
                return "pending", "native result awaits offline report verification"
            verification = contracts.read_json(executor.read(verified), "scenario-verification")
            require(verification["receipt_digest"] == canonical_digest(receipt)
                    and verification["result_sha256"] == state["result_sha256"],
                    "stale native verification certificate")
            if verification["measurement_status"] not in ("pass", "fail", "complete"):
                return "failed", "native verified report is incomplete; no improvement claim"
            completion = verification["completion"]
            require(completion["request"] == native and completion["result_sha256"] == state["result_sha256"]
                    and completion["experiment"] == intent["experiment"]
                    and completion["source_request_digest"] == intent["source_request_digest"],
                    "completion differs from native request, result or source handoff")
            verdict = completion["verdict"]
            if native["work"] == "core" and (verdict is None or verdict["provenance"] != "measured"
                    or verdict["aggregate"]["result"] != verification["measurement_status"]):
                return "failed", "native report contains no measured matched timing evidence"
            return "done", document_bytes(completion).decode()

    def verify_pending(self, verifier=executor.verify_original):
        """Offline report verification. Native exit status and HTML names never settle a Wait."""
        with self.writer():
            config = executor.load_config(self.config["executor"])
            with executor.Journal(config) as journal:
                for name in self.names():
                    intent = self.held("sha256:" + name)
                    path = self.path(intent["intent_id"])
                    receipt_path = os.path.join(path, "receipt.json")
                    if not os.path.exists(receipt_path):
                        continue
                    receipt = contracts.read_json(executor.read(receipt_path), "scenario-receipt")
                    require(receipt["intent_digest"] == canonical_digest(intent), "stale intent receipt")
                    status_path = os.path.join(path, "status.json")
                    if os.path.exists(status_path):
                        status = contracts.read_json(executor.read(status_path), "scenario-status")
                        require(status["intent_digest"] == canonical_digest(intent), "stale verification status")
                        if status["state"] == "refused":
                            continue
                    native = journal.request(receipt["request_id"])
                    require(canonical_digest(config) == intent["experiment"]["configuration_digest"],
                            "installed executor configuration changed")
                    source = loads_strict(executor.read(os.path.join(path, "source/source.json")), "native source receipt", 65536)
                    frozen = dict(config, repository_path=source["repository"])
                    self.check_native(intent, native, source, frozen)
                    require(canonical_digest(native) == receipt["request_digest"], "changed native request")
                    state = journal.state(receipt["request_id"])
                    if state["state"] != "complete":
                        continue
                    certificate_path = os.path.join(path, "verification.json")
                    if os.path.exists(certificate_path):
                        held = contracts.read_json(executor.read(certificate_path), "scenario-verification")
                        require(held["receipt_digest"] == canonical_digest(receipt)
                                and held["result_sha256"] == state["result_sha256"],
                                "existing native verification certificate changed")
                        continue
                    evaluated = {}
                    def check():
                        try:
                            native, state, result = verifier(journal, receipt["request_id"], evaluated=evaluated)
                            self.check_native(intent, native, source, frozen)
                            verdict = None
                            if native["work"] == "core":
                                verdict = evaluated[receipt["request_id"]].verdict
                                require(verdict["aggregate"]["result"] == result["measurement_status"],
                                        "native verdict differs from result status")
                            completion = dict(schema="af.perf-scenario-completion/1", request=native,
                                experiment=intent["experiment"], result_sha256=state["result_sha256"],
                                source_request_digest=intent["source_request_digest"],
                                baseline_snapshot_id=source["baseline_snapshot_id"],
                                candidate_snapshot_id=source["candidate_snapshot_id"],
                                verdict=verdict, telemetry_is_diagnostic=True)
                            require(len(document_bytes(completion)) <= 24576,
                                    "verified native report exceeds bounded Scenario evidence; narrow the declared workload")
                            certificate = dict(schema="af.perf-scenario-verification/1",
                                receipt_digest=canonical_digest(receipt), result_sha256=state["result_sha256"],
                                verified_unix_ms=int(time.time() * 1000), measurement_status=result["measurement_status"],
                                completion=completion)
                            contracts.write_bytes(certificate, "scenario-verification")
                            return certificate, None, False
                        except (ContractError, OSError) as error:
                            # Offline verification owns no sample/container. Every definitive
                            # refusal, including the output bound, is durable and skippable.
                            return None, str(error), False
                    certificate, refusal = journal.exclusive("native report verification", check)
                    if refusal is not None:
                        executor.atomic(os.path.join(path, "status.json"), dict(schema="af.perf-scenario-status/1",
                            intent_digest=canonical_digest(intent), state="refused",
                            detail=("offline native verification refused: " + refusal)[:4096]), "scenario-status")
                        return
                    executor.atomic(os.path.join(path, "verification.json"), certificate, "scenario-verification")
                    return
