use std::process::ExitCode;

fn main() -> ExitCode {
    ExitCode::from(textloom_qa_tools::native_report::main(
        std::env::args().skip(1),
    ))
}
