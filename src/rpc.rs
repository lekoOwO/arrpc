use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex as StdMutex, OnceLock,
    },
    time::Duration,
};

use anyhow::anyhow;
use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info};
use serde_json::{json, Map, Number, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        handshake::server::{Request, Response},
        Message,
    },
};

use crate::{bridge::Bridge, state_file::StateFile};

const ALLOWED_ORIGINS: &[&str] = &[
    "https://discord.com",
    "https://ptb.discord.com",
    "https://canary.discord.com",
];

#[derive(Clone)]
pub struct RpcServer {
    bridge: Bridge,
    next_socket_id: Arc<AtomicU64>,
    host: String,
    ports: Vec<u16>,
    data_dir: Option<PathBuf>,
    state_file: Option<StateFile>,
}

#[derive(Clone)]
pub(crate) struct RpcSession {
    socket_id: String,
    client_id: String,
    client_name: Option<String>,
    last_pid: Option<Value>,
    actions: Arc<dyn RpcActionHandler>,
}

#[derive(Debug, Default)]
pub(crate) struct RpcEffects {
    pub(crate) replies: Vec<Value>,
    pub(crate) activity: Option<Value>,
}

pub(crate) trait RpcActionHandler: Send + Sync {
    fn invite_browser(&self, code: Value, nonce: Value) -> RpcEffects;
    fn guild_template_browser(&self, code: Value, nonce: Value) -> RpcEffects;
    fn deep_link(&self, nonce: Value) -> RpcEffects;
}

#[derive(Debug, Default)]
pub(crate) struct DefaultRpcActionHandler;

impl RpcActionHandler for DefaultRpcActionHandler {
    fn invite_browser(&self, code: Value, nonce: Value) -> RpcEffects {
        ack_code("INVITE_BROWSER", code, nonce)
    }

    fn guild_template_browser(&self, code: Value, nonce: Value) -> RpcEffects {
        ack_code("GUILD_TEMPLATE_BROWSER", code, nonce)
    }

    fn deep_link(&self, nonce: Value) -> RpcEffects {
        RpcEffects {
            replies: vec![json!({
                "cmd": "DEEP_LINK",
                "data": null,
                "evt": null,
                "nonce": nonce
            })],
            activity: None,
        }
    }
}

impl RpcSession {
    #[cfg(test)]
    pub(crate) fn new(socket_id: String, client_id: String) -> Self {
        Self::with_actions(socket_id, client_id, Arc::new(DefaultRpcActionHandler))
    }

