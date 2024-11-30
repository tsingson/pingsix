//! Handler for the `request_filter` phase.

use crate::logs::{debug, info, warn};
use crate::proxy::plugin::YamlValue;
use async_trait::async_trait;
use http::{method::Method, status::StatusCode};
use pingora_error::OrErr;

use crate::proxy::plugin::files::file_writer::{error_response, file_response};
use crate::proxy::plugin::files::metadata::Metadata;
use crate::proxy::plugin::files::path::resolve_uri;
use crate::proxy::plugin::files::range::{extract_range, Range};
use crate::proxy::plugin::ProxyPlugin;
use crate::proxy::ProxyContext;
use pingora_error::ErrorType::ReadError;
use pingora_http::ResponseHeader;
use pingora_proxy::Session;
use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;

pub const PLUGIN_NAME: &str = "static_files";

pub fn create_static_files_plugin(cfg: YamlValue) -> pingora_error::Result<Arc<dyn ProxyPlugin>> {
    let config: PluginConfig = serde_yaml::from_value(cfg)
        .or_err_with(ReadError, || "Invalid static files plugin config")?;

    info!("registered plugin {}", PLUGIN_NAME);

    Ok(Arc::new(PluginStaticFiles { config }))
}

#[derive(Default, Debug, Serialize, Deserialize)]
struct PluginConfig {
    #[serde(default = "PluginConfig::default_root_path")]
    root_path: Option<PathBuf>,
    index_file: Vec<String>,
}

impl PluginConfig {
    fn default_root_path() -> Option<PathBuf> {
        Some(PathBuf::from("/home/www/"))
    }
}

#[allow(dead_code)]
pub struct PluginStaticFiles {
    config: PluginConfig,
}

#[async_trait]
impl ProxyPlugin for PluginStaticFiles {
    fn name(&self) -> &str {
        crate::proxy::plugin::tinycache::PLUGIN_NAME
    }

    fn priority(&self) -> i32 {
        412
    }

    async fn request_filter(
        &self,
        session: &mut Session,
        _ctx: &mut ProxyContext,
    ) -> pingora_error::Result<bool> {
        let method = &session.req_header().method;
        if Method::GET != method {
            info!("method: {:?}", method);
            return Ok(false);
        }

        let root = if let Some(root) = &self.config.root_path {
            root
        } else {
            debug!("received request but static files handler is not configured, ignoring");
            return Ok(false);
        };

        let uri = &session.req_header().uri;

        debug!("received URI path {}", uri.path());

        let (mut path, not_found) = match resolve_uri(uri.path(), root) {
            Ok(path) => (path, false),
            Err(err) if err.kind() == ErrorKind::NotFound => {
                //
                return Ok(false);
            }
            Err(err) => {
                let _status = match err.kind() {
                    ErrorKind::InvalidInput => {
                        warn!("rejecting invalid path {}", uri.path());
                        StatusCode::BAD_REQUEST
                    }
                    ErrorKind::InvalidData => {
                        warn!("Requested path outside root directory: {}", uri.path());
                        StatusCode::BAD_REQUEST
                    }
                    ErrorKind::PermissionDenied => {
                        debug!("canonicalizing resulted in PermissionDenied error");
                        StatusCode::FORBIDDEN
                    }
                    _ => {
                        warn!("failed canonicalizing the path {}: {err}", uri.path());
                        StatusCode::INTERNAL_SERVER_ERROR
                    }
                };
                // TODO: handle error
                return Ok(false);
            }
        };

        debug!("translated into file path {path:?}");

        if path.is_dir() {
            for filename in &self.config.index_file {
                let candidate = path.join(filename);
                if candidate.is_file() {
                    debug!("using directory index file {filename}");
                    path = candidate;
                }
            }
        }

        info!("successfully resolved request path: {path:?}");

        match session.req_header().method {
            Method::GET | Method::HEAD => {
                // Allowed
            }
            _ => {
                warn!("Denying method {}", session.req_header().method);
                error_response(session, StatusCode::METHOD_NOT_ALLOWED).await?;
                // TODO: handle error
                return Ok(false);
            }
        }

        let orig_path = None;

        let meta = match Metadata::from_path(&path, orig_path.as_ref()) {
            Ok(meta) => meta,
            Err(err) if err.kind() == ErrorKind::InvalidInput => {
                warn!("Path {path:?} is not a regular file, denying access");
                error_response(session, StatusCode::FORBIDDEN).await?;
                //TODO:handle response send
                return Ok(false);
            }
            Err(err) => {
                warn!("failed retrieving metadata for path {path:?}: {err}");
                error_response(session, StatusCode::INTERNAL_SERVER_ERROR).await?;
                //TODO:handle response send
                return Ok(false);
            }
        };

        //
        // if meta.is_not_modified(session) {
        //     debug!("If-None-Match/If-Modified-Since check resulted in Not Modified");
        //     let header = meta.to_custom_header(StatusCode::NOT_MODIFIED)?;
        //     let header = compression.transform_header(session, header)?;
        //     session.write_response_header(header, true).await?;
        //     return Ok(RequestFilterResult::ResponseSent);
        // }

        let charset = None;

        let (mut header, start, end) = match extract_range(session, &meta) {
            Some(Range::Valid(start, end)) => {
                debug!("bytes range requested: {start}-{end}");
                let header = meta.to_partial_content_header(charset, start, end)?;
                (header, start, end)
            }
            Some(Range::OutOfBounds) => {
                debug!("requested bytes range is out of bounds");
                let header = meta.to_not_satisfiable_header(charset)?;

                session.write_response_header(header, true).await?;
                // return Ok(RequestFilterResult::ResponseSent);
                return Ok(false);
            }
            None => {
                // Range is either missing or cannot be parsed, produce the entire file.
                let header = meta.to_response_header(charset)?;

                (header, 0, meta.size - 1)
            }
        };

        if not_found {
            header.set_status(StatusCode::NOT_FOUND)?;
        }

        let send_body = session.req_header().method != Method::HEAD;
        session.write_response_header(header, !send_body).await?;

        if send_body {
            // sendfile would be nice but not currently possible within pingora-proxy (see
            // https://github.com/cloudflare/pingora/issues/160)
            file_response(session, &path, start, end).await?;
        }
        Ok(true)
        // Ok(RequestFilterResult::ResponseSent)
    }
    async fn response_filter(
        &self,
        session: &mut Session,
        _upstream_response: &mut ResponseHeader,
        ctx: &mut ProxyContext,
    ) -> pingora_error::Result<()> {
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
