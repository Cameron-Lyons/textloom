#[cfg(unix)]
fn main() -> std::process::ExitCode {
    textloom_qa_tools::winvm_checks::main()
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("winvm-checks drives the Windows VM from a Linux host");
    std::process::ExitCode::FAILURE
}
