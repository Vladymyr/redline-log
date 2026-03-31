# redline-decode

Offline decoder CLI for the compact binary format emitted by [`redline-log`](https://crates.io/crates/redline-log).

## Usage

```bash
redline-decode input.bin output.ndjson
redline-decode input.bin -
redline-decode - output.ndjson
```

Use `-` for stdin or stdout.
