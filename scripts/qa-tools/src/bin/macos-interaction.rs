#[cfg(unix)]
fn main() -> std::process::ExitCode {
    textloom_qa_tools::macos_interaction::main()
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("This clipboard/input probe runs only on an isolated hosted macOS Actions runner");
    std::process::ExitCode::FAILURE
}
