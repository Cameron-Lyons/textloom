//! A Windows 11 VM for Textloom's native checks, run with QEMU/KVM on Linux.
//!
//! The VM uses Microsoft's 90-day Windows 11 Enterprise LTSC evaluation. Keys
//! and clicks go through QEMU's virtual PS/2 keyboard and USB tablet, so
//! Windows receives hardware scancodes as it would from a physical keyboard. A
//! PowerShell agent in the logged-in session runs jobs sent by this tool. See
//! `scripts/windows-vm/README.md`.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{env, thread};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli::Arguments;
use crate::{Context, Result, TempDir, repo_root};

pub const USAGE: &str = "\
usage: winvm create [--force|--resume]   install Windows unattended (about 30 minutes)
       winvm provision              install Mesa, PowerShell 7, NVDA, IMEs, and layouts
       winvm up [--view] [--audio wav|pipewire|none]
       winvm down
       winvm status
       winvm run [--admin] [--timeout S] SCRIPT   PowerShell in the desktop session (\"-\" reads stdin)
       winvm share FILE [NAME]      offer a host file to jobs (Get-HostFile NAME DESTINATION)
       winvm screenshot FILE.png
       winvm keys CHORD...          for example ctrl+c altgr+q win+spc
       winvm type TEXT              ASCII text through US-layout keys
       winvm click X Y [--right]
       winvm snapshot NAME | restore NAME | snapshots";

const ISO_URL: &str =
    "https://go.microsoft.com/fwlink/?linkid=2289029&clcid=0x409&culture=en-us&country=us";
const ISO_SIZE: u64 = 5_112_850_432;
const OVMF_CODE: &str = "/usr/share/edk2/x64/OVMF_CODE.4m.fd";
const OVMF_VARS: &str = "/usr/share/edk2/x64/OVMF_VARS.4m.fd";
/// Seconds a guest poll waits for a job.
const POLL_WAIT: f64 = 25.0;

const ALIASES: &[(&str, &str)] = &[
    ("win", "meta_l"),
    ("altgr", "alt_r"),
    ("enter", "ret"),
    ("space", "spc"),
    ("del", "delete"),
    ("pageup", "pgup"),
    ("pagedown", "pgdn"),
    ("backtick", "grave_accent"),
];
const US_SYMBOLS: &[(char, &str)] = &[
    (' ', "spc"),
    ('\n', "ret"),
    ('\t', "tab"),
    ('-', "minus"),
    ('=', "equal"),
    ('[', "bracket_left"),
    (']', "bracket_right"),
    ('\\', "backslash"),
    (';', "semicolon"),
    ('\'', "apostrophe"),
    ('`', "grave_accent"),
    (',', "comma"),
    ('.', "dot"),
    ('/', "slash"),
];
const US_SHIFTED: &[(char, &str)] = &[
    ('!', "1"),
    ('@', "2"),
    ('#', "3"),
    ('$', "4"),
    ('%', "5"),
    ('^', "6"),
    ('&', "7"),
    ('*', "8"),
    ('(', "9"),
    (')', "0"),
    ('_', "minus"),
    ('+', "equal"),
    ('{', "bracket_left"),
    ('}', "bracket_right"),
    ('|', "backslash"),
    (':', "semicolon"),
    ('"', "apostrophe"),
    ('~', "grave_accent"),
    ('<', "comma"),
    ('>', "dot"),
    ('?', "slash"),
];

fn lookup<K: PartialEq, V: Copy>(table: &[(K, V)], key: &K) -> Option<V> {
    table
        .iter()
        .find(|(candidate, _)| candidate == key)
        .map(|&(_, value)| value)
}

fn sleep(seconds: f64) {
    thread::sleep(Duration::from_secs_f64(seconds));
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

/// Runs a command with this terminal's output, failing unless it succeeds.
fn exec(program: &str, arguments: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(arguments)
        .status()
        .context(program)?;
    if !status.success() {
        return Err(format!("{program} failed ({status})").into());
    }
    Ok(())
}

fn random_hex(bytes: usize) -> Result<String> {
    let mut buffer = vec![0; bytes];
    File::open("/dev/urandom")?.read_exact(&mut buffer)?;
    Ok(buffer.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// The job server's port and the token every request carries.
#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub port: u16,
    pub token: String,
}

/// What a guest job printed and how it ended.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct JobResult {
    pub exit: i64,
    pub timed_out: bool,
    pub output: String,
}

/// The VM's state directory and everything kept in it.
#[derive(Clone)]
pub struct Vm {
    pub state: PathBuf,
}

impl Vm {
    /// `TEXTLOOM_WINVM`, or `~/.local/share/textloom-winvm`.
    pub fn from_env() -> Self {
        let state = env::var_os("TEXTLOOM_WINVM")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
                home.join(".local/share/textloom-winvm")
            });
        Self { state }
    }

    fn iso(&self) -> PathBuf {
        self.state.join("win11-ltsc-eval-x64-en-us.iso")
    }

    fn disk(&self) -> PathBuf {
        self.state.join("disk.qcow2")
    }

    fn vars(&self) -> PathBuf {
        self.state.join("OVMF_VARS.4m.fd")
    }

    fn unattend(&self) -> PathBuf {
        self.state.join("unattend.iso")
    }

    fn qmp_socket(&self) -> PathBuf {
        self.state.join("qmp.sock")
    }

    fn config_path(&self) -> PathBuf {
        self.state.join("config.json")
    }

    fn share_dir(&self) -> PathBuf {
        self.state.join("share")
    }

    /// Files guest jobs send with `Send-HostFile`.
    pub fn inbox(&self) -> PathBuf {
        self.state.join("inbox")
    }

    fn qemu_pid(&self) -> PathBuf {
        self.state.join("qemu.pid")
    }

    fn server_pid(&self) -> PathBuf {
        self.state.join("server.pid")
    }

    /// Recorded guest audio, when the VM is up with `--audio wav`.
    pub fn audio(&self) -> PathBuf {
        self.state.join("audio.wav")
    }

    /// The job server's settings, created with a fresh token on first use.
    pub fn config(&self) -> Result<Config> {
        let path = self.config_path();
        if !path.exists() {
            fs::create_dir_all(&self.state).context(self.state.display())?;
            let config = Config {
                port: 18765,
                token: random_hex(16)?,
            };
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            file.write_all(serde_json::to_string(&config)?.as_bytes())?;
        }
        let text = fs::read_to_string(&path).context(path.display())?;
        Ok(serde_json::from_str(&text)?)
    }

    /// Sends a request to the job server; returns its status and body.
    pub fn request(
        &self,
        method: &str,
        path: &str,
        payload: Option<&Value>,
        timeout: f64,
    ) -> Result<(u16, Vec<u8>)> {
        let config = self.config()?;
        let body = payload
            .map(serde_json::to_vec)
            .transpose()?
            .unwrap_or_default();
        Ok(http(
            config.port,
            &config.token,
            method,
            path,
            &body,
            timeout,
        )?)
    }

    fn request_json(&self, method: &str, path: &str, timeout: f64) -> Result<Value> {
        let (status, body) = self.request(method, path, None, timeout)?;
        if status >= 400 {
            return Err(format!("HTTP {status}").into());
        }
        Ok(if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body)?
        })
    }

    fn ensure_server(&self) -> Result<()> {
        if alive(&self.server_pid()).is_some() {
            return Ok(());
        }
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.state.join("server.log"))?;
        // This binary serves; its new process group outlives this command.
        let child = Command::new(env::current_exe()?)
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .process_group(0)
            .spawn()?;
        fs::write(self.server_pid(), child.id().to_string())?;
        for _ in 0..50 {
            if self.request_json("GET", "/status", 2.0).is_ok() {
                return Ok(());
            }
            sleep(0.1);
        }
        Err("the job server did not start; see server.log".into())
    }

    fn agent_seen(&self, since: f64) -> bool {
        let Ok(status) = self.request_json("GET", "/status", 5.0) else {
            return false;
        };
        let last = status["last_poll"].as_f64().unwrap_or(0.0);
        last > since && unix_now() - last < POLL_WAIT + 15.0
    }

    fn wait_for_agent(&self, timeout: f64, since: f64, mut progress: impl FnMut(f64)) -> bool {
        let started = Instant::now();
        while started.elapsed().as_secs_f64() < timeout {
            if self.agent_seen(since) {
                return true;
            }
            progress(started.elapsed().as_secs_f64());
            sleep(10.0);
        }
        false
    }

    /// Runs PowerShell in the guest's desktop session.
    pub fn run(&self, script: &str, admin: bool, timeout: u64) -> Result<JobResult> {
        let payload = json!({"script": script, "admin": admin, "timeout": timeout});
        let (status, body) =
            self.request("POST", "/run", Some(&payload), timeout as f64 + 180.0)?;
        if status >= 400 {
            return Err(format!("guest job failed with HTTP {status}").into());
        }
        let result: Option<JobResult> = if body.is_empty() {
            None
        } else {
            serde_json::from_slice(&body)?
        };
        Ok(result.ok_or("guest job returned nothing")?)
    }

    /// Offers a host file to guest jobs; returns its name there.
    pub fn share(&self, path: &Path, name: Option<&str>) -> Result<String> {
        let directory = self.share_dir();
        fs::create_dir_all(&directory)?;
        let name = match name {
            Some(name) => name.to_owned(),
            None => path
                .file_name()
                .ok_or("nothing to share")?
                .to_string_lossy()
                .into_owned(),
        };
        fs::copy(path, directory.join(&name)).context(path.display())?;
        Ok(name)
    }

    /// Connects to QEMU's monitor, waiting briefly for its socket.
    pub fn qmp(&self, timeout: f64) -> Result<Qmp> {
        let socket = self.qmp_socket();
        for _ in 0..100 {
            if socket.exists()
                && let Ok(monitor) = Qmp::connect(&socket, timeout)
            {
                return Ok(monitor);
            }
            sleep(0.1);
        }
        Err("QEMU's QMP socket is not available; is the VM up?".into())
    }
}

