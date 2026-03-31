use std::{env, path::PathBuf};

use redline::{Builder, Sink};
use tracing::{debug, info};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| env::temp_dir().join("redline-example.ndjson"));

    let handle = Builder::new()
        .filter_spec("info,app::db=debug")?
        .sink(Sink::file(&path))
        .install_global()?;

    info!(
        target: "app::http",
        request_id = 42u64,
        method = "GET",
        route = "/health",
        status = 200u16,
        message = "request_complete"
    );
    debug!(
        target: "app::db",
        rows = 1u64,
        query = "select 1",
        message = "query_complete"
    );

    handle.flush()?;
    println!("wrote {}", path.display());
    Ok(())
}
