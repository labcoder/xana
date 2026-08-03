mod config;

use anyhow::Context;
use config::{XanaConfig, config_path};

fn main() -> anyhow::Result<()> {
    let path = config_path().context("could not resolve Xana config path")?;

    println!("loading Xana config from {}", path.display());

    let config = XanaConfig::load_from(&path)
        .with_context(|| format!("failed to load config from {}", path.display()))?;

    println!("model = {}", config.model);
    println!("base_url = {}", config.base_url);

    Ok(())
}
