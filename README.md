# redline-log

![license](https://img.shields.io/badge/license-MIT-blue?style=flat)
![output](https://img.shields.io/badge/output-NDJSON%20%7C%20binary-lightgrey?style=flat)

Fast structured logging for `tracing`, with NDJSON or binary output to stdout, stderr, or files.

Built for the common production path: bounded memory, simple filtering, and low overhead.

## Features

- Level and target filtering via a `target=level` filter spec
- Structured primitive fields with optional span context
- NDJSON and compact binary output
- `stdout`, `stderr`, file, and null sinks
- Bounded queuing with visible loss/error counters

## Workspace

| Crate | Role |
|---|---|
| [`redline-core`](./redline-core) | `no_std + alloc` - filters, field capture, NDJSON/binary encoding and decoding |
| [`redline-log`](./redline) | package name for the `redline` crate: `std` subscriber, sinks, background drain thread, builder API, counters |
| [`redline-layer`](./redline-layer) | `tracing-subscriber` layer that reuses `redline` encoding and sinks |
| [`redline-decode`](./redline-decode) | CLI tool to expand binary logs back into NDJSON |

The hot path uses thread-local callsite caching, pre-registered enabled callsites, `RwLock` reads for span lookups, `FxHashMap` registries, pooled frame buffers, and batched sink draining.

## Quick Start

```rust
use redline::{Builder, Sink};
use tracing::info;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let handle = Builder::new()
        .filter_spec("info,my_app::db=debug")?
        .sink(Sink::file("app.ndjson"))
        .install_global()?;

    info!(
        target: "my_app::http",
        request_id = 42u64,
        method = "GET",
        ok = true,
        message = "request_started"
    );

    handle.flush()?;
    Ok(())
}
```

<details>
<summary>Defaults</summary>

| Key | Default |
|---|---|
| `filter` | `info` |
| `format` | NDJSON |
| `sink` | `stderr` |
| `include_current_span` | `true` |
| `include_span_list` | `false` |
| `queue_capacity` | `1024` |
| `frame_buffer_size` | `1024` |
| `frame_buffer_count` | `64` |

</details>

More examples in [`redline/examples`](./redline/examples): stdout, file, binary output, target filtering, scoped subscribers, and nested span context.

```bash
cargo run -p redline-log --example basic_stdout
```

Use [`redline-layer`](./redline-layer) when you want the same output path inside a broader `tracing-subscriber` stack.

## Contributing

Pull requests are welcome. For major changes, please open an issue first to discuss what you would like to change and the reason for it.

```bash
cargo test
cargo run --release -p redline-decode -- input.bin > output.ndjson
```

## License

Licensed under [MIT](./LICENSE).
