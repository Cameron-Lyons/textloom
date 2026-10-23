//! `winvm-checks` (`cargo winvm-checks`): builds the native host for Windows
//! and runs Textloom's Windows checks in the `winvm` VM. See
//! [`textloom_qa_tools::winvm_checks`].

use std::process::ExitCode;

#[cfg(unix)]
fn main() -> ExitCode {
    textloom_qa_tools::winvm_checks::main()
}

#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("winvm-checks drives the Windows VM from a Linux host");
    ExitCode::FAILURE
}
