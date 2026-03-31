# redline-core

Core types for [`redline-log`](https://crates.io/crates/redline-log), including filter parsing, field capture, NDJSON encoding, binary framing, and binary decoding.

## Example

```rust
use redline_core::TargetFilter;
use tracing_core::metadata::LevelFilter;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filter = TargetFilter::parse("info,my_app::db=debug")?;
    assert_eq!(filter.default_level(), LevelFilter::INFO);
    Ok(())
}
```
