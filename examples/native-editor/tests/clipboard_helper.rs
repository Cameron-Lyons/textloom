//! Checks that a Linux clipboard copy helper exits when its host process is
//! killed, so a crashed host never leaves a helper serving the clipboard.

#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[test]
fn copy_helper_exits_when_its_host_is_killed() {
    let script = r#"exec 3<&0
TEXTLOOM_CLIPBOARD_PARENT=$$ "$0" --native-clipboard-copy 0 <&3 3<&- &
echo $!
exec sleep 30"#;
    let mut host = Command::new("sh")
        .args(["-c", script, env!("CARGO_BIN_EXE_textloom-native-example")])
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let input = host.stdin.take().unwrap();
    let mut output = BufReader::new(host.stdout.take().unwrap());
    let mut helper = String::new();
    output.read_line(&mut helper).unwrap();
    let helper = helper.trim().to_owned();
    thread::sleep(Duration::from_millis(200));
    host.kill().unwrap();
    host.wait().unwrap();

    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = output.read_to_end(&mut Vec::new());
        let _ = sender.send(());
    });
    let exited = receiver.recv_timeout(Duration::from_secs(5)).is_ok();
    if !exited {
        let _ = Command::new("kill").args(["-9", &helper]).status();
    }
    drop(input);
    assert!(exited, "clipboard helper {helper} outlived its killed host");
}