/// The running process recorded in a PID file.
fn alive(pid_file: &Path) -> Option<i32> {
    use rustix::process::{Pid, test_kill_process};
    let pid: i32 = fs::read_to_string(pid_file).ok()?.trim().parse().ok()?;
    test_kill_process(Pid::from_raw(pid)?).ok().map(|()| pid)
}

fn terminate(pid: i32) {
    use rustix::process::{Pid, Signal, kill_process};
    if let Some(pid) = Pid::from_raw(pid) {
        let _ = kill_process(pid, Signal::TERM);
    }
}

// ---------------------------------------------------------------------------- job server

#[derive(Default)]
struct Jobs {
    state: Mutex<JobState>,
    changed: Condvar,
}

#[derive(Default)]
struct JobState {
    pending: VecDeque<Value>,
    results: HashMap<String, Value>,
    last_poll: f64,
}

impl Jobs {
    fn lock(&self) -> MutexGuard<'_, JobState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait_until(
        &self,
        wait: f64,
        mut ready: impl FnMut(&mut JobState) -> bool,
    ) -> MutexGuard<'_, JobState> {
        let deadline = Instant::now() + Duration::from_secs_f64(wait);
        let mut state = self.lock();
        while !ready(&mut state) {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            state = self
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        state
    }

    fn submit(&self, job: Value) {
        self.lock().pending.push_back(job);
        self.changed.notify_all();
    }

    fn take(&self, wait: f64) -> Option<Value> {
        self.lock().last_poll = unix_now();
        let mut state = self.wait_until(wait, |state| !state.pending.is_empty());
        state.last_poll = unix_now();
        state.pending.pop_front()
    }

    fn finish(&self, id: String, result: Value) {
        self.lock().results.insert(id, result);
        self.changed.notify_all();
    }

    fn result(&self, id: &str, wait: f64) -> Option<Value> {
        self.wait_until(wait, |state| state.results.contains_key(id))
            .results
            .remove(id)
    }
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

