#!/usr/bin/env python3
"""A Windows 11 VM for Textloom's native checks, run with QEMU/KVM on Linux.

The VM uses Microsoft's 90-day Windows 11 Enterprise LTSC evaluation. Keys and
clicks go through QEMU's virtual PS/2 keyboard and USB tablet, so Windows
receives hardware scancodes as it would from a physical keyboard. A PowerShell
agent in the logged-in session runs jobs sent by this script. See README.md.

usage: winvm.py create [--force|--resume]   install Windows unattended (about 30 minutes)
       winvm.py provision              install Mesa, PowerShell 7, NVDA, IMEs, and layouts
       winvm.py up [--view] [--audio wav|pipewire|none]
       winvm.py down
       winvm.py status
       winvm.py run [--admin] [--timeout S] SCRIPT   PowerShell in the desktop session ("-" reads stdin)
       winvm.py share FILE [NAME]      offer a host file to jobs (Get-HostFile NAME DESTINATION)
       winvm.py screenshot FILE.png
       winvm.py keys CHORD...          for example ctrl+c altgr+q win+spc
       winvm.py type TEXT              ASCII text through US-layout keys
       winvm.py click X Y [--right]
       winvm.py snapshot NAME | restore NAME | snapshots
"""

import argparse
import hmac
import json
import os
import secrets
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
STATE = Path(os.environ.get("TEXTLOOM_WINVM", Path.home() / ".local/share/textloom-winvm"))
ISO = STATE / "win11-ltsc-eval-x64-en-us.iso"
ISO_URL = "https://go.microsoft.com/fwlink/?linkid=2289029&clcid=0x409&culture=en-us&country=us"
ISO_SIZE = 5112850432
DISK = STATE / "disk.qcow2"
VARS = STATE / "OVMF_VARS.4m.fd"
UNATTEND = STATE / "unattend.iso"
QMP_SOCKET = STATE / "qmp.sock"
CONFIG = STATE / "config.json"
SHARE = STATE / "share"
INBOX = STATE / "inbox"
QEMU_PID = STATE / "qemu.pid"
SERVER_PID = STATE / "server.pid"
OVMF_CODE = Path("/usr/share/edk2/x64/OVMF_CODE.4m.fd")
OVMF_VARS = Path("/usr/share/edk2/x64/OVMF_VARS.4m.fd")
MEMORY = os.environ.get("TEXTLOOM_WINVM_MEMORY", "4096")
CPUS = os.environ.get("TEXTLOOM_WINVM_CPUS", "4")
POLL_WAIT = 25

ALIASES = {"win": "meta_l", "altgr": "alt_r", "enter": "ret", "space": "spc", "del": "delete",
           "pageup": "pgup", "pagedown": "pgdn", "backtick": "grave_accent"}
US_SYMBOLS = {" ": "spc", "\n": "ret", "\t": "tab", "-": "minus", "=": "equal", "[": "bracket_left",
              "]": "bracket_right", "\\": "backslash", ";": "semicolon", "'": "apostrophe",
              "`": "grave_accent", ",": "comma", ".": "dot", "/": "slash"}
US_SHIFTED = {"!": "1", "@": "2", "#": "3", "$": "4", "%": "5", "^": "6", "&": "7", "*": "8", "(": "9",
              ")": "0", "_": "minus", "+": "equal", "{": "bracket_left", "}": "bracket_right",
              "|": "backslash", ":": "semicolon", '"': "apostrophe", "~": "grave_accent", "<": "comma",
              ">": "dot", "?": "slash"}


def fail(message):
    print(f"winvm: {message}", file=sys.stderr)
    sys.exit(1)


def config():
    if not CONFIG.exists():
        STATE.mkdir(parents=True, exist_ok=True)
        CONFIG.write_text(json.dumps({"port": 18765, "token": secrets.token_hex(16)}))
        CONFIG.chmod(0o600)
    return json.loads(CONFIG.read_text())


def alive(pid_file):
    try:
        pid = int(pid_file.read_text().strip())
        os.kill(pid, 0)
        return pid
    except (OSError, ValueError):
        return None


# --------------------------------------------------------------------------- job server

