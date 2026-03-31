# redline-log

Fast structured logging for `tracing`, published as `redline-log` with the Rust crate name `redline`.

## Example

```rust
use redline::{Builder, Sink};
use tracing::info;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let handle = Builder::new()
        .filter_spec("info,my_app::db=debug")?
        .sink(Sink::Stdout)
        .install_global()?;

    info!(target: "my_app::http", request_id = 42u64, message = "request_started");
    handle.flush()?;
    Ok(())
}
```

## Related Packages

- [`redline-core`](https://crates.io/crates/redline-core): shared filter, encoding, and record types
- [`redline-decode`](https://crates.io/crates/redline-decode): binary log decoder CLI
- [`redline-layer`](https://crates.io/crates/redline-layer): [`tracing-subscriber`](https://crates.io/crates/tracing-subscriber) layer built on the same pipeline
