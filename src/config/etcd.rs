use std::{error::Error, fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use etcd_client::{Client, ConnectOptions, Event, GetOptions, GetResponse, WatchOptions};
use pingora_core::{server::ShutdownWatch, services::background::BackgroundService};
use tokio::{sync::Mutex, time::sleep};

use super::Etcd;

#[derive(Debug)]
pub enum EtcdError {
    ClientNotInitialized,
    ConnectionFailed(String),
    ListOperationFailed(String),
    WatchOperationFailed(String),
    Other(String),
}

impl fmt::Display for EtcdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EtcdError::ClientNotInitialized => write!(f, "Etcd client is not initialized"),
            EtcdError::ConnectionFailed(msg) => write!(f, "Connection failed: {}", msg),
            EtcdError::ListOperationFailed(msg) => write!(f, "List operation failed: {}", msg),
            EtcdError::WatchOperationFailed(msg) => write!(f, "Watch operation failed: {}", msg),
            EtcdError::Other(msg) => write!(f, "Other error: {}", msg),
        }
    }
}

impl std::error::Error for EtcdError {}

pub struct EtcdConfigSync {
    config: Etcd,
    client: Arc<Mutex<Option<Client>>>,
    revision: Arc<Mutex<i64>>,
    handler: Box<dyn EtcdEventHandler + Send + Sync>,
}

impl EtcdConfigSync {
    pub fn new(config: Etcd, handler: Box<dyn EtcdEventHandler + Send + Sync>) -> Self {
        assert!(
            !config.prefix.is_empty(),
            "EtcdConfigSync requires a non-empty prefix"
        );

        Self {
            config,
            client: Arc::new(Mutex::new(None)),
            revision: Arc::new(Mutex::new(0)),
            handler,
        }
    }

    /// 获取初始化的 etcd 客户端
    async fn get_client(&self) -> Result<Arc<Mutex<Option<Client>>>, EtcdError> {
        let mut client_guard = self.client.lock().await;

        if client_guard.is_none() {
            log::info!("Creating new etcd client...");
            *client_guard = Some(create_client(&self.config).await?);
        }

        Ok(self.client.clone())
    }

    /// 初始化时同步 etcd 数据
    async fn list(&self) -> Result<(), EtcdError> {
        let client_arc = self.get_client().await?;
        let mut client_guard = client_arc.lock().await;

        let client = client_guard
            .as_mut()
            .ok_or(EtcdError::ClientNotInitialized)?;

        let options = GetOptions::new().with_prefix();
        let response = client
            .get(self.config.prefix.as_str(), Some(options))
            .await
            .map_err(|e| EtcdError::ListOperationFailed(e.to_string()))?;

        if let Some(header) = response.header() {
            *self.revision.lock().await = header.revision();
        } else {
            return Err(EtcdError::Other(
                "Failed to get header from response".to_string(),
            ));
        }

        self.handler.handle_list_response(&response);
        Ok(())
    }

    /// 监听 etcd 数据变更
    async fn watch(&self) -> Result<(), EtcdError> {
        let start_revision = *self.revision.lock().await + 1;
        let options = WatchOptions::new()
            .with_start_revision(start_revision)
            .with_prefix();

        let client_arc = self.get_client().await?;
        let mut client_guard = client_arc.lock().await;

        let client = client_guard
            .as_mut()
            .ok_or(EtcdError::ClientNotInitialized)?;

        let (mut watcher, mut stream) = client
            .watch(self.config.prefix.as_str(), Some(options))
            .await
            .map_err(|e| EtcdError::WatchOperationFailed(e.to_string()))?;

        watcher.request_progress().await.map_err(|e| {
            EtcdError::WatchOperationFailed(format!("Failed to request progress: {}", e))
        })?;

        while let Some(response) = stream.message().await.map_err(|e| {
            EtcdError::WatchOperationFailed(format!("Failed to receive watch message: {}", e))
        })? {
            if response.canceled() {
                log::warn!("Watch stream was canceled");
                break;
            }

            for event in response.events() {
                self.handler.handle_event(event);
            }
        }
        Ok(())
    }

    /// 重置客户端
    async fn reset_client(&self) {
        log::warn!("Resetting etcd client...");
        *self.client.lock().await = None;
    }

    /// 主任务循环
    async fn run_sync_loop(&self, shutdown: &mut ShutdownWatch) {
        loop {
            tokio::select! {
                biased; // 优先处理关闭信号
                // Shutdown signal handling
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        log::info!("Shutdown signal received, stopping etcd config sync");
                        return;
                    }
                },

                // Perform list operation
                result = self.list() => {
                    if let Err(err) = result {
                        log::error!("List operation failed: {:?}", err);
                        self.reset_client().await;
                        sleep(Duration::from_secs(3)).await;
                        continue;
                    }
                }
            }

