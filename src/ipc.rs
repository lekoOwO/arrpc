use std::{
    io::ErrorKind,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use anyhow::{anyhow, bail, Context};
use log::{debug, error, info};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[cfg(unix)]
use std::env;

#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

#[cfg(windows)]
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use crate::{
    bridge::Bridge,
    rpc::{rpc_ready_payload, RpcSession},
    state_file::StateFile,
};

const MAX_IPC_INDEX: u8 = 9;
const MAX_PACKET_SIZE: u32 = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct IpcServer {
    bridge: Bridge,
    next_socket_id: Arc<AtomicU64>,
    data_dir: Option<PathBuf>,
    state_file: Option<StateFile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IpcOpcode {
    Handshake = 0,
    Frame = 1,
    Close = 2,
    Ping = 3,
    Pong = 4,
}

#[derive(Debug)]
struct IpcPacket {
    opcode: IpcOpcode,
    data: Value,
}

impl TryFrom<i32> for IpcOpcode {
    type Error = anyhow::Error;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Handshake),
            1 => Ok(Self::Frame),
            2 => Ok(Self::Close),
            3 => Ok(Self::Ping),
            4 => Ok(Self::Pong),
            _ => Err(anyhow!("invalid IPC opcode {value}")),
        }
    }
}

impl IpcServer {
    pub fn new(bridge: Bridge) -> Self {
        Self {
            bridge,
            next_socket_id: Arc::new(AtomicU64::new(0)),
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
        run_platform_ipc_server(self).await
    }

    fn next_socket_id(&self) -> String {
        self.next_socket_id
            .fetch_add(1, Ordering::Relaxed)
            .to_string()
    }
}

#[cfg(windows)]
async fn run_platform_ipc_server(server: IpcServer) -> anyhow::Result<()> {
    let (path, mut pipe) = create_windows_pipe()?;
    if let Some(state_file) = &server.state_file {
        state_file.set_ipc_server(path.clone()).await;
    }
    info!("[arRPC > ipc] listening at {path}");

    loop {
        pipe.connect().await?;

        let client = pipe;
        pipe = ServerOptions::new()
            .create(&path)
            .with_context(|| format!("failed to create next pipe instance at {path}"))?;

        let server = server.clone();
        tokio::spawn(async move {
            let bridge = server.bridge.clone();
            let socket_id = server.next_socket_id();
            info!("[arRPC > ipc] client connected ({socket_id})");
            if let Err(err) =
                handle_ipc_client(bridge, socket_id.clone(), server.data_dir.clone(), client).await
            {
                error!("[arRPC > ipc] client error ({socket_id}): {err:#}");
            }
            info!("[arRPC > ipc] client disconnected ({socket_id})");
        });
    }
}

#[cfg(windows)]
fn create_windows_pipe() -> anyhow::Result<(String, NamedPipeServer)> {
    for index in 0..=MAX_IPC_INDEX {
        let path = format!(r"\\.\pipe\discord-ipc-{index}");
        match ServerOptions::new().first_pipe_instance(true).create(&path) {
            Ok(pipe) => return Ok((path, pipe)),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) if err.kind() == ErrorKind::PermissionDenied => continue,
            Err(err) => return Err(err.into()),
        }
    }

    bail!("no IPC named pipe slots were available")
}

#[cfg(unix)]
async fn run_platform_ipc_server(server: IpcServer) -> anyhow::Result<()> {
    let path = available_unix_socket_path().await?;
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("failed to bind IPC socket at {}", path.display()))?;
    if let Some(state_file) = &server.state_file {
        state_file
            .set_ipc_server(path.to_string_lossy().to_string())
            .await;
    }
    info!("[arRPC > ipc] listening at {}", path.display());

    loop {
        let (stream, _) = listener.accept().await?;
        let server = server.clone();

        tokio::spawn(async move {
            let socket_id = server.next_socket_id();
            info!("[arRPC > ipc] client connected ({socket_id})");
            if let Err(err) = handle_ipc_client(
                server.bridge.clone(),
                socket_id.clone(),
                server.data_dir.clone(),
                stream,
            )
            .await
            {
                error!("[arRPC > ipc] client error ({socket_id}): {err:#}");
            }
            info!("[arRPC > ipc] client disconnected ({socket_id})");
        });
    }
}

#[cfg(unix)]
async fn available_unix_socket_path() -> anyhow::Result<PathBuf> {
    let base = env::var_os("XDG_RUNTIME_DIR")
        .or_else(|| env::var_os("TMPDIR"))
        .or_else(|| env::var_os("TMP"))
        .or_else(|| env::var_os("TEMP"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));

    for index in 0..=MAX_IPC_INDEX {
        let path = base.join(format!("discord-ipc-{index}"));
        if UnixStream::connect(&path).await.is_ok() {
            continue;
        }

        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }

        return Ok(path);
    }

    bail!("no IPC Unix socket slots were available")
}

