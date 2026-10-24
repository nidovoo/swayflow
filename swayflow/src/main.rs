//! swayflow — floating/tiling toggle, auto-placement daemon and workspace
//! overview for the Sway window manager.
//!
//! ```text
//! swayflow toggle     switch the focused workspace between tiling and auto-arranged floating
//! swayflow overview   open (or close) the interactive workspace overview
//! swayflow daemon     event loop that keeps new windows of floating workspaces arranged
//!                     (started automatically by `toggle`)
//! ```

mod commands;
mod error;
mod floating;
mod geom;
mod ipc;
mod overview;
mod paths;
mod sim;
mod state;
mod sys;
mod tree;

use std::process::ExitCode;

const USAGE: &str = "usage: swayflow toggle | overview | daemon";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    let result = match args.first().map(String::as_str) {
        Some("toggle") => commands::cmd_toggle(),
        Some("daemon") => commands::cmd_daemon(),
        Some("overview") => overview::cmd_overview(args.get(1..).unwrap_or_default()),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("swayflow: {err}");
            ExitCode::FAILURE
        }
    }
}
