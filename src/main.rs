#![allow(clippy::upper_case_acronyms)]

use config::{Config, Tls};
use pingora::services::listening::Service;
use pingora_core::apps::HttpServerOptions;
use pingora_core::listeners::tls::TlsSettings;
use pingora_core::server::configuration::Opt;
use pingora_core::server::Server;
use pingora_proxy::http_proxy_service_with_name;
use proxy::{service::load_services, upstream::load_upstreams};
use sentry::IntoDsn;
use service::http::build_http_service;
use spdlog::sink::{AsyncPoolSink, RotatingFileSink, RotationPolicy};
use spdlog::{error, info};
use std::env;
use std::error::Error;
use std::sync::Arc;
mod config;
mod proxy;
mod service;

fn run() -> Result<(), Box<dyn Error>> {
    // Initialize logging
    // env_logger::init();

    let mut path_buf = env::current_exe().unwrap();
    path_buf.pop();
    path_buf.push("log");
    path_buf.push("async");
    path_buf.set_extension("log");
    println!("--- {:?}", path_buf);

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

    info!("this log will be written to the file `rotating_daily.log`, and the file will be rotated daily at 00:00");

    // Read command-line arguments and load configuration
    let opt = Opt::parse_args();
    let config = Config::load_yaml_with_opt_override(&opt).expect("Failed to load configuration");

    // Log loading stages and initialize necessary services
    log::info!("Loading services, upstreams, and routers...");
    load_services(&config).expect("Failed to load services");
    load_upstreams(&config).expect("Failed to load upstreams");
    let http_service = build_http_service(&config).expect("Failed to initialize proxy service");

    // Create Pingora server with optional config and add HTTP service
    let mut pingsix_server = Server::new_with_opt_and_conf(Some(opt), config.pingora);
    let mut http_service =
        http_proxy_service_with_name(&pingsix_server.configuration, http_service, "pingsix");

    // Add listeners (TLS or TCP) based on configuration
    log::info!("Adding listeners...");
    for list_cfg in config.listeners.iter() {
        match &list_cfg.tls {
            Some(Tls {
                cert_path,
                key_path,
            }) => {
                let mut settings = TlsSettings::intermediate(cert_path, key_path)
                    .expect("Adding TLS listener shouldn't fail");
                if list_cfg.offer_h2 {
                    settings.enable_h2();
                }
                http_service.add_tls_with_settings(&list_cfg.address.to_string(), None, settings);
            }
            None => {
                if list_cfg.offer_h2c {
                    let http_logic = http_service.app_logic_mut().unwrap();
                    let mut http_server_options = HttpServerOptions::default();
                    http_server_options.h2c = true;
                    http_logic.server_options = Some(http_server_options);
                }
                http_service.add_tcp(&list_cfg.address.to_string());
            }
        }
    }

    // Add Sentry configuration if provided
    if let Some(sentry_cfg) = &config.sentry {
        log::info!("Adding Sentry config...");
        pingsix_server.sentry = Some(sentry::ClientOptions {
            dsn: sentry_cfg.dsn.clone().into_dsn().unwrap(),
            ..Default::default()
        });
    }

    // Add Prometheus service if provided
    if let Some(prometheus_cfg) = &config.prometheus {
        log::info!("Adding Prometheus Service...");
        let mut prometheus_service_http = Service::prometheus_http_service();
        prometheus_service_http.add_tcp(&prometheus_cfg.address.to_string());
        pingsix_server.add_service(prometheus_service_http);
    }

    // Bootstrapping and server startup
    log::info!("Bootstrapping...");
    pingsix_server.bootstrap();
    log::info!("Bootstrapped. Adding Services...");
    pingsix_server.add_service(http_service);

    log::info!("Starting Server...");
    pingsix_server.run_forever();
}

fn main() {
    if let Err(err) = run() {
        error!("{}", err.to_string());
    }
}
