"""Faults below Running's conversion, over actual Profile and supervisor launches.

Successful requests and their processes are real test stand-ins. At the selected request,
lose the pipe reply after Popen and tool output, then also lose cancellation evidence. No
candidate/Docker process is left live by this fixture; only cleanup evidence is withdrawn.
"""
import contextlib
import json
from unittest import mock

from perflab import supervise


@contextlib.contextmanager
def fault(match, exception=OSError, contained=False, prelaunch=False, callback=False):
    actual_launch, actual_popen = supervise.launch, supervise.subprocess.Popen
    actual_remove = supervise.remove_container
    state = {"requests": [], "faults": [], "popen": [], "removals": []}
    active = [None]

    def launch(argv, request, **kwargs):
        state["requests"].append(request)
        active[0] = request
        if callback and match(request):
            original = kwargs.get("started")
            def started(process):
                if original: original(process)
                state["faults"].append(request)
                raise exception("callback failed after Popen")
            kwargs["started"] = started
        try:
            return actual_launch(argv, request, **kwargs)
        finally:
            active[0] = None

    def popen(*args, **kwargs):
        request = active[0]
        if request is None or not match(request):
            return actual_popen(*args, **kwargs)
        if prelaunch:
            state["faults"].append(request)
            raise OSError("proven Popen refusal")
        process = actual_popen(*args, **kwargs)
        state["popen"].append(process)
        if not callback:
            communicate = process.communicate
            def broken(data=None, timeout=None):
                if data is not None:
                    # Execute the selected actual request and preserve its written paths.
                    json.loads(data)
                    communicate(data, timeout=timeout)
                    state["faults"].append(request)
                raise exception("pipe evidence lost after successful Popen")
            process.communicate = broken
        return process

    def remove(*args, **kwargs):
        result = actual_remove(*args, **kwargs)
        if state["faults"]:
            state["removals"].append(result)
            return "container removal unconfirmed by fault injection"
        return result

    with mock.patch.object(supervise, "launch", launch), \
            mock.patch.object(supervise.subprocess, "Popen", popen), \
            (mock.patch.object(supervise, "remove_container", remove)
             if contained else contextlib.nullcontext()):
        yield state
