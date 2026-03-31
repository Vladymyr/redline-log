use redline::{Builder, Sink};
use tracing::{info, info_span};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = Builder::new()
        .sink(Sink::Stdout)
        .include_current_span(true)
        .build()?;
    let handle = subscriber.handle();

    tracing::subscriber::with_default(subscriber, || {
        let job = info_span!("job", name = "backfill", shard = 3u64);
        let _guard = job.enter();

        info!(
            target: "jobs::backfill",
            records = 1200u64,
            message = "job_started"
        );
        info!(
            target: "jobs::backfill",
            records = 1200u64,
            message = "job_finished"
        );
    });

    handle.flush()?;
    Ok(())
}