    pub(crate) async fn new_resolved(
        socket_id: String,
        client_id: String,
        data_dir: Option<&Path>,
    ) -> Self {
        let client_name = resolve_app_name_by_id(&client_id, data_dir).await;
        Self::with_actions_and_name(
            socket_id,
            client_id,
            client_name,
            Arc::new(DefaultRpcActionHandler),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_actions(
        socket_id: String,
        client_id: String,
        actions: Arc<dyn RpcActionHandler>,
    ) -> Self {
        let client_name = app_name_by_id(&client_id);
        Self::with_actions_and_name(socket_id, client_id, client_name, actions)
    }

    fn with_actions_and_name(
        socket_id: String,
        client_id: String,
        client_name: Option<String>,
        actions: Arc<dyn RpcActionHandler>,
    ) -> Self {
        Self {
            socket_id,
            client_id,
            client_name,
            last_pid: None,
            actions,
        }
    }

    pub(crate) fn handle_payload(&mut self, payload: &Value) -> RpcEffects {
        handle_rpc_payload(self, payload)
    }

    pub(crate) fn close_activity(&self) -> Value {
        json!({
            "activity": null,
            "pid": self.last_pid,
            "socketId": self.socket_id
        })
    }
}

impl RpcServer {
    pub fn new(bridge: Bridge) -> Self {
        Self::with_bind(
            bridge,
            crate::config::DEFAULT_HOST.to_owned(),
            crate::config::WEBSOCKET_PORTS
                .chain(crate::config::WEBSOCKET_PORTS_HYPERV)
                .collect(),
        )
    }

    pub fn with_bind(bridge: Bridge, host: String, ports: Vec<u16>) -> Self {
        Self {
            bridge,
            next_socket_id: Arc::new(AtomicU64::new(0)),
            host,
            ports,
            data_dir: None,
            state_file: None,
        }
    }

    pub fn with_data_dir(mut self, data_dir: Option<PathBuf>) -> Self {
        self.data_dir = data_dir;
        self
    }

    pub fn with_state_file(mut self, state_file: StateFile) -> Self {
        self.state_file = Some(state_file);
        self
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let listener = bind_rpc_listener(&self.host, &self.ports).await?;
        let port = listener.local_addr()?.port();
        if let Some(state_file) = &self.state_file {
            state_file
                .set_server("websocket", self.host.clone(), port)
                .await;
        }
        info!("[arRPC > websocket] listening on {}:{port}", self.host);

        loop {
            let (stream, _) = listener.accept().await?;
            let server = self.clone();
            let next_id = self.next_socket_id.clone();
            let bridge = self.bridge.clone();

            tokio::spawn(async move {
                let socket_id = next_id.fetch_add(1, Ordering::Relaxed).to_string();
                info!("[arRPC > websocket] client connected ({socket_id})");
                if let Err(err) =
                    handle_ws_client(bridge, socket_id.clone(), server.data_dir.clone(), stream).await
                {
                    error!("[arRPC > websocket] client error ({socket_id}): {err:#}");
                }
                info!("[arRPC > websocket] client disconnected ({socket_id})");
            });
        }
    }
}

#[allow(clippy::result_large_err)]
async fn handle_ws_client(
    bridge: Bridge,
    socket_id: String,
    data_dir: Option<PathBuf>,
    stream: TcpStream,
) -> anyhow::Result<()> {
    let client_id = Arc::new(StdMutex::new(String::new()));
    let client_id_for_callback = client_id.clone();

    let ws =
        accept_hdr_async(
            stream,
            move |req: &Request, response: Response| match validate_rpc_request(req) {
                Ok(value) => {
                    *client_id_for_callback
                        .lock()
                        .expect("client id lock poisoned") = value;
                    Ok(response)
                }
                Err(message) => Err(Response::builder()
                    .status(400)
                    .body(Some(message))
                    .expect("valid websocket error response")),
            },
        )
        .await?;

    let client_id = { client_id.lock().expect("client id lock poisoned").clone() };
    let mut session =
        RpcSession::new_resolved(socket_id.clone(), client_id, data_dir.as_deref()).await;

    let (mut write, mut read) = ws.split();
    debug!("[arRPC > websocket] sending ready ({socket_id})");
    write
        .send(Message::Text(rpc_ready_payload().to_string()))
        .await?;

    while let Some(message) = read.next().await {
        let message = message?;
        if !message.is_text() {
            continue;
        }

        let payload: Value = serde_json::from_str(message.to_text()?)?;
        debug!("[arRPC > websocket] received ({socket_id}) {payload}");
        let effects = session.handle_payload(&payload);

        for reply in effects.replies {
            debug!("[arRPC > websocket] sending ({socket_id}) {reply}");
            write.send(Message::Text(reply.to_string())).await?;
        }

        if let Some(activity) = effects.activity {
            bridge.send(activity).await;
        }
    }

    bridge.send(session.close_activity()).await;

    Ok(())
}

async fn bind_rpc_listener(host: &str, ports: &[u16]) -> anyhow::Result<TcpListener> {
    for port in ports {
        match TcpListener::bind((host, *port)).await {
            Ok(listener) => return Ok(listener),
            Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
                println!("[arRPC > websocket] {host}:{port} in use");
            }
            Err(err) => return Err(err.into()),
        }
    }

    Err(anyhow!("no RPC websocket ports were available on {host}"))
}

fn validate_rpc_request(req: &Request) -> Result<String, String> {
    if let Some(origin) = req
        .headers()
        .get("origin")
        .and_then(|value| value.to_str().ok())
    {
        if !ALLOWED_ORIGINS.contains(&origin) {
            return Err(format!("disallowed origin: {origin}"));
        }
    }

    let params = parse_query(req.uri().query().unwrap_or_default());
    let version = params
        .get("v")
        .map(String::as_str)
        .unwrap_or("1")
        .parse::<u8>()
        .map_err(|_| "unsupported version requested".to_owned())?;
    if version != 1 {
        return Err(format!("unsupported version requested: {version}"));
    }

    let encoding = params.get("encoding").map(String::as_str).unwrap_or("json");
    if encoding != "json" {
        return Err(format!("unsupported encoding requested: {encoding}"));
    }

    Ok(params.get("client_id").cloned().unwrap_or_default())
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key.to_owned(), value.to_owned())
        })
        .collect()
}

