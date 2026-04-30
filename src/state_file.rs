use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;

const STATE_FILE_PREFIX: &str = "arrpc-state";
const STATE_FILE_MAX_INDEX: u8 = 9;
const STALE_AFTER_MS: u128 = 10_000;

#[derive(Debug, Clone)]
pub struct StateFile {
    path: PathBuf,
    app_version: String,
    servers: Arc<Mutex<StateServers>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StateFileContent {
    app_version: String,
    timestamp: u128,
    servers: StateServers,
    activities: Vec<StateActivity>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct StateServers {
    #[serde(skip_serializing_if = "Option::is_none")]
    bridge: Option<ServerInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    websocket: Option<ServerInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ipc: Option<IpcServerInfo>,
}

#[derive(Debug, Clone, Serialize)]
struct ServerInfo {
    host: String,
    port: u16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct IpcServerInfo {
    socket_path: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StateActivity {
    socket_id: String,
    name: String,
    application_id: String,
    pid: u64,
    start_time: Option<u64>,
}

impl StateFile {
    pub async fn create(app_version: impl Into<String>) -> anyhow::Result<Self> {
        Self::create_in(std::env::temp_dir(), app_version).await
    }

    async fn create_in(dir: PathBuf, app_version: impl Into<String>) -> anyhow::Result<Self> {
        let path = select_state_file_path(&dir).await?;
        Ok(Self {
            path,
            app_version: app_version.into(),
            servers: Arc::new(Mutex::new(StateServers::default())),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn set_server(&self, server_type: &str, host: String, port: u16) {
        let info = ServerInfo { host, port };
        let mut servers = self.servers.lock().await;
        match server_type {
            "bridge" => servers.bridge = Some(info),
            "websocket" => servers.websocket = Some(info),
            _ => {}
        }
    }

    pub async fn set_ipc_server(&self, socket_path: String) {
        self.servers.lock().await.ipc = Some(IpcServerInfo { socket_path });
    }

    pub async fn write(&self, messages: &[Value]) -> anyhow::Result<()> {
        let content = StateFileContent {
            app_version: self.app_version.clone(),
            timestamp: now_ms(),
            servers: self.servers.lock().await.clone(),
            activities: messages
                .iter()
                .filter_map(state_activity_from_message)
                .collect(),
        };

        tokio::fs::write(&self.path, serde_json::to_vec_pretty(&content)?)
            .await
            .with_context(|| format!("failed to write state file {}", self.path.display()))
    }

    pub async fn cleanup(&self) {
        let _ = tokio::fs::remove_file(&self.path).await;
    }
}

async fn select_state_file_path(dir: &Path) -> anyhow::Result<PathBuf> {
    for index in 0..=STATE_FILE_MAX_INDEX {
        let path = dir.join(format!("{STATE_FILE_PREFIX}-{index}"));
        if is_available_state_path(&path).await {
            return Ok(path);
        }
    }

    Ok(dir.join(format!("{STATE_FILE_PREFIX}-{}", std::process::id())))
}

async fn is_available_state_path(path: &Path) -> bool {
    let Ok(raw) = tokio::fs::read_to_string(path).await else {
        return true;
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return true;
    };
    let Some(timestamp) = value.get("timestamp").and_then(Value::as_u64) else {
        return true;
    };

    now_ms().saturating_sub(timestamp as u128) > STALE_AFTER_MS
}

fn state_activity_from_message(msg: &Value) -> Option<StateActivity> {
    let activity = msg.get("activity")?;
    if activity.is_null() {
        return None;
    }

    Some(StateActivity {
        socket_id: msg.get("socketId")?.as_str()?.to_owned(),
        name: activity
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Unknown")
            .to_owned(),
        application_id: activity
            .get("application_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        pid: msg.get("pid").and_then(Value::as_u64).unwrap_or_default(),
        start_time: activity
            .get("timestamps")
            .and_then(|timestamps| timestamps.get("start"))
            .and_then(Value::as_u64),
    })
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{state_activity_from_message, StateFile};

    #[test]
    fn state_activity_uses_arrpc_bun_shape() {
        let activity = state_activity_from_message(&json!({
            "socketId": "42",
            "pid": 10,
            "activity": {
                "name": "Example",
                "application_id": "123",
                "timestamps": { "start": 1700000000000u64 }
            }
        }))
        .unwrap();

        assert_eq!(activity.socket_id, "42");
        assert_eq!(activity.name, "Example");
        assert_eq!(activity.application_id, "123");
        assert_eq!(activity.pid, 10);
        assert_eq!(activity.start_time, Some(1_700_000_000_000));
    }

    #[tokio::test]
    async fn state_file_records_server_metadata() {
        let state = StateFile::create_in(std::env::temp_dir(), "test")
            .await
            .unwrap();
        state
            .set_server("bridge", "127.0.0.1".to_owned(), 1337)
            .await;
        state
            .set_server("websocket", "127.0.0.1".to_owned(), 6463)
            .await;
        state.set_ipc_server("discord-ipc-0".to_owned()).await;

        state.write(&[]).await.unwrap();
        let raw = tokio::fs::read_to_string(state.path()).await.unwrap();
        state.cleanup().await;
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();

        assert_eq!(json["servers"]["bridge"]["port"], 1337);
        assert_eq!(json["servers"]["websocket"]["port"], 6463);
        assert_eq!(json["servers"]["ipc"]["socketPath"], "discord-ipc-0");
    }
}
