use std::sync::Arc;

use async_trait::async_trait;

use pingora_core::{Error, Result};
use pingora_proxy::Session;

use super::ProxyPlugin;
use crate::proxy::{get_request_host, ProxyContext};
use crate::slogs::info;
use serde_yaml::Value as YamlValue;
pub const PLUGIN_NAME: &str = "logger";

#[warn(dead_code)]
pub fn create_logger_plugin(_cfg: YamlValue) -> Result<Arc<dyn ProxyPlugin>> {
    info!("registered plugin {}", PLUGIN_NAME);
    Ok(Arc::new(PluginLogger {}))
}

pub struct PluginLogger;

#[async_trait]
impl ProxyPlugin for PluginLogger {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn priority(&self) -> i32 {
        500
    }

    async fn logging(&self, session: &mut Session, _e: Option<&Error>, ctx: &mut ProxyContext) {
        // Clone router only once
        let router = ctx.router.clone();

        // Extract response code
        let code = session
            .response_written()
            .map_or("", |resp| resp.status.as_str());

        // Extract route information, falling back to empty string if not present
        let route = router.as_ref().map_or_else(|| "", |r| r.inner.id.as_str());

        // Extract URI and host, falling back to empty string or default values
        let uri = router
            .as_ref()
            .map_or("", |_| session.req_header().uri.path());

        let host = router.as_ref().map_or("", |_| {
            get_request_host(session.req_header()).unwrap_or_default()
        });

        // Extract service, falling back to host if service_id is None
        let service = router
            .as_ref()
            .map_or_else(|| host, |r| r.inner.service_id.as_deref().unwrap_or(host));

        // Extract node from context variables
        let upstream = ctx.vars.get("upstream").map_or("", |s| s.as_str());
        let remote_addr = ctx.vars.get("remote_addr").map_or("", |s| s.as_str());
        let remote_port = ctx.vars.get("remote_port").map_or("", |s| s.as_str());

        let latency = ctx.request_start.elapsed().as_millis() as f64 / 1000.0;
        let ingress = session.body_bytes_read() as u64;
        let egress = session.body_bytes_sent() as u64;

        info!(
            "code:{} route:{} uri:{} host:{} service:{} remote:{}:{} -> upstream:{} latency:{} ingress:{} egress:{}",
            code, route, uri, host, service, remote_addr, remote_port, upstream, latency, ingress, egress
        );
    }
}