pub(crate) fn rpc_ready_payload() -> Value {
    json!({
        "cmd": "DISPATCH",
        "data": {
            "v": 1,
            "config": {
                "cdn_host": "cdn.discordapp.com",
                "api_endpoint": "//discord.com/api",
                "environment": "production"
            },
            "user": {
                "id": "1045800378228281345",
                "username": "arrpc",
                "discriminator": "0",
                "global_name": "arRPC",
                "avatar": "cfefa4d9839fb4bdf030f91c2a13e95c",
                "avatar_decoration_data": null,
                "bot": false,
                "flags": 0,
                "premium_type": 0
            }
        },
        "evt": "READY",
        "nonce": null
    })
}

fn handle_rpc_payload(socket: &mut RpcSession, payload: &Value) -> RpcEffects {
    let cmd = payload
        .get("cmd")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args = payload.get("args").cloned().unwrap_or_else(|| json!({}));
    let nonce = payload.get("nonce").cloned().unwrap_or(Value::Null);

    match cmd {
        "CONNECTIONS_CALLBACK" => RpcEffects {
            replies: vec![json!({
                "cmd": cmd,
                "data": { "code": 1000 },
                "evt": "ERROR",
                "nonce": nonce
            })],
            activity: None,
        },
        "SET_ACTIVITY" => handle_set_activity(socket, cmd, &args, nonce),
        "INVITE_BROWSER" => socket
            .actions
            .invite_browser(args.get("code").cloned().unwrap_or(Value::Null), nonce),
        "GUILD_TEMPLATE_BROWSER" => socket
            .actions
            .guild_template_browser(args.get("code").cloned().unwrap_or(Value::Null), nonce),
        "DEEP_LINK" => socket.actions.deep_link(nonce),
        _ => RpcEffects::default(),
    }
}

fn ack_code(cmd: &str, code: Value, nonce: Value) -> RpcEffects {
    RpcEffects {
        replies: vec![json!({
            "cmd": cmd,
            "data": { "code": code },
            "evt": null,
            "nonce": nonce
        })],
        activity: None,
    }
}

fn handle_set_activity(
    socket: &mut RpcSession,
    cmd: &str,
    args: &Value,
    nonce: Value,
) -> RpcEffects {
    let activity = args.get("activity").cloned().unwrap_or(Value::Null);
    let pid = args.get("pid").cloned().unwrap_or(Value::Null);

    if activity.is_null() {
        return RpcEffects {
            replies: vec![json!({
                "cmd": cmd,
                "data": null,
                "evt": null,
                "nonce": nonce
            })],
            activity: Some(json!({
                "activity": null,
                "pid": pid,
                "socketId": socket.socket_id
            })),
        };
    }

    if !pid.is_null() {
        socket.last_pid = Some(pid.clone());
    }

    let mut activity_obj = activity.as_object().cloned().unwrap_or_default();
    normalize_timestamps(&mut activity_obj);

    let metadata = build_metadata(&activity_obj);
    let extra_buttons = build_button_labels(&activity_obj);
    let instance = activity_obj
        .get("instance")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut bridge_activity = Map::new();
    bridge_activity.insert(
        "application_id".to_owned(),
        Value::String(socket.client_id.clone()),
    );
    bridge_activity.insert("type".to_owned(), Value::Number(Number::from(0)));
    bridge_activity.insert(
        "name".to_owned(),
        Value::String(socket.client_name.clone().unwrap_or_default()),
    );
    bridge_activity.insert("metadata".to_owned(), metadata);
    bridge_activity.insert(
        "flags".to_owned(),
        Value::Number(Number::from(if instance { 1 } else { 0 })),
    );
    bridge_activity.extend(activity_obj.clone());
    if let Some(labels) = extra_buttons {
        bridge_activity.insert("buttons".to_owned(), labels);
    }

    let mut reply_data = activity_obj;
    reply_data.insert(
        "name".to_owned(),
        Value::String(socket.client_name.clone().unwrap_or_default()),
    );
    reply_data.insert(
        "application_id".to_owned(),
        Value::String(socket.client_id.clone()),
    );
    reply_data.insert("type".to_owned(), Value::Number(Number::from(0)));

    RpcEffects {
        replies: vec![json!({
            "cmd": cmd,
            "data": Value::Object(reply_data),
            "evt": null,
            "nonce": nonce
        })],
        activity: Some(json!({
            "activity": Value::Object(bridge_activity),
            "pid": pid,
            "socketId": socket.socket_id
        })),
    }
}

