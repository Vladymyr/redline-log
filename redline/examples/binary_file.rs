use std::{env, path::PathBuf};

use redline::{Builder, OutputFormat, Sink};
use tracing::{info, info_span};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| env::temp_dir().join("redline-example.bin"));

    let handle = Builder::new()
        .format(OutputFormat::Binary)
        .sink(Sink::file(&path))
        .install_global()?;

    let request = info_span!("request", request_id = 42u64, user_id = 7u64);
    let _guard = request.enter();

    info!(
        target: "app::worker",
        job = "index_documents",
        documents = 128u64,
        ok = true,
        message = "job_complete"
    );

    handle.flush()?;
    println!("wrote {}", path.display());
    println!(
        "decode with: cargo run -p redline-decode -- {} -",
        path.display()
    );
    Ok(())
}