class Jobs:
    def __init__(self):
        self.condition = threading.Condition()
        self.pending = []
        self.results = {}
        self.last_poll = 0.0

    def submit(self, job):
        with self.condition:
            self.pending.append(job)
            self.condition.notify_all()

    def take(self, wait):
        deadline = time.monotonic() + wait
        with self.condition:
            self.last_poll = time.time()
            while not self.pending and time.monotonic() < deadline:
                self.condition.wait(deadline - time.monotonic())
            self.last_poll = time.time()
            return self.pending.pop(0) if self.pending else None

    def finish(self, job_id, result):
        with self.condition:
            self.results[job_id] = result
            self.condition.notify_all()

    def result(self, job_id, wait):
        deadline = time.monotonic() + wait
        with self.condition:
            while job_id not in self.results and time.monotonic() < deadline:
                self.condition.wait(deadline - time.monotonic())
            return self.results.pop(job_id, None)


def serve():
    settings = config()
    token = settings["token"].encode()
    jobs = Jobs()
    SHARE.mkdir(parents=True, exist_ok=True)
    INBOX.mkdir(parents=True, exist_ok=True)

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, format, *arguments):
            pass

        def reply(self, status, body=b"", content_type="application/json"):
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if body:
                self.wfile.write(body)

        def body(self):
            return self.rfile.read(int(self.headers.get("Content-Length") or 0))

        def authorized(self):
            if hmac.compare_digest(self.headers.get("X-Token", "").encode(), token):
                return True
            self.reply(403)
            return False

        def name(self, prefix):
            name = self.path[len(prefix):]
            if not name or "/" in name or name.startswith("."):
                self.reply(400)
                return None
            return name

        def do_GET(self):
            if not self.authorized():
                return
            if self.path == "/next":
                job = jobs.take(POLL_WAIT)
                self.reply(200, json.dumps(job).encode()) if job else self.reply(204)
            elif self.path == "/status":
                self.reply(200, json.dumps({"last_poll": jobs.last_poll, "pending": len(jobs.pending)}).encode())
            elif self.path.startswith("/share/"):
                name = self.name("/share/")
                path = SHARE / name if name else None
                if path and path.is_file():
                    self.reply(200, path.read_bytes(), "application/octet-stream")
                elif name:
                    self.reply(404)
            else:
                self.reply(404)

        def do_PUT(self):
            if not self.authorized():
                return
            name = self.name("/inbox/") if self.path.startswith("/inbox/") else None
            if name:
                (INBOX / name).write_bytes(self.body())
                self.reply(204)
            elif not self.path.startswith("/inbox/"):
                self.reply(404)

        def do_POST(self):
            if not self.authorized():
                return
            if self.path.startswith("/result/"):
                jobs.finish(self.path[len("/result/"):], json.loads(self.body().decode("utf-8-sig")))
                self.reply(204)
            elif self.path == "/run":
                request = json.loads(self.body())
                job = {"id": uuid.uuid4().hex, "script": request["script"], "admin": bool(request.get("admin")),
                       "timeout": int(request.get("timeout", 600))}
                jobs.submit(job)
                result = jobs.result(job["id"], job["timeout"] + 120)
                self.reply(200, json.dumps(result).encode()) if result else self.reply(504)
            else:
                self.reply(404)

    server = ThreadingHTTPServer(("127.0.0.1", settings["port"]), Handler)
    server.daemon_threads = True
    server.serve_forever()


def request(method, path, payload=None, timeout=60):
    settings = config()
    data = json.dumps(payload).encode() if payload is not None else None
    http_request = urllib.request.Request(f"http://127.0.0.1:{settings['port']}{path}", data=data, method=method,
                                          headers={"X-Token": settings["token"], "Content-Type": "application/json"})
    with urllib.request.urlopen(http_request, timeout=timeout) as response:
        return json.loads(response.read() or b"null")


def ensure_server():
    if alive(SERVER_PID):
        return
    log = (STATE / "server.log").open("a")
    process = subprocess.Popen([sys.executable, "-I", str(Path(__file__).resolve()), "serve"], stdout=log,
                               stderr=subprocess.STDOUT, start_new_session=True)
    SERVER_PID.write_text(str(process.pid))
    for _ in range(50):
        try:
            request("GET", "/status", timeout=2)
            return
        except OSError:
            time.sleep(0.1)
    fail("the job server did not start; see server.log")