#[cfg(not(any(windows, unix)))]
async fn run_platform_ipc_server(_server: IpcServer) -> anyhow::Result<()> {
    futures_util::future::pending::<()>().await;
    Ok(())
}

async fn handle_ipc_client<S>(
    bridge: Bridge,
    socket_id: String,
    data_dir: Option<PathBuf>,
    mut stream: S,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut session: Option<RpcSession> = None;

    loop {
        let Some(packet) = read_packet(&mut stream).await? else {
            break;
        };

        debug!("[arRPC > ipc] received ({socket_id}) {:?}", packet);

        match packet.opcode {
            IpcOpcode::Handshake => {
                let client_id = validate_handshake(&packet.data)?;
                session = Some(
                    RpcSession::new_resolved(socket_id.clone(), client_id, data_dir.as_deref())
                        .await,
                );
                debug!("[arRPC > ipc] sending handshake reply ({socket_id})");
                write_packet(&mut stream, IpcOpcode::Frame, &rpc_ready_payload()).await?;
            }
            IpcOpcode::Ping => {
                write_packet(&mut stream, IpcOpcode::Pong, &packet.data).await?;
            }
            IpcOpcode::Pong => {}
            IpcOpcode::Close => break,
            IpcOpcode::Frame => {
                let Some(session) = session.as_mut() else {
                    write_close(&mut stream, 1003, "need to handshake first").await?;
                    break;
                };

                let effects = session.handle_payload(&packet.data);
                for reply in effects.replies {
                    debug!("[arRPC > ipc] sending ({socket_id}) {reply}");
                    write_packet(&mut stream, IpcOpcode::Frame, &reply).await?;
                }
                if let Some(activity) = effects.activity {
                    bridge.send(activity).await;
                }
            }
        }
    }

    if let Some(session) = session {
        bridge.send(session.close_activity()).await;
    }

    Ok(())
}

fn validate_handshake(data: &Value) -> anyhow::Result<String> {
    let version = data.get("v").and_then(Value::as_u64).unwrap_or(1);
    if version != 1 {
        bail!("unsupported version requested: {version}");
    }

    let client_id = data
        .get("client_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if client_id.is_empty() {
        bail!("client id required");
    }

    Ok(client_id.to_owned())
}

async fn read_packet<R>(reader: &mut R) -> anyhow::Result<Option<IpcPacket>>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0u8; 8];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) if err.kind() == ErrorKind::ConnectionReset => return Ok(None),
        Err(err) => return Err(err.into()),
    }

    let opcode = IpcOpcode::try_from(i32::from_le_bytes(header[0..4].try_into()?))?;
    let length = u32::from_le_bytes(header[4..8].try_into()?);
    if length > MAX_PACKET_SIZE {
        bail!("IPC packet too large: {length} bytes");
    }

    let mut body = vec![0u8; length as usize];
    reader.read_exact(&mut body).await?;
    let data = serde_json::from_slice(&body)?;

    Ok(Some(IpcPacket { opcode, data }))
}

async fn write_close<W>(writer: &mut W, code: u16, message: &str) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
{
    write_packet(
        writer,
        IpcOpcode::Close,
        &json!({
            "code": code,
            "message": message
        }),
    )
    .await
}

async fn write_packet<W>(writer: &mut W, opcode: IpcOpcode, data: &Value) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let body = serde_json::to_vec(data)?;
    if body.len() > MAX_PACKET_SIZE as usize {
        bail!("IPC packet too large: {} bytes", body.len());
    }

    writer.write_all(&(opcode as i32).to_le_bytes()).await?;
    writer.write_all(&(body.len() as u32).to_le_bytes()).await?;
    writer.write_all(&body).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{read_packet, validate_handshake, write_packet, IpcOpcode};

    #[test]
    fn handshake_requires_client_id() {
        assert!(validate_handshake(&json!({ "v": 1 })).is_err());
        assert_eq!(
            validate_handshake(&json!({ "v": 1, "client_id": "123" })).unwrap(),
            "123"
        );
    }

    #[tokio::test]
    async fn packet_round_trip_preserves_opcode_and_json() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let payload = json!({
            "cmd": "PING",
            "nonce": "abc"
        });

        write_packet(&mut client, IpcOpcode::Ping, &payload)
            .await
            .unwrap();
        let packet = read_packet(&mut server).await.unwrap().unwrap();

        assert_eq!(packet.opcode, IpcOpcode::Ping);
        assert_eq!(packet.data, payload);
    }
}
