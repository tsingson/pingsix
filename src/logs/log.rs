use spdlog::sink::{AsyncPoolSink, RotatingFileSink, RotationPolicy};
use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;

use spdlog::prelude::*;

#[allow(dead_code)]
pub fn default_logger() {
    let default_logger = spdlog::default_logger();
    default_logger.set_level_filter(LevelFilter::All);
}

pub fn init_logger(path_buf: PathBuf) -> Result<(), Box<dyn Error>> {
    let mut p_log: PathBuf = path_buf.clone();

    p_log.set_extension("proxy");
    p_log.set_extension("log");
    let mut p_err: PathBuf = path_buf.clone();
    p_err.push("error");
    p_err.set_extension("log");
    let log_file_sink = Arc::new(
        RotatingFileSink::builder()
            .base_path(p_log)
            .rotation_policy(RotationPolicy::Daily { hour: 0, minute: 0 })
            .level_filter(LevelFilter::All)
            .build()?,
    );
    let err_log_file_sink = Arc::new(
        RotatingFileSink::builder()
            .base_path(p_err)
            .rotation_policy(RotationPolicy::Daily { hour: 0, minute: 0 })
            .level_filter(LevelFilter::Equal(Level::Error))
            .build()?,
    );
    // AsyncPoolSink is a combined sink which wraps other sinks

    let new_logger = spdlog::default_logger().fork_with(|new| {
        let _async_log_sink = Arc::new(AsyncPoolSink::builder().sink(log_file_sink).build()?);
        let _async_err_sink = Arc::new(AsyncPoolSink::builder().sink(err_log_file_sink).build()?);
        new.sinks_mut().push(_async_log_sink);
        new.sinks_mut().push(_async_err_sink);
        Ok(())
    })?;

    spdlog::set_default_logger(new_logger);
    Ok(())
}