struct Response {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Response {
    fn status(status: u16) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: Vec::new(),
        }
    }

    fn json(value: &Value) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            body: value.to_string().into_bytes(),
        }
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        504 => "Gateway Timeout",
        _ => "",
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn read_line(reader: &mut impl BufRead) -> io::Result<String> {
    let mut line = String::new();
    reader.take(16 * 1024).read_line(&mut line)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

fn read_headers(reader: &mut impl BufRead) -> io::Result<Vec<(String, String)>> {
    let mut headers = Vec::new();
    loop {
        let line = read_line(reader)?;
        if line.is_empty() {
            return Ok(headers);
        }
        if headers.len() == 100 {
            return Err(invalid("too many headers"));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| invalid("malformed header"))?;
        headers.push((name.trim().to_owned(), value.trim().to_owned()));
    }
}

fn content_length(headers: &[(String, String)]) -> io::Result<Option<usize>> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Content-Length"))
        .map(|(_, value)| value.parse().map_err(|_| invalid("bad Content-Length")))
        .transpose()
}

/// A guest file name: no directories, and nothing hidden.
fn file_name(name: &str) -> Option<&str> {
    (!name.is_empty() && !name.contains('/') && !name.starts_with('.')).then_some(name)
}

/// Compares in time independent of where the inputs differ.
fn same_token(given: &[u8], token: &[u8]) -> bool {
    given.len() == token.len()
        && given
            .iter()
            .zip(token)
            .fold(0, |difference, (a, b)| difference | (a ^ b))
            == 0
}

struct Server {
    vm: Vm,
    token: String,
    jobs: Jobs,
}

impl Server {
    fn connection(&self, stream: TcpStream) -> io::Result<()> {
        let mut writer = stream.try_clone()?;
        let mut reader = BufReader::new(stream);
        let line = read_line(&mut reader)?;
        let mut parts = line.split_whitespace();
        let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
            return Err(invalid("malformed request line"));
        };
        let headers = read_headers(&mut reader)?;
        // Windows PowerShell's web clients wait for this before sending a body.
        let expects = headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("Expect") && value.eq_ignore_ascii_case("100-continue")
        });
        if expects {
            writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        }
        let mut body = vec![0; content_length(&headers)?.unwrap_or(0)];
        reader.read_exact(&mut body)?;
        let request = Request {
            method: method.to_owned(),
            path: path.to_owned(),
            headers,
            body,
        };
        let response = self.respond(&request);
        write!(
            writer,
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            response.status,
            reason(response.status),
            response.content_type,
            response.body.len()
        )?;
        writer.write_all(&response.body)?;
        writer.flush()
    }

    fn respond(&self, request: &Request) -> Response {
        if !same_token(
            request.header("X-Token").unwrap_or("").as_bytes(),
            self.token.as_bytes(),
        ) {
            return Response::status(403);
        }
        let path = request.path.as_str();
        match request.method.as_str() {
            "GET" if path == "/next" => match self.jobs.take(POLL_WAIT) {
                Some(job) => Response::json(&job),
                None => Response::status(204),
            },
            "GET" if path == "/status" => {
                let state = self.jobs.lock();
                Response::json(
                    &json!({"last_poll": state.last_poll, "pending": state.pending.len()}),
                )
            }
            "GET" if path.starts_with("/share/") => {
                let Some(name) = file_name(&path["/share/".len()..]) else {
                    return Response::status(400);
                };
                match fs::read(self.vm.share_dir().join(name)) {
                    Ok(bytes) => Response {
                        status: 200,
                        content_type: "application/octet-stream",
                        body: bytes,
                    },
                    Err(_) => Response::status(404),
                }
            }
            "PUT" if path.starts_with("/inbox/") => {
                let Some(name) = file_name(&path["/inbox/".len()..]) else {
                    return Response::status(400);
                };
                match fs::write(self.vm.inbox().join(name), &request.body) {
                    Ok(()) => Response::status(204),
                    Err(_) => Response::status(500),
                }
            }
            "POST" if path.starts_with("/result/") => {
                let body = request
                    .body
                    .strip_prefix(b"\xef\xbb\xbf")
                    .unwrap_or(&request.body);
                match serde_json::from_slice(body) {
                    Ok(result) => {
                        self.jobs
                            .finish(path["/result/".len()..].to_owned(), result);
                        Response::status(204)
                    }
                    Err(_) => Response::status(400),
                }
            }
            "POST" if path == "/run" => {
                let Ok(request) = serde_json::from_slice::<Value>(&request.body) else {
                    return Response::status(400);
                };
                let Some(script) = request["script"].as_str() else {
                    return Response::status(400);
                };
                let timeout = request["timeout"].as_u64().unwrap_or(600);
                let Ok(id) = random_hex(16) else {
                    return Response::status(500);
                };
                let admin = request["admin"].as_bool().unwrap_or(false);
                self.jobs.submit(
                    json!({"id": id, "script": script, "admin": admin, "timeout": timeout}),
                );
                match self.jobs.result(&id, timeout as f64 + 120.0) {
                    Some(result) => Response::json(&result),
                    None => Response::status(504),
                }
            }
            "GET" | "PUT" | "POST" => Response::status(404),
            _ => Response::status(501),
        }
    }
}

/// Serves guest agents on `listener` until the process ends.
pub fn serve_on(vm: &Vm, config: &Config, listener: TcpListener) -> Result<()> {
    fs::create_dir_all(vm.share_dir())?;
    fs::create_dir_all(vm.inbox())?;
    let server = Arc::new(Server {
        vm: vm.clone(),
        token: config.token.clone(),
        jobs: Jobs::default(),
    });
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let server = Arc::clone(&server);
        thread::spawn(move || {
            let _ = server.connection(stream);
        });
    }
    Ok(())
}

fn serve(vm: &Vm) -> Result<()> {
    let config = vm.config()?;
    let listener =
        TcpListener::bind(("127.0.0.1", config.port)).context(format!("port {}", config.port))?;
    serve_on(vm, &config, listener)
}

