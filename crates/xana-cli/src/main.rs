//! Installed Xana command. Process and terminal concerns are composed here;
//! the runtime crate remains reusable by tests and future frontends.

fn main() -> anyhow::Result<()> {
    xana_runtime::run()
}
