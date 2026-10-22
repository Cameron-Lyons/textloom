//! `winvm` (`cargo winvm`): creates, runs, and drives the local Windows 11
//! VM. See [`textloom_qa_tools::winvm`].

use std::process::ExitCode;

#[cfg(unix)]
fn main() -> ExitCode {
    textloom_qa_tools::winvm::main()
}

#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("winvm runs the Windows VM with QEMU/KVM on Linux");
    ExitCode::FAILURE
}
