//! Installed Xana executable facade.
//!
//! Process composition currently lives in `xana-runtime::entry`; future
//! frontends consume Xana's application contract rather than this binary.

fn main() -> std::process::ExitCode {
    xana_runtime::entry()
}
