#!/usr/bin/env python3
"""Exercise Textloom's web input path in headless Chromium with native browser gestures."""

import argparse
import functools
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import threading
import time
import tomllib
from contextlib import contextmanager
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import HTTPError
from urllib.request import Request, urlopen


HERE = Path(__file__).resolve().parent
TARGET = "wasm32-unknown-unknown"


def packages(lockfile):
    return tomllib.loads(lockfile.read_text())["package"]


def build(args, work, site):
    lock = packages(args.lockfile)
    versions = {p["name"]: p["version"] for p in lock}
    manifest = f"""
[package]
name = "web-touch-probe"
version = "0.0.0"
edition = "2024"
publish = false
[workspace]
[profile.dev]
debug = 0
[lib]
path = "host.rs"
crate-type = ["cdylib"]
[dependencies]
textloom = {{ path = {json.dumps(str(args.package.resolve()))}, features = ["egui"] }}
eframe = {{ path = {json.dumps(str(args.eframe.resolve()))}, default-features = false, features = ["glow", "default_fonts"] }}
wasm-bindgen = "={versions['wasm-bindgen']}"
wasm-bindgen-futures = "={versions['wasm-bindgen-futures']}"
web-sys = {{ version = "={versions['web-sys']}", features = ["HtmlCanvasElement"] }}
"""
    local = {"textloom": args.package, "eframe": args.eframe}
    if args.egui:
        local["egui"] = args.egui
        manifest += f"\n[patch.crates-io]\negui = {{ path = {json.dumps(str(args.egui.resolve()))} }}\n"
    (work / "Cargo.toml").write_text(manifest)
    shutil.copy(HERE / "host.rs", work)
    shutil.copy(args.lockfile, work / "Cargo.lock")
    target = Path(os.getenv("CARGO_TARGET_DIR", work / "target")).resolve()
    # Cargo prunes unused native dependencies and adds the probe as the new root.
    graph = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--filter-platform", TARGET], cwd=work,
    ))
    resolved = packages(work / "Cargo.lock")
    for name, directory in local.items():
        sources = [p.get("source") for p in resolved if p["name"] == name]
        manifests = [Path(p["manifest_path"]).resolve() for p in graph["packages"] if p["name"] == name]
        if sources != [None] or manifests != [(directory / "Cargo.toml").resolve()]:
            raise RuntimeError(f"unexpected {name} resolution: {sources}, {manifests}")
    pinned = {(p["name"], p["version"], p["checksum"]) for p in lock if "checksum" in p}
    for p in resolved:
        if "source" in p and (p["name"], p["version"], p.get("checksum")) not in pinned:
            raise RuntimeError(f"probe dependency absent from supplied lockfile: {p['name']} {p['version']}")
    subprocess.run(["cargo", "build", "--locked", "--target", TARGET, "--target-dir", target],
                   cwd=work, check=True)
    subprocess.run([
        os.getenv("WASM_BINDGEN", "wasm-bindgen"), "--target", "web", "--out-dir", site,
        "--out-name", "host", target / TARGET / "debug/web_touch_probe.wasm",
    ], check=True)
    shutil.copy(HERE / "index.html", site)


@contextmanager
def serve(site):
    class QuietHandler(SimpleHTTPRequestHandler):
        def log_message(self, *args):
            pass

    handler = functools.partial(QuietHandler, directory=site)
    with ThreadingHTTPServer(("127.0.0.1", 0), handler) as server:
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            yield f"http://127.0.0.1:{server.server_port}/"
        finally:
            server.shutdown()
            thread.join()


def request(url, method="GET", payload=None):
    data = None if payload is None else json.dumps(payload).encode()
    req = Request(url, data=data, method=method, headers={"Content-Type": "application/json"})
    try:
        with urlopen(req, timeout=30) as response:
            return json.load(response)["value"]
    except HTTPError as exc:
        raise RuntimeError(f"{method} {url}: {exc.read().decode()}") from exc