fn normalize_timestamps(activity: &mut Map<String, Value>) {
    let Some(timestamps) = activity
        .get_mut("timestamps")
        .and_then(Value::as_object_mut)
    else {
        return;
    };

    for value in timestamps.values_mut() {
        if let Some(timestamp) = value.as_i64() {
            if timestamp < 10_000_000_000 {
                *value = Value::Number(Number::from(timestamp * 1000));
            }
        }
    }
}

fn build_metadata(activity: &Map<String, Value>) -> Value {
    let Some(buttons) = activity.get("buttons").and_then(Value::as_array) else {
        return json!({});
    };

    let urls = buttons
        .iter()
        .filter_map(|button| button.get("url").cloned())
        .collect::<Vec<_>>();

    json!({ "button_urls": urls })
}

fn build_button_labels(activity: &Map<String, Value>) -> Option<Value> {
    let buttons = activity.get("buttons")?.as_array()?;
    Some(Value::Array(
        buttons
            .iter()
            .filter_map(|button| button.get("label").cloned())
            .collect(),
    ))
}

#[cfg(test)]
fn app_name_by_id(app_id: &str) -> Option<String> {
    app_name_by_id_from_data(app_id, None)
}

fn app_name_by_id_from_data(app_id: &str, data_dir: Option<&Path>) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct AppName {
        id: String,
        name: String,
    }

    let raw = data_dir
        .and_then(|data_dir| std::fs::read_to_string(data_dir.join("detectable.json")).ok())
        .unwrap_or_else(|| include_str!("../assets/detectable.json").to_owned());

    serde_json::from_str::<Vec<AppName>>(raw.trim_start_matches('\u{feff}'))
        .ok()?
        .into_iter()
        .find(|app| app.id == app_id)
        .map(|app| app.name)
}

async fn resolve_app_name_by_id(app_id: &str, data_dir: Option<&Path>) -> Option<String> {
    if let Some(name) = app_name_by_id_from_data(app_id, data_dir) {
        return Some(name);
    }

    static CACHE: OnceLock<StdMutex<HashMap<String, Option<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| StdMutex::new(HashMap::new()));
    if let Some(cached) = cache
        .lock()
        .expect("app name cache lock poisoned")
        .get(app_id)
        .cloned()
    {
        return cached;
    }

    let fetched = fetch_app_name_from_discord(app_id).await;
    cache
        .lock()
        .expect("app name cache lock poisoned")
        .insert(app_id.to_owned(), fetched.clone());
    fetched
}

