//! `check-native-report`: validates a native host `--qa-report` file against
//! the run it should come from. See [`textloom_qa_tools::native_report`].

use std::process::ExitCode;

fn main() -> ExitCode {
    ExitCode::from(textloom_qa_tools::native_report::main(
        std::env::args().skip(1),
    ))
}