@contextmanager
def browser(work):
    name = os.getenv("CHROME")
    chrome = shutil.which(name) if name else shutil.which("chromium") or shutil.which("google-chrome")
    if not chrome:
        raise RuntimeError("set CHROME to a Chromium or Google Chrome executable")
    log = work / "chromedriver.log"
    env = dict(os.environ, XDG_CONFIG_HOME=str(work / "config"), XDG_CACHE_HOME=str(work / "cache"))
    with log.open("w") as output, subprocess.Popen(
        [os.getenv("CHROMEDRIVER", "chromedriver"), "--port=0"], stdout=output, stderr=output, env=env,
    ) as driver:
        session = None
        try:
            for _ in range(100):
                match = re.search(r"started successfully on port (\d+)", log.read_text())
                if match:
                    break
                if driver.poll() is not None:
                    raise RuntimeError(log.read_text())
                time.sleep(0.1)
            else:
                raise RuntimeError("ChromeDriver startup timed out: " + log.read_text())
            address = "http://127.0.0.1:" + match[1]
            options = {"binary": chrome, "args": [
                "--headless", "--remote-debugging-pipe", f"--user-data-dir={work / 'profile'}",
                "--no-first-run", "--disable-background-networking", "--disable-sync",
                "--use-angle=swiftshader", "--enable-unsafe-swiftshader",
                "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1",
            ]}
            answer = request(address + "/session", "POST", {"capabilities": {
                "alwaysMatch": {"browserName": "chrome", "goog:chromeOptions": options},
            }})
            session = address + "/session/" + answer["sessionId"]
            print("Testing Chromium", answer["capabilities"]["browserVersion"], flush=True)
            yield Driver(session)
        finally:
            try:
                if session:
                    request(session, "DELETE")
            finally:
                driver.terminate()


class Driver:
    def __init__(self, session):
        self.session = session

    def post(self, path, payload):
        return request(self.session + path, "POST", payload)

    def cdp(self, command, **params):
        return self.post("/goog/cdp/execute", {"cmd": command, "params": params})

    def script(self, code):
        return self.post("/execute/sync", {"script": code, "args": []})

    def check(self, description, condition):
        deadline = time.monotonic() + 8
        while True:
            data = self.script("return {...window.qa, active: document.activeElement.id}")
            if data and data.get("error"):
                raise RuntimeError(data["error"])
            if data and "selection" in data and condition(data):
                print(f"{description}: focus={data['focused']} find={data['find']} "
                      f"selection={data['selection']} scroll={data['scroll']} "
                      f"primary={data['held']} secondary={data['secondary']}", flush=True)
                return data
            if time.monotonic() >= deadline:
                raise AssertionError(f"{description}: {data}")
            time.sleep(0.05)

    def viewport(self, touch):
        self.cdp("Emulation.setTouchEmulationEnabled", enabled=touch, maxTouchPoints=5)
        self.cdp("Emulation.setDeviceMetricsOverride", width=840, height=700, deviceScaleFactor=1, mobile=False)

    def navigate(self, url):
        self.post("/url", {"url": url})
        self.check("fixture startup", lambda q: q["scroll"] == 0)

    def route_tab(self, url, route):
        tab = self.post("/window/new", {"type": "tab"})
        self.post("/window", {"handle": tab["handle"]})
        self.viewport(route == "touch")
        self.navigate(url)
        self.check(route + " input route", lambda q: q["advertisesTouchEvents"] == (route == "touch"))

    def mouse(self, kind, x, y, held=False, button="left"):
        self.cdp("Input.dispatchMouseEvent", type=kind, x=x, y=y, pointerType="mouse",
                 button="none" if kind == "mouseMoved" else button,
                 buttons=(2 if button == "right" else 1) if held else 0,
                 clickCount=0 if kind == "mouseMoved" else 1)
        time.sleep(0.05)

    def touch(self, kind, *points):
        self.cdp("Input.dispatchTouchEvent", type=kind,
                 touchPoints=[{"id": i, "x": x, "y": y} for i, x, y in points])
        time.sleep(0.06)

    def focus_find(self):
        self.mouse("mouseMoved", 80, 17)
        self.mouse("mousePressed", 80, 17, True)
        self.mouse("mouseReleased", 80, 17)
        return self.check("Find focus", lambda q: q["find"])