/// One HTTP/1.1 request to the job server on this host.
pub fn http(
    port: u16,
    token: &str,
    method: &str,
    path: &str,
    body: &[u8],
    timeout: f64,
) -> io::Result<(u16, Vec<u8>)> {
    let timeout = Duration::from_secs_f64(timeout);
    let mut stream =
        TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Token: {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    let mut reader = BufReader::new(stream);
    let status_line = read_line(&mut reader)?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse().ok())
        .ok_or_else(|| invalid("malformed status line"))?;
    let headers = read_headers(&mut reader)?;
    let mut response = Vec::new();
    match content_length(&headers)? {
        Some(length) => {
            response.resize(length, 0);
            reader.read_exact(&mut response)?;
        }
        None => {
            reader.read_to_end(&mut response)?;
        }
    }
    Ok((status, response))
}

// ---------------------------------------------------------------------------- QEMU

/// A connection to QEMU's monitor, which serves one client at a time.
pub struct Qmp {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Qmp {
    fn connect(socket: &Path, timeout: f64) -> Result<Self> {
        let stream = UnixStream::connect(socket)?;
        let timeout = Some(Duration::from_secs_f64(timeout));
        stream.set_read_timeout(timeout)?;
        stream.set_write_timeout(timeout)?;
        let mut monitor = Self {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
        };
        monitor.read()?;
        monitor.command("qmp_capabilities", None)?;
        Ok(monitor)
    }

    fn read(&mut self) -> Result<Value> {
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line)? == 0 {
                return Err("QMP connection closed".into());
            }
            let message: Value = serde_json::from_str(&line)?;
            if message.get("event").is_none() {
                return Ok(message);
            }
        }
    }

    pub fn command(&mut self, name: &str, arguments: Option<Value>) -> Result<Value> {
        let message = match arguments {
            Some(arguments) => json!({"execute": name, "arguments": arguments}),
            None => json!({"execute": name}),
        };
        self.writer.write_all(format!("{message}\n").as_bytes())?;
        let reply = self.read()?;
        if let Some(error) = reply.get("error") {
            return Err(
                format!("QMP {name}: {}", error["desc"].as_str().unwrap_or_default()).into(),
            );
        }
        Ok(reply.get("return").cloned().unwrap_or(Value::Null))
    }

    fn input(&mut self, events: Value) -> Result<()> {
        self.command("input-send-event", Some(json!({"events": events})))
            .map(drop)
    }

    pub fn key(&mut self, qcode: &str, down: bool) -> Result<()> {
        let qcode = lookup(ALIASES, &qcode).unwrap_or(qcode);
        self.input(json!([{"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": qcode}}}]))
    }

    /// Presses keys such as `ctrl+shift+z` in order and releases them in reverse.
    pub fn chord(&mut self, spec: &str) -> Result<()> {
        self.chord_held(spec, 0.06)
    }

    fn chord_held(&mut self, spec: &str, hold: f64) -> Result<()> {
        let keys: Vec<&str> = spec.split('+').collect();
        for key in &keys {
            self.key(key, true)?;
            sleep(0.02);
        }
        sleep(hold);
        for key in keys.iter().rev() {
            self.key(key, false)?;
            sleep(0.02);
        }
        Ok(())
    }

    /// Types ASCII text with US-layout keys.
    pub fn type_text(&mut self, text: &str) -> Result<()> {
        for character in text.chars() {
            let (key, shifted) = if character.is_ascii_alphabetic() {
                (
                    character.to_ascii_lowercase().to_string(),
                    character.is_ascii_uppercase(),
                )
            } else if character.is_ascii_digit() {
                (character.to_string(), false)
            } else if let Some(key) = lookup(US_SYMBOLS, &character) {
                (key.to_owned(), false)
            } else if let Some(key) = lookup(US_SHIFTED, &character) {
                (key.to_owned(), true)
            } else {
                return Err(format!("no US key for {character:?}").into());
            };
            self.chord(&if shifted { format!("shift+{key}") } else { key })?;
            sleep(0.04);
        }
        Ok(())
    }

    pub fn screenshot(&mut self, path: &Path) -> Result<PathBuf> {
        let path = std::path::absolute(path)?;
        self.command(
            "screendump",
            Some(json!({"filename": path, "format": "png"})),
        )?;
        for _ in 0..50 {
            if fs::metadata(&path).is_ok_and(|metadata| metadata.len() > 0) {
                return Ok(path);
            }
            sleep(0.05);
        }
        Err("screendump produced no file".into())
    }

    /// The guest display's size in pixels.
    pub fn size(&mut self) -> Result<(u32, u32)> {
        let directory = TempDir::new("textloom-winvm-")?;
        let data = fs::read(self.screenshot(&directory.path().join("size.png"))?)?;
        png_size(&data).ok_or_else(|| "the screenshot is not a PNG".into())
    }

    fn move_to(&mut self, x: i64, y: i64, (width, height): (u32, u32)) -> Result<()> {
        let scale = |value: i64, extent: u32| {
            (value as f64 * 32767.0 / f64::from(extent.max(2) - 1)).round_ties_even()
        };
        self.input(json!([
            {"type": "abs", "data": {"axis": "x", "value": scale(x, width) as i64}},
            {"type": "abs", "data": {"axis": "y", "value": scale(y, height) as i64}},
        ]))
    }

    fn button(&mut self, down: bool, button: &str) -> Result<()> {
        self.input(json!([{"type": "btn", "data": {"down": down, "button": button}}]))
    }

    /// Clicks at a screen position; `size` saves a screenshot to measure it.
    pub fn click(&mut self, x: i64, y: i64, button: &str, size: Option<(u32, u32)>) -> Result<()> {
        let size = match size {
            Some(size) => size,
            None => self.size()?,
        };
        self.move_to(x, y, size)?;
        sleep(0.05);
        self.button(true, button)?;
        sleep(0.06);
        self.button(false, button)
    }
}