def agent_seen(since=0.0):
    try:
        last = request("GET", "/status", timeout=5)["last_poll"]
    except OSError:
        return False
    return last > since and time.time() - last < POLL_WAIT + 15


def wait_for_agent(timeout, since=0.0, progress=None):
    started = time.monotonic()
    while time.monotonic() - started < timeout:
        if agent_seen(since):
            return True
        if progress:
            progress(time.monotonic() - started)
        time.sleep(10)
    return False


def run(script, admin=False, timeout=600):
    """Run PowerShell in the guest's desktop session; returns {exit, timed_out, output}."""
    try:
        result = request("POST", "/run", {"script": script, "admin": admin, "timeout": timeout},
                         timeout=timeout + 180)
    except urllib.error.HTTPError as error:
        raise RuntimeError(f"guest job failed with HTTP {error.code}") from error
    if result is None:
        raise RuntimeError("guest job returned nothing")
    return result


def share(path, name=None):
    SHARE.mkdir(parents=True, exist_ok=True)
    target = SHARE / (name or Path(path).name)
    shutil.copyfile(path, target)
    return target.name


# --------------------------------------------------------------------------- QEMU

class QMP:
    def __init__(self, timeout=30):
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.socket.settimeout(timeout)
        self.socket.connect(str(QMP_SOCKET))
        self.file = self.socket.makefile("rwb")
        self.read()
        self.command("qmp_capabilities")

    def read(self):
        while True:
            line = self.file.readline()
            if not line:
                raise ConnectionError("QMP connection closed")
            message = json.loads(line)
            if "event" not in message:
                return message

    def command(self, name, **arguments):
        message = {"execute": name, "arguments": arguments} if arguments else {"execute": name}
        self.file.write(json.dumps(message).encode() + b"\n")
        self.file.flush()
        reply = self.read()
        if "error" in reply:
            raise RuntimeError(f"QMP {name}: {reply['error'].get('desc')}")
        return reply.get("return")

    def close(self):
        # The socket stays open while its file wrapper is, and QEMU serves one client at a time.
        self.file.close()
        self.socket.close()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()

    def key(self, qcode, down):
        self.command("input-send-event", events=[
            {"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": ALIASES.get(qcode, qcode)}}}])

    def chord(self, spec, hold=0.06):
        keys = spec.split("+")
        for key in keys:
            self.key(key, True)
            time.sleep(0.02)
        time.sleep(hold)
        for key in reversed(keys):
            self.key(key, False)
            time.sleep(0.02)

    def type(self, text, delay=0.04):
        for character in text:
            if character.isascii() and character.isalpha():
                key, shifted = character.lower(), character.isupper()
            elif character.isdigit():
                key, shifted = character, False
            elif character in US_SYMBOLS:
                key, shifted = US_SYMBOLS[character], False
            elif character in US_SHIFTED:
                key, shifted = US_SHIFTED[character], True
            else:
                raise ValueError(f"no US key for {character!r}")
            self.chord(f"shift+{key}" if shifted else key)
            time.sleep(delay)

    def screenshot(self, path):
        path = Path(path).resolve()
        self.command("screendump", filename=str(path), format="png")
        for _ in range(50):
            if path.exists() and path.stat().st_size > 0:
                return path
            time.sleep(0.05)
        raise RuntimeError("screendump produced no file")

    def size(self):
        with tempfile.TemporaryDirectory() as directory:
            data = self.screenshot(Path(directory) / "size.png").read_bytes()
        return struct.unpack(">II", data[16:24])

    def move(self, x, y, size=None):
        width, height = size or self.size()
        self.command("input-send-event", events=[
            {"type": "abs", "data": {"axis": "x", "value": round(x * 32767 / max(width - 1, 1))}},
            {"type": "abs", "data": {"axis": "y", "value": round(y * 32767 / max(height - 1, 1))}}])

    def button(self, down, button="left"):
        self.command("input-send-event", events=[{"type": "btn", "data": {"down": down, "button": button}}])

    def click(self, x, y, button="left", size=None):
        self.move(x, y, size)
        time.sleep(0.05)
        self.button(True, button)
        time.sleep(0.06)
        self.button(False, button)


