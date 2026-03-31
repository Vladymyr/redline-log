use redline::{Builder, Sink};
use tracing::{info, warn};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let handle = Builder::new().sink(Sink::Stdout).install_global()?;

    info!(
        target: "app::startup",
        service = "api",
        version = env!("CARGO_PKG_VERSION"),
        message = "service_ready"
    );
    warn!(
        target: "app::startup",
        config_source = "defaults",
        message = "using_default_settings"
    );

    handle.flush()?;
    Ok(())
}