def gestures(d, url):
    d.viewport(True)
    d.navigate(url)
    initial = d.focus_find()
    d.touch("touchStart", (3, 130, 534))
    for y in range(514, 333, -20):
        d.touch("touchMove", (3, 130, y))
    # Pause before lifting so kinetic scrolling does not obscure press-only deltas.
    time.sleep(0.2)
    d.touch("touchEnd")
    scrolled = d.check("cold swipe", lambda q: q["scroll"] >= 160 and q["find"]
                       and not q["held"] and q["selection"] == initial["selection"])
    d.mouse("mouseMoved", 75, 60)
    d.check("hover preserves offset", lambda q: q["scroll"] == scrolled["scroll"])
    d.touch("touchStart", (7, 360, 560))
    d.check("press after distant hover", lambda q: q["held"]
            and q["scroll"] == scrolled["scroll"] and q["find"])
    d.touch("touchEnd")

    d.navigate(url + "?hybrid")
    d.touch("touchStart", (4, 90, 150))
    d.touch("touchEnd")
    d.check("touch tap", lambda q: q["focused"])
    d.focus_find()
    d.mouse("mouseMoved", 50, 190)
    d.mouse("mousePressed", 50, 190, True)
    for x in range(60, 141, 10):
        d.mouse("mouseMoved", x, 190, True)
    d.mouse("mouseReleased", 140, 190)
    baseline = d.check("first post-touch mouse drag", lambda q: q["focused"] and
                       not q["held"] and q["selection"] == [7, 6, 7, 22])

    d.navigate(url + "?readonly")
    d.mouse("mouseMoved", 60, 180)
    d.mouse("mousePressed", 60, 180, True, button="right")
    d.check("secondary mouse button held", lambda q: q["secondary"] and not q["held"])
    frame = d.script("return cancelWithinTask()")
    d.check("cancel deferred press", lambda q: q["frames"] >= frame + 2 and
            q["cancels"] == [False] and q["secondary"] and not q["held"] and not q["links"])
    d.touch("touchStart", (8, 42, 55))
    d.check("both buttons held", lambda q: q["held"] and q["secondary"])
    d.touch("touchCancel")
    d.check("native touch cancel preserves secondary", lambda q: q["cancels"] == [False, True]
            and q["secondary"] and not q["held"] and not q["links"])
    d.mouse("mouseReleased", 60, 180, button="right")
    d.check("secondary mouse button released", lambda q: not q["secondary"])
    d.touch("touchStart", (4, 42, 55))
    d.touch("touchEnd")
    time.sleep(0.3)  # Let compatibility mouse events following the tap finish.
    expected = [{"url": "https://example.invalid/qa", "trusted": True}]
    d.check("read-only link tap", lambda q: q["links"] == expected and not q["held"])

    d.navigate(url + "?secondary")
    d.focus_find()
    secondary(d)

    for route in ("pointer", "touch"):
        d.route_tab(url + "?readonly&blur=" + route, route)
        d.mouse("mousePressed", 600, 300, True)
        d.mouse("mouseReleased", 600, 300)
        d.script("document.querySelector('canvas').focus()")
        d.check(route + " canvas focus", lambda q: q["active"] == "editor"
                and q["advertisesTouchEvents"] == (route == "touch"))
        first = (7, 42, 55)
        d.touch("touchStart", first)
        d.touch("touchStart", first, (2, 120, 60))
        d.check(route + " contacts held", lambda q: q["contacts"] == 2 and q["held"])
        d.script("document.querySelector('#outside').focus()")
        d.check(route + " blur releases contacts", lambda q: q["active"] == "outside"
                and not q["contacts"] and not q["held"] and not q["links"])
        d.touch("touchEnd")
        d.check(route + " late end preserves outside focus", lambda q: q["touchUps"] == [True, True]
                and q["active"] == "outside" and not q["contacts"] and not q["held"] and not q["links"])
        d.mouse("mouseMoved", 42, 55)
        d.mouse("mousePressed", 42, 55, True)
        d.check(route + " mouse presses link", lambda q: q["active"] == "editor" and q["held"])
        d.script("document.querySelector('#outside').focus()")
        d.check(route + " mouse blur cancels press", lambda q: q["active"] == "outside" and not q["held"])
        d.mouse("mouseReleased", 42, 55)
        d.check(route + " late mouse up preserves outside focus", lambda q:
                q["active"] == "outside" and not q["held"] and not q["links"])
        d.mouse("mousePressed", 42, 55, True)
        d.mouse("mouseReleased", 42, 55)
        d.check(route + " next mouse click opens link once", lambda q: q["links"] == expected and not q["held"])
        d.route_tab(url + "?blur=" + route, route)
        concurrent_primary(d, route, baseline["selection"])
        text_agent_blur(d, route)