            tokio::select! {
                biased; // 优先处理关闭信号
                // Shutdown signal handling during watch
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        log::info!("Shutdown signal received, stopping etcd config sync");
                        return;
                    }
                },

                // Perform watch operation
                result = self.watch() => {
                    if let Err(err) = result {
                        log::error!("Watch operation failed: {:?}", err);
                        self.reset_client().await;
                        sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }
    }
}

#[async_trait]
impl BackgroundService for EtcdConfigSync {
    async fn start(&self, mut shutdown: ShutdownWatch) {
        self.run_sync_loop(&mut shutdown).await;
    }
}

pub trait EtcdEventHandler {
    fn handle_event(&self, event: &Event);
    fn handle_list_response(&self, response: &GetResponse);
}

async fn create_client(cfg: &Etcd) -> Result<Client, EtcdError> {
    let mut options = ConnectOptions::default();
    if let Some(timeout) = cfg.timeout {
        options = options.with_timeout(Duration::from_secs(timeout as _));
    }
    if let Some(connect_timeout) = cfg.connect_timeout {
        options = options.with_connect_timeout(Duration::from_secs(connect_timeout as _));
    }
    if let (Some(user), Some(password)) = (&cfg.user, &cfg.password) {
        options = options.with_user(user.clone(), password.clone());
    }

    Client::connect(cfg.host.clone(), Some(options))
        .await
        .map_err(|e| EtcdError::ConnectionFailed(e.to_string()))
}

pub fn json_to_resource<T>(value: &[u8]) -> Result<T, Box<dyn Error>>
where
    T: serde::de::DeserializeOwned,
{
    // Deserialize the input value from JSON
    let json_value: serde_json::Value = serde_json::from_slice(value)?;

    // Serialize the JSON value to YAML directly into a Vec<u8>
    let mut yaml_output = Vec::new();
    let mut serializer = serde_yaml::Serializer::new(&mut yaml_output);
    serde_transcode::transcode(json_value, &mut serializer)?;

    // Deserialize directly from the YAML bytes
    let resource: T = serde_yaml::from_slice(&yaml_output)?;

    Ok(resource)
}

pub struct EtcdClientWrapper {
    config: Etcd,
    client: Arc<Mutex<Option<Client>>>,
}

impl EtcdClientWrapper {
    pub fn new(cfg: Etcd) -> Self {
        Self {
            config: cfg,
            client: Arc::new(Mutex::new(None)),
        }
    }

    async fn ensure_connected(&self) -> Result<Arc<Mutex<Option<Client>>>, EtcdError> {
        let mut client_guard = self.client.lock().await;

        if client_guard.is_none() {
            log::info!("Creating new etcd client...");
            *client_guard = Some(
                create_client(&self.config)
                    .await
                    .map_err(|e| EtcdError::ConnectionFailed(e.to_string()))?,
            );
        }

        Ok(self.client.clone())
    }

    pub async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, EtcdError> {
        let client_arc = self.ensure_connected().await?;
        let mut client_guard = client_arc.lock().await;

        let client = client_guard
            .as_mut()
            .ok_or(EtcdError::ClientNotInitialized)?;

        client
            .get(self.with_prefix(key), None)
            .await
            .map_err(|e| EtcdError::ListOperationFailed(e.to_string()))
            .map(|resp| resp.kvs().first().map(|kv| kv.value().to_vec()))
    }

    pub async fn put(&self, key: &str, value: Vec<u8>) -> Result<(), EtcdError> {
        let client_arc = self.ensure_connected().await?;
        let mut client_guard = client_arc.lock().await;

        let client = client_guard
            .as_mut()
            .ok_or(EtcdError::ClientNotInitialized)?;

        client
            .put(self.with_prefix(key), value, None)
            .await
            .map_err(|e| EtcdError::Other(format!("Put operation failed: {}", e)))?;
        Ok(())
    }

    pub async fn delete(&self, key: &str) -> Result<(), EtcdError> {
        let client_arc = self.ensure_connected().await?;
        let mut client_guard = client_arc.lock().await;

        let client = client_guard
            .as_mut()
            .ok_or(EtcdError::ClientNotInitialized)?;

        client
            .delete(self.with_prefix(key), None)
            .await
            .map_err(|e| EtcdError::Other(format!("Delete operation failed: {}", e)))?;
        Ok(())
    }

    fn with_prefix(&self, key: &str) -> String {
        format!("{}/{}", self.config.prefix, key)
    }
}
