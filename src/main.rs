mod ffi;

use anyhow::{Context, Result};
use vosulay::remote::{RemoteServer, vosu_base_url_from_arguments};

fn main() -> Result<()> {
    let arguments = std::env::args_os()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let remote_server = match vosu_base_url_from_arguments(&arguments) {
        Ok(base_url) => match RemoteServer::start(base_url) {
            Ok(server) => Some(server),
            Err(error) => {
                eprintln!("Remote overlay hosting is unavailable: {error:#}");
                None
            }
        },
        Err(error) => {
            eprintln!("Remote overlay hosting is unavailable: {error:#}");
            None
        }
    };
    let arguments_json =
        serde_json::to_string(&arguments).context("failed to serialize command-line arguments")?;
    let exit_code = ffi::run_qt_application(&arguments_json);
    drop(remote_server);

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}
