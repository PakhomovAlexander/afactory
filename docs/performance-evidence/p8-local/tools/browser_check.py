"""Open one offline report page in headless Chrome over the DevTools pipe and record what the page
did: its first section, the filter over every searchable row, the disclosures, console and CSP
messages, and, separately, how the browser shut down. Usage:
browser_check.py CHROME PAGE PROFILE_DIR OUTPUT_JSON
"""
import json
import os
import subprocess
import sys
import time

CHROME, PAGE, PROFILE, OUTPUT = sys.argv[1:5]


class Pipe:
    def __init__(self):
        to_chrome_r, self.to_chrome = os.pipe()
        self.from_chrome, from_chrome_w = os.pipe()
        os.set_inheritable(to_chrome_r, True)
        os.set_inheritable(from_chrome_w, True)

        def fds():
            os.dup2(to_chrome_r, 3)
            os.dup2(from_chrome_w, 4)
        self.process = subprocess.Popen(
            [CHROME, "--headless=new", "--remote-debugging-pipe", "--user-data-dir=" + PROFILE,
             "--no-first-run", "--no-default-browser-check", "--disable-extensions",
             "--disable-background-networking", "--disable-sync", "--metrics-recording-only",
             "about:blank"],
            preexec_fn=fds, close_fds=False, stdout=subprocess.DEVNULL,
            stderr=open(os.path.join(PROFILE + "-stderr.log"), "wb"))
        os.close(to_chrome_r)
        os.close(from_chrome_w)
        self.buffer = b""
        self.next_id = 0
        self.events = []

    def send(self, method, params=None, session=None):
        self.next_id += 1
        message = {"id": self.next_id, "method": method, "params": params or {}}
        if session:
            message["sessionId"] = session
        os.write(self.to_chrome, json.dumps(message).encode() + b"\0")
        return self.wait(lambda item: item.get("id") == self.next_id)

    def wait(self, match, timeout=30):
        deadline = time.time() + timeout
        while True:
            while b"\0" in self.buffer:
                raw, self.buffer = self.buffer.split(b"\0", 1)
                item = json.loads(raw)
                if "method" in item:
                    self.events.append(item)
                if match(item):
                    if "error" in item:
                        raise RuntimeError(item["error"])
                    return item
            if time.time() > deadline:
                raise TimeoutError("no DevTools reply")
            chunk = os.read(self.from_chrome, 1 << 16)
            if not chunk:
                raise EOFError("DevTools pipe closed")
            self.buffer += chunk


EVALUATE = r"""
(function () {
  function visible() {
    var rows = document.querySelectorAll("table.searchable tbody tr");
    var shown = 0;
    for (var i = 0; i < rows.length; i++) { if (!rows[i].hidden) { shown++; } }
    return [rows.length, shown];
  }
  var input = document.getElementById("search");
  var before = visible();
  input.value = "no-such-text-p8-zz";
  input.dispatchEvent(new Event("input"));
  var filtered = visible();
  input.value = "";
  input.dispatchEvent(new Event("input"));
  var cleared = visible();
  input.value = "cli_discovery";
  input.dispatchEvent(new Event("input"));
  var selected = visible();
  input.value = "";
  input.dispatchEvent(new Event("input"));
  var details = document.querySelectorAll("details");
  var closed = 0;
  for (var j = 0; j < details.length; j++) { if (!details[j].open) { closed++; } }
  var opened = [];
  for (var k = 0; k < details.length; k++) {
    details[k].querySelector("summary").click();
    opened.push(details[k].open);
  }
  var links = [];
  document.querySelectorAll("a[href]").forEach(function (a) { links.push(a.getAttribute("href")); });
  var images = [];
  document.querySelectorAll("img[src]").forEach(function (i) { images.push([i.getAttribute("src"), i.complete && i.naturalWidth > 0]); });
  return JSON.stringify({
    title: document.title,
    sections: Array.prototype.map.call(document.querySelectorAll("section > h2"), function (h) { return h.textContent; }),
    first_section: document.querySelector("section").textContent.slice(0, 2000),
    rows_before: before, rows_filtered_absent: filtered, rows_cleared: cleared,
    rows_filtered_cli_discovery: selected,
    details: details.length, details_closed_at_load: closed, details_open_after_click: opened,
    links: links, images: images
  });
}())
"""


def main():
    result = {"chrome": CHROME, "page": PAGE}
    pipe = Pipe()
    try:
        version = pipe.send("Browser.getVersion")["result"]
        result["browser"] = version.get("product")
        target = pipe.send("Target.createTarget", {"url": "about:blank"})["result"]["targetId"]
        session = pipe.send("Target.attachToTarget", {"targetId": target, "flatten": True}
                            )["result"]["sessionId"]
        for method in ("Page.enable", "Runtime.enable", "Log.enable"):
            pipe.send(method, session=session)
        pipe.send("Page.navigate", {"url": "file://" + os.path.abspath(PAGE)}, session=session)
        pipe.wait(lambda item: item.get("method") == "Page.loadEventFired"
                  and item.get("sessionId") == session)
        value = pipe.send("Runtime.evaluate", {"expression": EVALUATE, "returnByValue": True},
                          session=session)["result"]
        result["page_interaction"] = json.loads(value["result"]["value"])
        result["page_interaction_status"] = "completed"
        time.sleep(0.5)
        pipe.send("Runtime.evaluate", {"expression": "1"}, session=session)
        result["console_and_csp_messages"] = [
            item["params"] for item in pipe.events
            if item.get("method") in ("Log.entryAdded", "Runtime.consoleAPICalled",
                                      "Runtime.exceptionThrown")]
    except Exception as error:  # recorded, never hidden
        result["page_interaction_status"] = "failed: %r" % (error,)
    # Shutdown is a separate outcome from the page interaction.
    shutdown = {"requested": "Browser.close"}
    try:
        os.write(pipe.to_chrome, json.dumps({"id": 999999, "method": "Browser.close"}).encode()
                 + b"\0")
    except OSError as error:
        shutdown["request_error"] = repr(error)
    started = time.time()
    try:
        shutdown["exit_code"] = pipe.process.wait(timeout=30)
        shutdown["seconds"] = round(time.time() - started, 3)
    except subprocess.TimeoutExpired:
        pipe.process.kill()
        shutdown["exit_code"] = None
        shutdown["outcome"] = "did not exit within 30 s; killed"
        shutdown["after_kill_exit_code"] = pipe.process.wait(timeout=30)
    result["browser_shutdown"] = shutdown
    with open(OUTPUT, "w") as stream:
        json.dump(result, stream, indent=2, sort_keys=True)
        stream.write("\n")
    return 0 if result["page_interaction_status"] == "completed" else 1


if __name__ == "__main__":
    sys.exit(main())
