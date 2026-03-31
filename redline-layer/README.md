# redline-layer

[`tracing-subscriber`](https://crates.io/crates/tracing-subscriber) layer for [`redline-log`](https://crates.io/crates/redline-log), for use inside a broader `tracing-subscriber` stack.

## Example

```rust
use redline::Sink;
use redline_layer::Builder;
use tracing::info;
use tracing_subscriber::{prelude::*, registry};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let layer = Builder::new().sink(Sink::Stdout).build()?;
    let handle = layer.handle();
    let subscriber = registry().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        info!(target: "app", message = "started");
    });

    handle.flush()?;
    Ok(())
}
```