def qmp(timeout=30):
    for _ in range(100):
        if QMP_SOCKET.exists():
            try:
                return QMP(timeout)
            except OSError:
                pass
        time.sleep(0.1)
    fail("QEMU's QMP socket is not available; is the VM up?")


def qemu_command(install=False, view=False, audio="wav"):
    command = [
        "qemu-system-x86_64", "-name", "textloom-win11", "-machine", "q35,accel=kvm",
        "-cpu", "host,hv_relaxed,hv_vapic,hv_spinlocks=0x1fff,hv_time,hv_vpindex,hv_synic,hv_stimer",
        "-smp", CPUS, "-m", MEMORY, "-rtc", "base=localtime",
        "-drive", f"if=pflash,format=raw,readonly=on,file={OVMF_CODE}",
        "-drive", f"if=pflash,format=raw,file={VARS}",
        "-drive", f"file={DISK},if=none,id=hd0,format=qcow2,discard=unmap",
        "-device", f"ide-hd,drive=hd0,bus=ide.0,bootindex={1 if install else 0}",
        "-netdev", "user,id=net0", "-device", "e1000e,netdev=net0",
        "-device", "qemu-xhci", "-device", "usb-tablet",
        "-vga", "none", "-device", "VGA,edid=on,xres=1280,yres=800",
        "-qmp", f"unix:{QMP_SOCKET},server=on,wait=off",
        "-pidfile", str(QEMU_PID),
        "-display", "gtk" if view else "none",
    ]
    if audio == "wav":
        command += ["-audiodev", f"wav,id=snd0,path={STATE / 'audio.wav'}"]
    elif audio == "pipewire":
        command += ["-audiodev", "pipewire,id=snd0"]
    else:
        command += ["-audiodev", "none,id=snd0"]
    command += ["-device", "ich9-intel-hda", "-device", "hda-output,audiodev=snd0"]
    if install:
        command += [
            "-drive", f"file={ISO},media=cdrom,if=none,id=cd0,readonly=on",
            "-device", "ide-cd,drive=cd0,bus=ide.1,bootindex=0",
            "-drive", f"file={UNATTEND},media=cdrom,if=none,id=cd1,readonly=on",
            "-device", "ide-cd,drive=cd1,bus=ide.2",
        ]
    return command


def start_qemu(install=False, view=False, audio="wav"):
    if alive(QEMU_PID):
        fail("the VM is already running")
    QMP_SOCKET.unlink(missing_ok=True)
    log = (STATE / "qemu.log").open("a")
    subprocess.Popen(qemu_command(install, view, audio), stdout=log, stderr=subprocess.STDOUT,
                     start_new_session=True)
    for _ in range(100):
        if alive(QEMU_PID) and QMP_SOCKET.exists():
            return
        time.sleep(0.1)
    fail("QEMU did not start; see qemu.log")


def require_tools():
    missing = [tool for tool in ("qemu-system-x86_64", "qemu-img", "xorriso") if not shutil.which(tool)]
    missing += [str(path) for path in (OVMF_CODE, OVMF_VARS) if not path.exists()]
    if missing:
        fail("missing " + ", ".join(missing) +
             "; install them with: sudo pacman -S --needed qemu-desktop edk2-ovmf libisoburn")


# --------------------------------------------------------------------------- commands

def build_unattend():
    settings = config()
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        shutil.copyfile(HERE / "autounattend.xml", root / "autounattend.xml")
        for name in ("firstlogon.ps1", "agent.ps1"):
            shutil.copyfile(HERE / "guest" / name, root / name)
        (root / "agent.json").write_text(json.dumps(settings))
        subprocess.run(["xorriso", "-as", "mkisofs", "-quiet", "-J", "-r", "-V", "TLQA", "-o", str(UNATTEND),
                        str(root)], check=True)


