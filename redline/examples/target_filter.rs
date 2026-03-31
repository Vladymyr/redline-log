use redline::{Builder, Sink};
use tracing::{debug, error, info, trace, warn};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let handle = Builder::new()
        .filter_spec("warn,app::http=info,app::db=trace")?
        .sink(Sink::Stdout)
        .install_global()?;

    trace!(
        target: "app::db",
        sql = "select 1",
        rows = 1u64,
        message = "query_started"
    );
    debug!(
        target: "app::http",
        route = "/health",
        message = "request_debug"
    );
    info!(
        target: "app::http",
        route = "/health",
        status = 200u16,
        message = "request_complete"
    );
    warn!(
        target: "app::cache",
        key = "profile:42",
        message = "cache_miss"
    );
    error!(
        target: "app::worker",
        job_id = 7u64,
        reason = "timeout",
        message = "job_failed"
    );

    handle.flush()?;
    Ok(())
}
