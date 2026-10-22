#[cfg(unix)]
fn main() -> std::process::ExitCode {
    textloom_qa_tools::winvm::main()
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("winvm runs the Windows VM with QEMU/KVM on Linux");
    std::process::ExitCode::FAILURE
}