def create(arguments):
    require_tools()
    if arguments.resume:
        # Wait for an install that is already running, for example after this script was interrupted.
        if not alive(QEMU_PID):
            fail("no install is running")
        ensure_server()
        started = 0.0
    else:
        if DISK.exists() and not arguments.force:
            fail(f"{DISK} exists; pass --force to reinstall Windows")
        if alive(QEMU_PID):
            fail("the VM is running; run `winvm.py down` first")
        STATE.mkdir(parents=True, exist_ok=True)
        if not ISO.exists() or ISO.stat().st_size != ISO_SIZE:
            print("Downloading the Windows 11 Enterprise LTSC evaluation ISO (5.1 GB)...", flush=True)
            subprocess.run(["curl", "-L", "--fail", "--retry", "3", "-C", "-", "-o", str(ISO), ISO_URL], check=True)
        build_unattend()
        DISK.unlink(missing_ok=True)
        subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", str(DISK), "64G"], check=True)
        shutil.copyfile(OVMF_VARS, VARS)
        ensure_server()
        started = time.time()
        start_qemu(install=True, audio="none")
        with qmp() as monitor:
            # The Windows disc waits for a key before booting.
            for _ in range(60):
                monitor.chord("ret")
                time.sleep(0.5)

    def progress(elapsed):
        if int(elapsed) % 120 < 10:
            with qmp() as monitor:
                monitor.screenshot(STATE / "progress.png")
            print(f"  installing: {int(elapsed // 60)} min (screen: {STATE / 'progress.png'})", flush=True)

    print("Installing Windows unattended; this takes about 30 minutes.", flush=True)
    if not wait_for_agent(150 * 60, since=started, progress=progress):
        fail(f"the guest agent did not start; the VM is still running (screen: {STATE / 'progress.png'})")
    result = run("(Get-CimInstance Win32_OperatingSystem) | ForEach-Object { \"$($_.Caption) $($_.Version)\" }")
    print("Installed:", result["output"].strip())
    down(None)
    snapshot_create("installed")
    print("Saved snapshot 'installed'. Next: winvm.py provision")


def provision(arguments):
    up_vm(view=False, audio="none")
    share(HERE / "guest" / "provision.ps1")
    share(HERE / "guest" / "agent.ps1")
    print("Provisioning; this downloads about 1.5 GB and takes 20 to 40 minutes.", flush=True)
    # The new agent takes over at the restart below.
    result = run("New-Item -ItemType Directory -Force C:\\textloom-qa | Out-Null\n"
                 "Get-ChildItem C:\\textloom-qa -File | ForEach-Object { $_.IsReadOnly = $false }\n"
                 "Get-HostFile agent.ps1 C:\\textloom-qa\\agent.ps1\n"
                 "Get-HostFile provision.ps1 C:\\textloom-qa\\provision.ps1\n"
                 "& C:\\textloom-qa\\provision.ps1", admin=True, timeout=4 * 3600)
    print(result["output"])
    if result["exit"] != 0:
        fail("provisioning failed; the VM is still running for inspection")
    restarted = time.time()
    run("shutdown /r /t 5", admin=True, timeout=60)
    time.sleep(60)
    # A long poll begun just before the restart can end up to 30 s later.
    if not wait_for_agent(15 * 60, since=restarted + 60):
        fail("the guest agent did not return after the restart")
    print(run("& C:\\textloom-qa\\provision.ps1 -Verify", timeout=300)["output"])
    down(None)
    snapshot_create("provisioned")
    print("Saved snapshot 'provisioned'.")


def up_vm(view=False, audio="wav"):
    if not DISK.exists():
        fail("no VM yet; run `winvm.py create`")
    ensure_server()
    if alive(QEMU_PID):
        if wait_for_agent(60):
            return
        fail("the VM is running but its agent is not responding")
    started = time.time()
    start_qemu(view=view, audio=audio)
    if not wait_for_agent(10 * 60, since=started):
        fail("the guest agent did not start within 10 minutes")


def up(arguments):
    up_vm(arguments.view, arguments.audio)
    print("The VM is up and its agent is responding.")


def down(arguments):
    pid = alive(QEMU_PID)
    if pid:
        try:
            with qmp(timeout=10) as monitor:
                monitor.command("system_powerdown")
        except (OSError, RuntimeError, SystemExit):
            pass
        for _ in range(180):
            if not alive(QEMU_PID):
                break
            time.sleep(1)
        else:
            os.kill(pid, signal.SIGTERM)
            time.sleep(3)
    server = alive(SERVER_PID)
    if server:
        os.kill(server, signal.SIGTERM)
    for path in (QEMU_PID, SERVER_PID, QMP_SOCKET):
        path.unlink(missing_ok=True)
    if arguments is not None:
        print("The VM is down.")