def text_agent_blur(d, route):
    for owner in ("touch", "mouse"):
        initial = d.focus_find()
        if owner == "touch":
            d.touch("touchStart", (7, 130, 520))
            d.touch("touchMove", (7, 130, 470))
            d.check(route + " touch scroll with Find focused", lambda q: q["find"] and q["held"]
                    and q["contacts"] == 1 and q["scroll"] == initial["scroll"] + 50)
        else:
            d.mouse("mouseMoved", 50, 190)
            d.mouse("mousePressed", 50, 190, True)
            d.check(route + " mouse holds editable document", lambda q: q["held"] and q["focused"])

        # Transfer focus before eframe handles the trusted release, with no intervening
        # motion or animation frame that could let its fallback focus poll hide a missed blur.
        d.script("""
            const agent = document.querySelector("input:not(#outside)");
            const outside = document.getElementById("outside");
            if (!agent || document.activeElement !== agent) throw Error("text agent is not focused");
            qa.agentRelease = null;
            window.addEventListener("pointerup", event => {
                const fromAgent = document.activeElement === agent;
                outside.focus();
                qa.agentRelease = [event.pointerType, event.isTrusted, fromAgent, document.activeElement === outside];
            }, { capture: true, once: true });
        """)
        if owner == "touch":
            d.touch("touchEnd")
        else:
            d.mouse("mouseReleased", 50, 190)
        d.check(f"{route} {owner} release after text-agent blur", lambda q:
                q["agentRelease"] == [owner, True, True, True] and q["active"] == "outside"
                and not q["held"] and not q["contacts"] and not q["links"])


