// memory cache via tinyufo
// only cache small static file request with GET method
// TODO: finish logic and add test code

use async_trait::async_trait;
use http::Method;
use http::{header, StatusCode};
use once_cell::sync::Lazy;
use pingora_error::{ErrorType::ReadError, OrErr, Result};
use pingora_http::ResponseHeader;
use pingora_proxy::Session;
use std::sync::Arc;

use crate::proxy::ProxyContext;
use serde::{Deserialize, Serialize};
use serde_yaml::Value as YamlValue;
use spdlog::info;
use tinyufo::TinyUfo;

use super::ProxyPlugin;

#[derive(Clone)]
struct TinyCacheData {
    uri: String,
    content_type: String,
    content_len: usize,
    payload: Vec<u8>,
}

static CACHE: Lazy<TinyUfo<String, TinyCacheData>> = Lazy::new(|| tinyufo::TinyUfo::new(1000, 10));

pub const PLUGIN_NAME: &str = "tiny_cache";

pub fn create_tiny_cache_plugin(cfg: YamlValue) -> Result<Arc<dyn ProxyPlugin>> {
    let config: PluginConfig = serde_yaml::from_value(cfg)
        .or_err_with(ReadError, || "Invalid tiny cache plugin config")?;

    info!("registered plugin {}", PLUGIN_NAME);

    Ok(Arc::new(PluginTinyCache { config }))
}

#[derive(Default, Debug, Serialize, Deserialize)]
struct PluginConfig {
    #[serde(default = "PluginConfig::default_total_weight_limit")]
    total_weight_limit: usize,
    #[serde(default = "PluginConfig::default_estimated_size")]
    estimated_size: usize,
    #[serde(default = "PluginConfig::default_payload_limit")]
    payload_limit: usize, // less than 1 Mb , save to cache
}

impl PluginConfig {
    fn default_total_weight_limit() -> usize {
        1000
    }
    fn default_estimated_size() -> usize {
        10
    }
    fn default_payload_limit() -> usize {
        1024 * 1024 * 1
    }
}

#[allow(dead_code)]
pub struct PluginTinyCache {
    config: PluginConfig,
}

#[async_trait]
impl ProxyPlugin for PluginTinyCache {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn priority(&self) -> i32 {
        412
    }

    async fn request_filter(&self, session: &mut Session, ctx: &mut ProxyContext) -> Result<bool> {
        let method = &session.req_header().method;
        if Method::GET != method {
            info!("method: {:?}", method);
        }
        let router = ctx.router.clone();
        // get uri and GET method as key in cache
        let uri = router
            .as_ref()
            .map_or("", |_| session.req_header().uri.path());

        let data = CACHE.get(&uri.to_string());

        if let Some(data) = data {
            info!("hit cache {}", data.uri);

            let mut resp = ResponseHeader::build(StatusCode::OK, Some(4))?;
            resp.insert_header(header::CONTENT_LENGTH, data.content_len)?;
            resp.insert_header(header::CONTENT_TYPE, data.content_type)?;
            // Write response header to the session
            session.write_response_header(Box::new(resp), false).await?;

            // Write response body to the session
            session
                .write_response_body(Some(data.payload.clone().into()), true)
                .await?;
            // tell response_filter not to save response to cache
            ctx.vars.insert("pass".to_string(), "1".to_string());

            return Ok(true);
        }

        // TODO: for test only , remove later
        ctx.vars.insert("pass".to_string(), "1".to_string());

        Ok(false)
    }
    async fn response_filter(
        &self,
        session: &mut Session,
        _upstream_response: &mut ResponseHeader,
        ctx: &mut ProxyContext,
    ) -> Result<()> {
        info!("response filter--------------------------");
        let pass = ctx.vars.get("pass");

        if pass.is_some() {
            info!("============== pass through");
            // return Ok(());
        }

        let router = ctx.router.clone();

        // Extract URI and host, falling back to empty string or default values
        let uri = router
            .as_ref()
            .map_or("", |_| session.req_header().uri.path());

        // Extract response code
        let code = session
            .response_written()
            .map_or("", |resp| resp.status.as_str());

        let method = &session.req_header().method;
        // TODO: for test only , remove later
        info!(
            "response filter--------------------------{} {} {} ",
            code,
            uri,
            method.to_string()
        );

        // TODO: save response to cache if size limit

        Ok(())
    }
}
