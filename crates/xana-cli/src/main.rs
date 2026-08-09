//! Installed Xana command. Process and terminal concerns are composed here;
//! the runtime crate remains reusable by tests and future frontends.

fn main() -> std::process::ExitCode {
    xana_runtime::entry()
}
