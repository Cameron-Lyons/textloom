//! Checks the build configuration that lets followed links open: without
//! egui-winit's `links` feature, an `OpenUrl` command only logs a warning.

use std::process::Command;

#[test]
fn followed_links_reach_the_system_browser() {
    let output = Command::new(env!("CARGO"))
        .args([
            "tree",
            "--offline",
            "--locked",
            "--manifest-path",
            concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"),
            "--edges",
            "features",
            "--invert",
            "egui-winit",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8(output.stdout).unwrap();
    assert!(
        tree.contains("egui-winit feature \"links\""),
        "egui-winit lacks its links feature, so OpenUrl only logs a warning:\n{tree}"
    );
}