def status(arguments):
    print("VM:", "running" if alive(QEMU_PID) else "stopped")
    print("job server:", "running" if alive(SERVER_PID) else "stopped")
    print("agent:", "responding" if alive(SERVER_PID) and agent_seen() else "not responding")
    print("state:", STATE)


def snapshot_create(name):
    if alive(QEMU_PID):
        fail("snapshots need the VM down")
    subprocess.run(["qemu-img", "snapshot", "-c", name, str(DISK)], check=True)
    (STATE / "vars").mkdir(exist_ok=True)
    shutil.copyfile(VARS, STATE / "vars" / f"{name}.fd")


def snapshot(arguments):
    snapshot_create(arguments.name)
    print(f"Saved snapshot '{arguments.name}'.")


def restore(arguments):
    if alive(QEMU_PID):
        fail("restoring needs the VM down")
    subprocess.run(["qemu-img", "snapshot", "-a", arguments.name, str(DISK)], check=True)
    shutil.copyfile(STATE / "vars" / f"{arguments.name}.fd", VARS)
    print(f"Restored snapshot '{arguments.name}'.")


def snapshots(arguments):
    subprocess.run(["qemu-img", "snapshot", "-l", str(DISK)], check=True)


def run_command(arguments):
    script = sys.stdin.read() if arguments.script == "-" else arguments.script
    result = run(script, arguments.admin, arguments.timeout)
    sys.stdout.write(result["output"])
    if result.get("timed_out"):
        fail(f"the job timed out after {arguments.timeout} s")
    sys.exit(result["exit"])


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("serve")
    create_parser = commands.add_parser("create")
    create_parser.add_argument("--force", action="store_true", help="replace an existing VM")
    create_parser.add_argument("--resume", action="store_true", help="wait for an install already running")
    commands.add_parser("provision")
    up_parser = commands.add_parser("up")
    up_parser.add_argument("--view", action="store_true", help="show the screen in a window")
    up_parser.add_argument("--audio", choices=("wav", "pipewire", "none"), default="wav",
                           help="wav records guest audio to audio.wav; pipewire plays it")
    commands.add_parser("down")
    commands.add_parser("status")
    run_parser = commands.add_parser("run")
    run_parser.add_argument("--admin", action="store_true")
    run_parser.add_argument("--timeout", type=int, default=600)
    run_parser.add_argument("script")
    share_parser = commands.add_parser("share")
    share_parser.add_argument("file")
    share_parser.add_argument("name", nargs="?")
    commands.add_parser("screenshot").add_argument("file")
    commands.add_parser("keys").add_argument("chords", nargs="+")
    commands.add_parser("type").add_argument("text")
    click_parser = commands.add_parser("click")
    click_parser.add_argument("x", type=int)
    click_parser.add_argument("y", type=int)
    click_parser.add_argument("--right", action="store_true")
    for name in ("snapshot", "restore"):
        commands.add_parser(name).add_argument("name")
    commands.add_parser("snapshots")
    arguments = parser.parse_args()

    if arguments.command == "serve":
        serve()
    elif arguments.command == "share":
        print(share(arguments.file, arguments.name))
    elif arguments.command == "screenshot":
        with qmp() as monitor:
            print(monitor.screenshot(arguments.file))
    elif arguments.command == "keys":
        with qmp() as monitor:
            for chord in arguments.chords:
                monitor.chord(chord)
                time.sleep(0.1)
    elif arguments.command == "type":
        with qmp() as monitor:
            monitor.type(arguments.text)
    elif arguments.command == "click":
        with qmp() as monitor:
            monitor.click(arguments.x, arguments.y, "right" if arguments.right else "left")
    else:
        {"create": create, "provision": provision, "up": up, "down": down, "status": status,
         "run": run_command, "snapshot": snapshot, "restore": restore,
         "snapshots": snapshots}[arguments.command](arguments)


if __name__ == "__main__":
    main()
