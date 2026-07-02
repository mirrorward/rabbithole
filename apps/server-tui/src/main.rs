//! `burrow-tui` — the sysop console.
//!
//! Wave 2 v1: a live connection monitor over the burrow's local ctl socket
//! (`<data_dir>/ctl.sock`). Shows server status and who's online, refreshed
//! on a timer; `q` / Esc / Ctrl-C to quit, `r` to refresh now. Config
//! editing and account management arrive alongside the richer admin work.
//!
//! The console talks to the burrow over a Unix domain socket, so the binary
//! is Unix-only for now (matching `burrow ctl`); a Windows named-pipe path
//! lands with the Windows server story.

#[cfg(unix)]
mod console;

#[cfg(unix)]
fn main() -> anyhow::Result<()> {
    console::run()
}

#[cfg(not(unix))]
fn main() {
    eprintln!(
        "burrow-tui talks to the burrow over a Unix domain socket (ctl.sock), \
         which isn't supported on this platform yet. Use `burrow ctl` on the \
         server host, or the remote admin protocol from any client."
    );
    std::process::exit(1);
}