async fn fetch_app_name_from_discord(app_id: &str) -> Option<String> {
    let url = format!("https://discord.com/api/v10/applications/{app_id}/rpc");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .ok()?;
    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }

    #[derive(serde::Deserialize)]
    struct AppNameResponse {
        name: Option<String>,
    }

    response.json::<AppNameResponse>().await.ok()?.name
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::{handle_rpc_payload, RpcSession};

    #[test]
    fn set_activity_translates_buttons_and_second_timestamps() {
        let mut socket = RpcSession::new("7".to_owned(), "123".to_owned());

        let effects = handle_rpc_payload(
            &mut socket,
            &json!({
                "cmd": "SET_ACTIVITY",
                "args": {
                    "pid": 42,
                    "activity": {
                        "name": "Game",
                        "instance": true,
                        "timestamps": { "start": 1710000000 },
                        "buttons": [
                            { "label": "Site", "url": "https://example.com" }
                        ]
                    }
                },
                "nonce": "abc"
            }),
        );

        let activity = effects.activity.unwrap();
        assert_eq!(activity["socketId"], "7");
        assert_eq!(activity["pid"], 42);
        assert_eq!(activity["activity"]["application_id"], "123");
        assert_eq!(activity["activity"]["flags"], 1);
        assert_eq!(
            activity["activity"]["timestamps"]["start"],
            1_710_000_000_000i64
        );
        assert_eq!(
            activity["activity"]["metadata"]["button_urls"][0],
            "https://example.com"
        );
        assert_eq!(activity["activity"]["buttons"][0], "Site");
        assert_eq!(effects.replies[0]["data"]["name"], "");
    }

    #[test]
    fn null_activity_clears_bridge_state_for_socket() {
        let mut socket = RpcSession::new("9".to_owned(), "123".to_owned());

        let effects = handle_rpc_payload(
            &mut socket,
            &json!({
                "cmd": "SET_ACTIVITY",
                "args": {
                    "pid": 42,
                    "activity": null
                },
                "nonce": "clear"
            }),
        );

        assert_eq!(effects.activity.unwrap()["activity"], Value::Null);
        assert_eq!(effects.replies[0]["data"], Value::Null);
    }

    #[test]
    fn invite_browser_acknowledges_code() {
        let mut socket = RpcSession::new("10".to_owned(), "123".to_owned());

        let effects = handle_rpc_payload(
            &mut socket,
            &json!({
                "cmd": "INVITE_BROWSER",
                "args": { "code": "abc123" },
                "nonce": "invite"
            }),
        );

        assert_eq!(effects.replies[0]["cmd"], "INVITE_BROWSER");
        assert_eq!(effects.replies[0]["data"]["code"], "abc123");
        assert_eq!(effects.replies[0]["evt"], Value::Null);
        assert_eq!(effects.replies[0]["nonce"], "invite");
    }

    #[test]
    fn guild_template_browser_acknowledges_code() {
        let mut socket = RpcSession::new("11".to_owned(), "123".to_owned());

        let effects = handle_rpc_payload(
            &mut socket,
            &json!({
                "cmd": "GUILD_TEMPLATE_BROWSER",
                "args": { "code": "template123" },
                "nonce": "template"
            }),
        );

        assert_eq!(effects.replies[0]["cmd"], "GUILD_TEMPLATE_BROWSER");
        assert_eq!(effects.replies[0]["data"]["code"], "template123");
        assert_eq!(effects.replies[0]["evt"], Value::Null);
        assert_eq!(effects.replies[0]["nonce"], "template");
    }

    #[test]
    fn deep_link_acknowledges_success() {
        let mut socket = RpcSession::new("12".to_owned(), "123".to_owned());

        let effects = handle_rpc_payload(
            &mut socket,
            &json!({
                "cmd": "DEEP_LINK",
                "args": { "params": "discord://example" },
                "nonce": "link"
            }),
        );

        assert_eq!(effects.replies[0]["cmd"], "DEEP_LINK");
        assert_eq!(effects.replies[0]["data"], Value::Null);
        assert_eq!(effects.replies[0]["evt"], Value::Null);
        assert_eq!(effects.replies[0]["nonce"], "link");
    }

    #[test]
    fn app_name_lookup_uses_detectable_database() {
        #[derive(serde::Deserialize)]
        struct App {
            id: String,
            name: String,
        }

        let app = serde_json::from_str::<Vec<App>>(
            include_str!("../assets/detectable.json").trim_start_matches('\u{feff}'),
        )
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

        assert_eq!(super::app_name_by_id(&app.id), Some(app.name));
    }
}
