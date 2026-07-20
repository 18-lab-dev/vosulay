mod ffi;

use anyhow::{Context, Result};

fn main() -> Result<()> {
    let arguments = std::env::args_os()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let arguments_json =
        serde_json::to_string(&arguments).context("failed to serialize command-line arguments")?;
    let exit_code = ffi::run_qt_application(&arguments_json);

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}
