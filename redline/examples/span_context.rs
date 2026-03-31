use redline::{Builder, Sink};
use tracing::{info, info_span};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let handle = Builder::new()
        .sink(Sink::Stdout)
        .include_current_span(true)
        .include_span_list(true)
        .install_global()?;

    let session = info_span!("session", account_id = 7u64, region = "eu-west");
    let _session = session.enter();

    let request = info_span!("request", request_id = 42u64, route = "/checkout");
    let _request = request.enter();

    info!(
        target: "app::checkout",
        step = "authorize",
        ok = true,
        message = "payment_checked"
    );
    info!(
        target: "app::checkout",
        step = "commit",
        ok = true,
        message = "order_created"
    );

    handle.flush()?;
    Ok(())
}