fn png_size(data: &[u8]) -> Option<(u32, u32)> {
    let width = data.get(16..20)?.try_into().ok().map(u32::from_be_bytes)?;
    let height = data.get(20..24)?.try_into().ok().map(u32::from_be_bytes)?;
    Some((width, height))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Audio {
    Wav,
    Pipewire,
    None,
}

impl Audio {
    const ALL: [Self; 3] = [Self::Wav, Self::Pipewire, Self::None];

    fn name(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Pipewire => "pipewire",
            Self::None => "none",
        }
    }
}

fn qemu_command(vm: &Vm, install: bool, view: bool, audio: Audio) -> Vec<String> {
    let memory = env::var("TEXTLOOM_WINVM_MEMORY").unwrap_or_else(|_| "4096".to_owned());
    let cpus = env::var("TEXTLOOM_WINVM_CPUS").unwrap_or_else(|_| "4".to_owned());
    let mut command: Vec<String> = [
        "qemu-system-x86_64",
        "-name",
        "textloom-win11",
        "-machine",
        "q35,accel=kvm",
        "-cpu",
        "host,hv_relaxed,hv_vapic,hv_spinlocks=0x1fff,hv_time,hv_vpindex,hv_synic,hv_stimer",
        "-smp",
        &cpus,
        "-m",
        &memory,
        "-rtc",
        "base=localtime",
        "-drive",
        &format!("if=pflash,format=raw,readonly=on,file={OVMF_CODE}"),
        "-drive",
        &format!("if=pflash,format=raw,file={}", vm.vars().display()),
        "-drive",
        &format!(
            "file={},if=none,id=hd0,format=qcow2,discard=unmap",
            vm.disk().display()
        ),
        "-device",
        &format!("ide-hd,drive=hd0,bus=ide.0,bootindex={}", u8::from(install)),
        "-netdev",
        "user,id=net0",
        "-device",
        "e1000e,netdev=net0",
        "-device",
        "qemu-xhci",
        "-device",
        "usb-tablet",
        "-vga",
        "none",
        "-device",
        "VGA,edid=on,xres=1280,yres=800",
        "-qmp",
        &format!("unix:{},server=on,wait=off", vm.qmp_socket().display()),
        "-pidfile",
        &vm.qemu_pid().display().to_string(),
        "-display",
        if view { "gtk" } else { "none" },
        "-audiodev",
        &match audio {
            Audio::Wav => format!("wav,id=snd0,path={}", vm.audio().display()),
            Audio::Pipewire => "pipewire,id=snd0".to_owned(),
            Audio::None => "none,id=snd0".to_owned(),
        },
        "-device",
        "ich9-intel-hda",
        "-device",
        "hda-output,audiodev=snd0",
    ]
    .iter()
    .map(|argument| (*argument).to_owned())
    .collect();
    if install {
        command.extend([
            "-drive".to_owned(),
            format!(
                "file={},media=cdrom,if=none,id=cd0,readonly=on",
                vm.iso().display()
            ),
            "-device".to_owned(),
            "ide-cd,drive=cd0,bus=ide.1,bootindex=0".to_owned(),
            "-drive".to_owned(),
            format!(
                "file={},media=cdrom,if=none,id=cd1,readonly=on",
                vm.unattend().display()
            ),
            "-device".to_owned(),
            "ide-cd,drive=cd1,bus=ide.2".to_owned(),
        ]);
    }
    command
}

fn start_qemu(vm: &Vm, install: bool, view: bool, audio: Audio) -> Result<()> {
    if alive(&vm.qemu_pid()).is_some() {
        return Err("the VM is already running".into());
    }
    let _ = fs::remove_file(vm.qmp_socket());
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(vm.state.join("qemu.log"))?;
    let command = qemu_command(vm, install, view, audio);
    Command::new(&command[0])
        .args(&command[1..])
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()
        .context(&command[0])?;
    for _ in 0..100 {
        if alive(&vm.qemu_pid()).is_some() && vm.qmp_socket().exists() {
            return Ok(());
        }
        sleep(0.1);
    }
    Err("QEMU did not start; see qemu.log".into())
}

fn which(tool: &str) -> bool {
    env::var_os("PATH")
        .is_some_and(|path| env::split_paths(&path).any(|directory| directory.join(tool).is_file()))
}

