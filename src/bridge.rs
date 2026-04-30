use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info};
use serde_json::Value;
use tokio::{
    net::TcpListener,
    sync::{mpsc, Mutex},
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

use crate::state_file::StateFile;

const MAX_CLIENT_QUEUE: usize = 64;

#[derive(Clone, Default)]
pub struct Bridge {
    state: Arc<Mutex<BridgeState>>,
    peers: Arc<Mutex<Vec<mpsc::Sender<String>>>>,
    state_file: Option<Arc<StateFile>>,
}

#[derive(Default)]
struct BridgeState {
    messages: HashMap<String, Value>,
}

impl BridgeState {
    fn update(&mut self, msg: &Value) {
        let Some(socket_id) = msg.get("socketId").and_then(Value::as_str) else {
            return;
        };

        if msg.get("activity").is_some_and(Value::is_null) {
            self.messages.remove(socket_id);
        } else {
            self.messages.insert(socket_id.to_owned(), msg.clone());
        }
    }

    fn replayable(&self) -> Vec<Value> {
        self.messages.values().cloned().collect()
    }
}

impl Bridge {
    pub fn with_state_file(state_file: StateFile) -> Self {
        Self {
            state_file: Some(Arc::new(state_file)),
            ..Self::default()
        }
    }

    pub async fn send(&self, msg: Value) {
        let replayable = {
            let mut state = self.state.lock().await;
            state.update(&msg);
            state.replayable()
        };

        if let Some(state_file) = &self.state_file {
            if let Err(err) = state_file.write(&replayable).await {
                eprintln!("[arRPC > state] {err:#}");
            }
        }

        let payload = match serde_json::to_string(&msg) {
            Ok(payload) => payload,
            Err(err) => {
                error!("[arRPC > bridge] failed to serialize payload: {err}");
                return;
            }
        };

        debug!("[arRPC > bridge] sending {payload}");

        let mut peers = self.peers.lock().await;
        peers.retain(|peer| peer.try_send(payload.clone()).is_ok());
    }

    pub async fn cleanup_state_file(&self) {
        if let Some(state_file) = &self.state_file {
            state_file.cleanup().await;
        }
    }

    pub async fn run(&self, host: String, ports: Vec<u16>) -> anyhow::Result<()> {
        let (listener, port) = bind_bridge_listener(&host, &ports).await?;
        if let Some(state_file) = &self.state_file {
            state_file.set_server("bridge", host.clone(), port).await;
        }

        println!("[arRPC > bridge] listening on {host}:{port}");

        loop {
            let (stream, _) = listener.accept().await?;
            let bridge = self.clone();

            tokio::spawn(async move {
                info!("[arRPC > bridge] client connected");
                if let Err(err) = bridge.handle_client(stream).await {
                    error!("[arRPC > bridge] client error: {err:#}");
                }
                info!("[arRPC > bridge] client disconnected");
            });
        }
    }

    async fn handle_client(&self, stream: tokio::net::TcpStream) -> anyhow::Result<()> {
        let ws = accept_async(stream).await?;
        let (mut write, mut read) = ws.split();
        let (tx, mut rx) = mpsc::channel::<String>(MAX_CLIENT_QUEUE);

        for msg in self.state.lock().await.replayable() {
            if tx.try_send(serde_json::to_string(&msg)?).is_err() {
                return Ok(());
            }
        }

        self.peers.lock().await.push(tx);

        loop {
            tokio::select! {
                maybe_payload = rx.recv() => {
                    let Some(payload) = maybe_payload else { break; };
                    write.send(Message::Text(payload)).await?;
                }
                incoming = read.next() => {
                    let Some(incoming) = incoming else { break; };
                    match incoming {
                        Ok(msg) => debug!("[arRPC > bridge] received {msg:?}"),
                        Err(err) => error!("[arRPC > bridge] receive error: {err:#}"),
                    }
                }
            }
        }

        Ok(())
    }
}

async fn bind_bridge_listener(host: &str, ports: &[u16]) -> anyhow::Result<(TcpListener, u16)> {
    for port in ports {
        match TcpListener::bind((host, *port)).await {
            Ok(listener) => return Ok((listener, *port)),
            Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
                println!("[arRPC > bridge] {host}:{port} in use");
            }
            Err(err) => {
                return Err(err).with_context(|| format!("failed to bind bridge on {host}:{port}"))
            }
        }
    }

    anyhow::bail!("no bridge ports were available on {host}")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::BridgeState;

    #[test]
    fn removes_cached_activity_when_socket_clears_it() {
        let mut state = BridgeState::default();
        let active = json!({
            "socketId": "1",
            "activity": { "name": "Example" }
        });

        state.update(&active);
        assert_eq!(state.replayable(), vec![active]);

        state.update(&json!({
            "socketId": "1",
            "activity": null
        }));

        assert!(state.replayable().is_empty());
    }

    #[test]
    fn null_activity_for_unknown_socket_does_not_grow_state() {
        let mut state = BridgeState::default();

        for id in 0..1000 {
            state.update(&json!({
                "socketId": id.to_string(),
                "activity": null
            }));
        }

        assert!(state.replayable().is_empty());
    }
}
