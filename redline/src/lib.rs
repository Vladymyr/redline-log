//! Fast structured logging for `tracing`.
//!
//! # Example
//!
//! ```no_run
//! use redline::{Builder, Sink};
//! use tracing::info;
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let handle = Builder::new().sink(Sink::Stdout).install_global()?;
//!
//!     info!(target: "app::startup", message = "service_ready");
//!     handle.flush()?;
//!     Ok(())
//! }
//! ```

mod config;
mod pipeline;
mod sink;
mod subscriber;

pub use config::{Builder, Handle, InstallError, Sink, Stats};
#[doc(hidden)]
pub use pipeline::RedlinePipeline;
pub use redline_core::{EncodeConfig, FilterParseError, OutputFormat, TargetFilter};
pub use subscriber::RedlineSubscriber;
