//! `macos-interaction`: the hosted macOS interaction probe. It runs only on a
//! GitHub Actions macOS runner; see [`textloom_qa_tools::macos_interaction`].

use std::process::ExitCode;

#[cfg(unix)]
fn main() -> ExitCode {
    textloom_qa_tools::macos_interaction::main()
}

#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("This clipboard/input probe runs only on an isolated hosted macOS Actions runner");
    ExitCode::FAILURE
}
