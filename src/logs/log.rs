use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(not(debug_assertions))]
use spdlog::prelude::*;
use spdlog::sink::{AsyncPoolSink, RotatingFileSink, RotationPolicy};

#[cfg(debug_assertions)]
pub fn init_logger(path_buf: PathBuf) -> Result<(), Box<dyn Error>> {
    let file_sink = Arc::new(
        RotatingFileSink::builder()
            .base_path(path_buf)
            .rotation_policy(RotationPolicy::Daily { hour: 0, minute: 0 })
            .build()?,
    );
    // AsyncPoolSink is a combined sink which wraps other sinks

    let new_logger = spdlog::default_logger().fork_with(|new| {
        let _async_pool_sink = Arc::new(AsyncPoolSink::builder().sink(file_sink).build()?);
        new.sinks_mut().push(_async_pool_sink);
        Ok(())
    })?;

    spdlog::set_default_logger(new_logger);
    Ok(())
}

#[cfg(not(debug_assertions))]
pub fn init_logger(path_buf: PathBuf) -> Result<(), Box<dyn Error>> {
    let file_sink = Arc::new(
        RotatingFileSink::builder()
            .base_path(path_buf)
            .rotation_policy(RotationPolicy::Daily { hour: 0, minute: 0 })
            .build()?,
    );
    // AsyncPoolSink is a combined sink which wraps other sinks
    let async_pool_sink = Arc::new(AsyncPoolSink::builder().sink(file_sink).build()?);

    let async_logger = Arc::new(
        Logger::builder()
            .sink(async_pool_sink)
            .flush_level_filter(LevelFilter::All)
            .build()?,
    );
    spdlog::set_default_logger(async_logger);
    Ok(())
}