fn require_tools() -> Result<()> {
    let mut missing: Vec<&str> = ["qemu-system-x86_64", "qemu-img", "xorriso"]
        .into_iter()
        .filter(|tool| !which(tool))
        .collect();
    missing.extend(
        [OVMF_CODE, OVMF_VARS]
            .into_iter()
            .filter(|path| !Path::new(path).exists()),
    );
    if !missing.is_empty() {
        return Err(format!(
            "missing {}; install them with: sudo pacman -S --needed qemu-desktop edk2-ovmf libisoburn",
            missing.join(", ")
        )
        .into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------- commands

fn here() -> PathBuf {
    repo_root().join("scripts/windows-vm")
}

fn build_unattend(vm: &Vm) -> Result<()> {
    let config = vm.config()?;
    let directory = TempDir::new("textloom-winvm-unattend-")?;
    let root = directory.path();
    fs::copy(
        here().join("autounattend.xml"),
        root.join("autounattend.xml"),
    )?;
    for name in ["firstlogon.ps1", "agent.ps1"] {
        fs::copy(here().join("guest").join(name), root.join(name))?;
    }
    fs::write(root.join("agent.json"), serde_json::to_string(&config)?)?;
    let (output, source) = (
        vm.unattend().display().to_string(),
        root.display().to_string(),
    );
    exec(
        "xorriso",
        &[
            "-as", "mkisofs", "-quiet", "-J", "-r", "-V", "TLQA", "-o", &output, &source,
        ],
    )
}

fn create(vm: &Vm, force: bool, resume: bool) -> Result<()> {
    require_tools()?;
    let started = if resume {
        // Wait for an install that is already running, for example after this tool was interrupted.
        if alive(&vm.qemu_pid()).is_none() {
            return Err("no install is running".into());
        }
        vm.ensure_server()?;
        0.0
    } else {
        if vm.disk().exists() && !force {
            return Err(format!(
                "{} exists; pass --force to reinstall Windows",
                vm.disk().display()
            )
            .into());
        }
        if alive(&vm.qemu_pid()).is_some() {
            return Err("the VM is running; run `cargo winvm down` first".into());
        }
        fs::create_dir_all(&vm.state)?;
        if fs::metadata(vm.iso()).map_or(true, |metadata| metadata.len() != ISO_SIZE) {
            println!("Downloading the Windows 11 Enterprise LTSC evaluation ISO (5.1 GB)...");
            let iso = vm.iso().display().to_string();
            exec(
                "curl",
                &[
                    "-L", "--fail", "--retry", "3", "-C", "-", "-o", &iso, ISO_URL,
                ],
            )?;
        }
        build_unattend(vm)?;
        let _ = fs::remove_file(vm.disk());
        exec(
            "qemu-img",
            &[
                "create",
                "-q",
                "-f",
                "qcow2",
                &vm.disk().display().to_string(),
                "64G",
            ],
        )?;
        fs::copy(OVMF_VARS, vm.vars())?;
        vm.ensure_server()?;
        let started = unix_now();
        start_qemu(vm, true, false, Audio::None)?;
        let mut monitor = vm.qmp(30.0)?;
        // The Windows disc waits for a key before booting.
        for _ in 0..60 {
            monitor.chord("ret")?;
            sleep(0.5);
        }
        started
    };
    let progress_png = vm.state.join("progress.png");
    let progress = |elapsed: f64| {
        if elapsed as u64 % 120 < 10 {
            if let Ok(mut monitor) = vm.qmp(30.0) {
                let _ = monitor.screenshot(&progress_png);
            }
            println!(
                "  installing: {} min (screen: {})",
                elapsed as u64 / 60,
                progress_png.display()
            );
        }
    };
    println!("Installing Windows unattended; this takes about 30 minutes.");
    if !vm.wait_for_agent(150.0 * 60.0, started, progress) {
        return Err(format!(
            "the guest agent did not start; the VM is still running (screen: {})",
            progress_png.display()
        )
        .into());
    }
    let result = vm.run(
        "(Get-CimInstance Win32_OperatingSystem) | ForEach-Object { \"$($_.Caption) $($_.Version)\" }",
        false,
        600,
    )?;
    println!("Installed: {}", result.output.trim());
    down(vm, false)?;
    snapshot_create(vm, "installed")?;
    println!("Saved snapshot 'installed'. Next: cargo winvm provision");
    Ok(())
}

fn provision(vm: &Vm) -> Result<()> {
    up_vm(vm, false, Audio::None)?;
    vm.share(&here().join("guest/provision.ps1"), None)?;
    vm.share(&here().join("guest/agent.ps1"), None)?;
    println!("Provisioning; this downloads about 1.5 GB and takes 20 to 40 minutes.");
    // The new agent takes over at the restart below.
    let result = vm.run(
        "New-Item -ItemType Directory -Force C:\\textloom-qa | Out-Null\n\
         Get-ChildItem C:\\textloom-qa -File | ForEach-Object { $_.IsReadOnly = $false }\n\
         Get-HostFile agent.ps1 C:\\textloom-qa\\agent.ps1\n\
         Get-HostFile provision.ps1 C:\\textloom-qa\\provision.ps1\n\
         & C:\\textloom-qa\\provision.ps1",
        true,
        4 * 3600,
    )?;
    println!("{}", result.output);
    if result.exit != 0 {
        return Err("provisioning failed; the VM is still running for inspection".into());
    }
    let restarted = unix_now();
    vm.run("shutdown /r /t 5", true, 60)?;
    sleep(60.0);
    // A long poll begun just before the restart can end up to 30 s later.
    if !vm.wait_for_agent(15.0 * 60.0, restarted + 60.0, |_| {}) {
        return Err("the guest agent did not return after the restart".into());
    }
    println!(
        "{}",
        vm.run("& C:\\textloom-qa\\provision.ps1 -Verify", false, 300)?
            .output
    );
    down(vm, false)?;
    snapshot_create(vm, "provisioned")?;
    println!("Saved snapshot 'provisioned'.");
    Ok(())
}

fn up_vm(vm: &Vm, view: bool, audio: Audio) -> Result<()> {
    if !vm.disk().exists() {
        return Err("no VM yet; run `cargo winvm create`".into());
    }
    vm.ensure_server()?;
    if alive(&vm.qemu_pid()).is_some() {
        if vm.wait_for_agent(60.0, 0.0, |_| {}) {
            return Ok(());
        }
        return Err("the VM is running but its agent is not responding".into());
    }
    let started = unix_now();
    start_qemu(vm, false, view, audio)?;
    if !vm.wait_for_agent(10.0 * 60.0, started, |_| {}) {
        return Err("the guest agent did not start within 10 minutes".into());
    }
    Ok(())
}

fn down(vm: &Vm, announce: bool) -> Result<()> {
    if let Some(pid) = alive(&vm.qemu_pid()) {
        if let Ok(mut monitor) = vm.qmp(10.0) {
            let _ = monitor.command("system_powerdown", None);
        }
        let mut stopped = false;
        for _ in 0..180 {
            stopped = alive(&vm.qemu_pid()).is_none();
            if stopped {
                break;
            }
            sleep(1.0);
        }
        if !stopped {
            terminate(pid);
            sleep(3.0);
        }
    }
    if let Some(server) = alive(&vm.server_pid()) {
        terminate(server);
    }
    for path in [vm.qemu_pid(), vm.server_pid(), vm.qmp_socket()] {
        let _ = fs::remove_file(path);
    }
    if announce {
        println!("The VM is down.");
    }
    Ok(())
}

fn status(vm: &Vm) {
    let running = |pid_file: PathBuf| {
        if alive(&pid_file).is_some() {
            "running"
        } else {
            "stopped"
        }
    };
    println!("VM: {}", running(vm.qemu_pid()));
    println!("job server: {}", running(vm.server_pid()));
    let responding = alive(&vm.server_pid()).is_some() && vm.agent_seen(0.0);
    println!(
        "agent: {}",
        if responding {
            "responding"
        } else {
            "not responding"
        }
    );
    println!("state: {}", vm.state.display());
}

fn snapshot_create(vm: &Vm, name: &str) -> Result<()> {
    if alive(&vm.qemu_pid()).is_some() {
        return Err("snapshots need the VM down".into());
    }
    exec(
        "qemu-img",
        &["snapshot", "-c", name, &vm.disk().display().to_string()],
    )?;
    fs::create_dir_all(vm.state.join("vars"))?;
    fs::copy(vm.vars(), vm.state.join("vars").join(format!("{name}.fd")))?;
    Ok(())
}

fn restore(vm: &Vm, name: &str) -> Result<()> {
    if alive(&vm.qemu_pid()).is_some() {
        return Err("restoring needs the VM down".into());
    }
    exec(
        "qemu-img",
        &["snapshot", "-a", name, &vm.disk().display().to_string()],
    )?;
    fs::copy(vm.state.join("vars").join(format!("{name}.fd")), vm.vars())?;
    println!("Restored snapshot '{name}'.");
    Ok(())
}

fn run_command(vm: &Vm, script: &str, admin: bool, timeout: u64) -> Result<ExitCode> {
    let script = if script == "-" {
        let mut script = String::new();
        io::stdin().read_to_string(&mut script)?;
        script
    } else {
        script.to_owned()
    };
    let result = vm.run(&script, admin, timeout)?;
    print!("{}", result.output);
    io::stdout().flush()?;
    if result.timed_out {
        return Err(format!("the job timed out after {timeout} s").into());
    }
    Ok(ExitCode::from(result.exit as u8))
}

enum Action {
    Serve,
    Create {
        force: bool,
        resume: bool,
    },
    Provision,
    Up {
        view: bool,
        audio: Audio,
    },
    Down,
    Status,
    Run {
        admin: bool,
        timeout: u64,
        script: String,
    },
    Share {
        file: PathBuf,
        name: Option<String>,
    },
    Screenshot(PathBuf),
    Keys(Vec<String>),
    Type(String),
    Click {
        x: i64,
        y: i64,
        right: bool,
    },
    Snapshot(String),
    Restore(String),
    Snapshots,
}

fn parse(arguments: Vec<String>) -> Result<Action, String> {
    let mut arguments = arguments.into_iter();
    let command = arguments.next().ok_or("a command is required")?;
    let mut rest = Arguments::new(arguments);
    let number = |text: &str| {
        text.parse::<i64>()
            .map_err(|_| format!("{text} is not a whole number"))
    };
    let plain = match command.as_str() {
        "serve" => Some(Action::Serve),
        "provision" => Some(Action::Provision),
        "down" => Some(Action::Down),
        "status" => Some(Action::Status),
        "snapshots" => Some(Action::Snapshots),
        _ => None,
    };
    if let Some(action) = plain {
        let [] = rest.finish_exact()?;
        return Ok(action);
    }
    Ok(match command.as_str() {
        "create" => {
            let (force, resume) = (rest.flag("--force"), rest.flag("--resume"));
            let [] = rest.finish_exact()?;
            Action::Create { force, resume }
        }
        "up" => {
            let view = rest.flag("--view");
            let audio = rest
                .choice("--audio", &Audio::ALL, Audio::name)?
                .unwrap_or(Audio::Wav);
            let [] = rest.finish_exact()?;
            Action::Up { view, audio }
        }
        "run" => {
            let admin = rest.flag("--admin");
            let timeout = match rest.option("--timeout")? {
                Some(timeout) => timeout
                    .parse()
                    .map_err(|_| "--timeout takes whole seconds".to_owned())?,
                None => 600,
            };
            let [script] = rest.finish_exact()?;
            Action::Run {
                admin,
                timeout,
                script,
            }
        }
        "share" => {
            let mut positionals = rest.finish()?.into_iter();
            let file = positionals.next().ok_or("share needs a FILE")?;
            let name = positionals.next();
            if positionals.next().is_some() {
                return Err("share takes FILE and an optional NAME".to_owned());
            }
            Action::Share {
                file: file.into(),
                name,
            }
        }
        "screenshot" => {
            let [file] = rest.finish_exact()?;
            Action::Screenshot(file.into())
        }
        "keys" => {
            let chords = rest.finish()?;
            if chords.is_empty() {
                return Err("keys needs at least one CHORD".to_owned());
            }
            Action::Keys(chords)
        }
        "type" => {
            let [text] = rest.finish_exact()?;
            Action::Type(text)
        }
        "click" => {
            let right = rest.flag("--right");
            let [x, y] = rest.finish_exact()?;
            Action::Click {
                x: number(&x)?,
                y: number(&y)?,
                right,
            }
        }
        "snapshot" | "restore" => {
            let [name] = rest.finish_exact()?;
            if command == "snapshot" {
                Action::Snapshot(name)
            } else {
                Action::Restore(name)
            }
        }
        _ => return Err(format!("unknown command {command}")),
    })
}

fn execute(vm: &Vm, action: Action) -> Result<ExitCode> {
    match action {
        Action::Serve => serve(vm)?,
        Action::Create { force, resume } => create(vm, force, resume)?,
        Action::Provision => provision(vm)?,
        Action::Up { view, audio } => {
            up_vm(vm, view, audio)?;
            println!("The VM is up and its agent is responding.");
        }
        Action::Down => down(vm, true)?,
        Action::Status => status(vm),
        Action::Run {
            admin,
            timeout,
            script,
        } => return run_command(vm, &script, admin, timeout),
        Action::Share { file, name } => println!("{}", vm.share(&file, name.as_deref())?),
        Action::Screenshot(file) => println!("{}", vm.qmp(30.0)?.screenshot(&file)?.display()),
        Action::Keys(chords) => {
            let mut monitor = vm.qmp(30.0)?;
            for chord in chords {
                monitor.chord(&chord)?;
                sleep(0.1);
            }
        }
        Action::Type(text) => vm.qmp(30.0)?.type_text(&text)?,
        Action::Click { x, y, right } => {
            vm.qmp(30.0)?
                .click(x, y, if right { "right" } else { "left" }, None)?
        }
        Action::Snapshot(name) => {
            snapshot_create(vm, &name)?;
            println!("Saved snapshot '{name}'.");
        }
        Action::Restore(name) => restore(vm, &name)?,
        Action::Snapshots => exec(
            "qemu-img",
            &["snapshot", "-l", &vm.disk().display().to_string()],
        )?,
    }
    Ok(ExitCode::SUCCESS)
}

/// The `winvm` command.
pub fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    if arguments.is_empty()
        || arguments
            .iter()
            .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let action = match parse(arguments) {
        Ok(action) => action,
        Err(message) => {
            eprintln!("{USAGE}\n\nwinvm: error: {message}");
            return ExitCode::from(2);
        }
    };
    execute(&Vm::from_env(), action).unwrap_or_else(|error| {
        eprintln!("winvm: {error}");
        ExitCode::FAILURE
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Harness {
        vm: Vm,
        port: u16,
        _state: TempDir,
    }

    fn harness() -> Harness {
        let state = TempDir::new("textloom-winvm-test-").unwrap();
        let vm = Vm {
            state: state.path().to_path_buf(),
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = Config {
            port,
            token: "secret".to_owned(),
        };
        fs::write(vm.config_path(), serde_json::to_string(&config).unwrap()).unwrap();
        let server = vm.clone();
        thread::spawn(move || serve_on(&server, &config, listener));
        Harness {
            vm,
            port,
            _state: state,
        }
    }

    impl Harness {
        fn guest(&self, method: &str, path: &str, body: &[u8], token: &str) -> (u16, Vec<u8>) {
            http(self.port, token, method, path, body, 40.0).unwrap()
        }
    }

    #[test]
    fn a_job_round_trips_through_a_polling_guest() {
        let harness = harness();
        thread::scope(|scope| {
            scope.spawn(|| {
                let (status, body) = harness.guest("GET", "/next", b"", "secret");
                assert_eq!(status, 200);
                let job: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(job["admin"], false);
                assert_eq!(job["timeout"], 30);
                let result = json!({"exit": 0, "timed_out": false, "output": format!("ran {}", job["script"].as_str().unwrap())});
                // Windows PowerShell may send a byte order mark.
                let mut body = b"\xef\xbb\xbf".to_vec();
                body.extend(result.to_string().as_bytes());
                let path = format!("/result/{}", job["id"].as_str().unwrap());
                assert_eq!(harness.guest("POST", &path, &body, "secret").0, 204);
            });
            let result = harness.vm.run("Get-Date", false, 30).unwrap();
            assert_eq!(result.output, "ran Get-Date");
            assert_eq!(result.exit, 0);
        });
        assert!(harness.vm.agent_seen(0.0));
    }

    #[test]
    fn requests_need_the_token() {
        let harness = harness();
        assert_eq!(harness.guest("GET", "/status", b"", "wrong").0, 403);
        assert_eq!(harness.guest("GET", "/status", b"", "").0, 403);
        assert_eq!(harness.guest("GET", "/status", b"", "secret").0, 200);
    }

    #[test]
    fn shared_names_cannot_leave_the_share() {
        let harness = harness();
        for name in ["..%2Fconfig.json", ".hidden", "", "a/b"] {
            assert_eq!(
                harness
                    .guest("GET", &format!("/share/{name}"), b"", "secret")
                    .0,
                400,
                "{name}"
            );
            assert_eq!(
                harness
                    .guest("PUT", &format!("/inbox/{name}"), b"x", "secret")
                    .0,
                400,
                "{name}"
            );
        }
        assert_eq!(harness.guest("GET", "/share/missing", b"", "secret").0, 404);
    }

    #[test]
    fn files_move_through_the_share_and_inbox() {
        let harness = harness();
        let source = harness.vm.state.join("source.txt");
        fs::write(&source, "shared").unwrap();
        let name = harness.vm.share(&source, None).unwrap();
        assert_eq!(
            harness.guest("GET", &format!("/share/{name}"), b"", "secret"),
            (200, b"shared".to_vec())
        );
        assert_eq!(
            harness
                .guest("PUT", "/inbox/upload.png", b"PNG", "secret")
                .0,
            204
        );
        assert_eq!(
            fs::read(harness.vm.inbox().join("upload.png")).unwrap(),
            b"PNG"
        );
    }

    #[test]
    fn windows_powershell_bodies_after_100_continue() {
        let harness = harness();
        let mut stream = TcpStream::connect(("127.0.0.1", harness.port)).unwrap();
        write!(
            stream,
            "PUT /inbox/expect.txt HTTP/1.1\r\nx-token: secret\r\nExpect: 100-continue\r\nContent-Length: 4\r\n\r\n"
        )
        .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        assert_eq!(read_line(&mut reader).unwrap(), "HTTP/1.1 100 Continue");
        assert_eq!(read_line(&mut reader).unwrap(), "");
        stream.write_all(b"body").unwrap();
        assert_eq!(read_line(&mut reader).unwrap(), "HTTP/1.1 204 No Content");
        assert_eq!(
            fs::read(harness.vm.inbox().join("expect.txt")).unwrap(),
            b"body"
        );
    }

    #[test]
    fn unknown_paths_and_methods() {
        let harness = harness();
        assert_eq!(harness.guest("GET", "/elsewhere", b"", "secret").0, 404);
        assert_eq!(harness.guest("DELETE", "/status", b"", "secret").0, 501);
        assert_eq!(harness.guest("POST", "/run", b"{}", "secret").0, 400);
    }

    #[test]
    fn png_dimensions_and_key_tables() {
        let mut header = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        header.extend(1280u32.to_be_bytes());
        header.extend(800u32.to_be_bytes());
        assert_eq!(png_size(&header), Some((1280, 800)));
        assert_eq!(png_size(b"short"), None);
        assert_eq!(lookup(ALIASES, &"altgr"), Some("alt_r"));
        assert_eq!(lookup(US_SHIFTED, &'?'), Some("slash"));
    }
}