def concurrent_primary(d, route, baseline):
    d.focus_find()
    d.script("""
        qa.primaryEvents = [];
        for (const type of ["pointerdown", "pointerup", "pointercancel"]) {
            document.addEventListener(type, e => {
                const position = type === "pointercancel" ? [] : [e.clientX, e.clientY];
                qa.primaryEvents.push([e.pointerType, type, e.isTrusted, ...position]);
            }, true);
        }
    """)
    d.mouse("mouseMoved", 50, 190)
    d.mouse("mousePressed", 50, 190, True)
    anchor = d.check(route + " mouse owns primary", lambda q:
                     q["held"] and q["focused"] and q["selection"] == baseline[:2] * 2)
    d.touch("touchStart", (3, 42, 55))
    contact = d.check(route + " competing touch begins", lambda q: q["contacts"] == 1)
    d.touch("touchCancel")
    cancelled = d.check(route + " competing touch cancels", lambda q: not q["contacts"])
    for x in range(60, 141, 10):
        d.mouse("mouseMoved", x, 190, True)
    d.mouse("mouseReleased", 140, 190)
    sequence = [["mouse", "pointerdown", True, 50, 190], ["touch", "pointerdown", True, 42, 55],
                ["touch", "pointercancel", True], ["mouse", "pointerup", True, 140, 190]]
    d.check(route + " mouse drag survives concurrent touch", lambda q:
            q["primaryEvents"] == sequence and not q["held"] and q["selection"] == baseline)
    for state in (contact, cancelled):
        assert state["held"] and state["selection"] == anchor["selection"], state

    initial = d.focus_find()
    d.script("qa.primaryEvents.length = 0")
    d.touch("touchStart", (7, 130, 490))
    d.touch("touchMove", (7, 130, 440))
    drag = d.check(route + " touch owns scroll", lambda q: q["held"] and q["contacts"] == 1
                   and q["scroll"] == initial["scroll"] + 50)
    d.mouse("mouseMoved", 50, 190)
    d.mouse("mousePressed", 50, 190, True)
    d.mouse("mouseMoved", 140, 190, True)
    d.mouse("mouseReleased", 140, 190)
    d.check(route + " competing mouse leaves touch in control", lambda q: q["held"] and q["find"]
            and q["selection"] == initial["selection"] and q["scroll"] == drag["scroll"])
    d.touch("touchMove", (7, 130, 390))
    time.sleep(0.2)
    d.touch("touchEnd")
    sequence = [["touch", "pointerdown", True, 130, 490], ["mouse", "pointerdown", True, 50, 190],
                ["mouse", "pointerup", True, 140, 190], ["touch", "pointerup", True, 130, 390]]
    d.check(route + " touch drag survives concurrent mouse", lambda q:
            q["primaryEvents"] == sequence and q["scroll"] == drag["scroll"] + 50
            and q["find"] and q["selection"] == initial["selection"] and not q["contacts"] and not q["held"])
    d.mouse("mouseMoved", 50, 190)
    d.mouse("mousePressed", 50, 190, True)
    d.mouse("mouseMoved", 140, 190, True)
    d.mouse("mouseReleased", 140, 190)
    d.check(route + " next mouse drag", lambda q: q["focused"] and not q["held"]
            and q["selection"][0] == q["selection"][2] and q["selection"][1::2] == baseline[1::2])


def secondary(d):
    def finger(name, actions):
        return {"type": "pointer", "id": name, "parameters": {"pointerType": "touch"}, "actions": actions}

    def move(x, y):
        return {"type": "pointerMove", "x": x, "y": y, "duration": 60}

    down = {"type": "pointerDown", "button": 0}
    up = {"type": "pointerUp", "button": 0}
    pause = {"type": "pause", "duration": 80}
    # One WebDriver action sequence keeps both device identities across ticks.
    # The primary stays still across the secondary lift and then moves another 120 px.
    moves = [move(130, y) for y in range(440, 339, -20)]
    d.post("/actions", {"actions": [
        finger("primary", [move(130, 490), down, move(130, 460), pause, pause, pause, *moves, up]),
        finger("secondary", [pause, pause, move(620, 180), down, up, pause]),
    ]})
    expected = [{"remaining": n, "trusted": True} for n in (1, 0)]
    d.check("secondary lift preserves the primary drag", lambda q:
            q["ends"] == expected and q["afterSecondary"] and
            all(frame["held"] for frame in q["afterSecondary"]) and
            q["afterSecondary"][-1]["scroll"] - q["afterSecondary"][0]["scroll"] >= 100 and
            q["find"] and q["selection"] == [0, 0, 0, 0] and
            not q["held"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in ("package", "eframe", "lockfile"):
        parser.add_argument("--" + arg, required=True, type=Path)
    parser.add_argument("--egui", type=Path, help="companion egui source for a patched eframe")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="textloom-web-touch-") as temp:
        work = Path(temp)
        site = work / "site"
        site.mkdir()
        build(args, work, site)
        with serve(site) as url, browser(work) as driver:
            gestures(driver, url)
    print("Chromium touch and mouse gestures passed")


if __name__ == "__main__":
    main()
