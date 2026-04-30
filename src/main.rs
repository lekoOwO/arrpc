use std::time::Duration;

use anyhow::Context;
use arrpc::{
    bridge::Bridge,
    cli,
    config::RuntimeConfig,
    ignore_list::IgnoreList,
    ipc::IpcServer,
    process_scan::{ProcessScanner, ProcessScannerOptions},
    rpc::RpcServer,
    state_file::StateFile,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = RuntimeConfig::from_env_and_args()?;

    let log_level = if config.debug {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };

    env_logger::Builder::new()
        .filter_level(log_level)
        .format_target(false)
        .format_timestamp(None)
        .init();

    if config.list_database {
        return cli::list_database(&config).await;
    }

    if config.list_detected {
        return cli::list_detected(&config).await;
    }

    if config.update_db {
        return cli::update_db(&config).await;
    }

    if config.validate_fixes {
        return cli::validate_fixes(&config).await;
    }

    println!("arRPC Rust v4.0.0-rust");

    let state_file = if config.state_file_enabled {
        let state_file = StateFile::create(env!("CARGO_PKG_VERSION")).await?;
        println!("[arRPC > state] writing {}", state_file.path().display());
        Some(state_file)
    } else {
        None
    };

    let bridge = if let Some(state_file) = &state_file {
        Bridge::with_state_file(state_file.clone())
    } else {
        Bridge::default()
    };

    let bridge_task = config.bridge_enabled.then(|| {
        let bridge = bridge.clone();
        let host = config.bridge_host.clone();
        let ports = config.bridge_ports.clone();
        tokio::spawn(async move { bridge.run(host, ports).await })
    });

    let ignore_list = if let Some(path) = &config.ignore_list_file {
        IgnoreList::from_file(path).await.unwrap_or_else(|err| {
            eprintln!(
                "[arRPC > ignore-list] failed to load {}: {err:#}",
                path.display()
            );
            IgnoreList::default()
        })
    } else {
        IgnoreList::default()
    };

    if config.process_scanning_enabled {
        let scanner = ProcessScanner::with_options(
            bridge.clone(),
            Duration::from_secs(5),
            ProcessScannerOptions {
                data_dir: config.data_dir.clone(),
                ignore_list,
                steam_enabled: config.steam_enabled,
            },
        )?;
        tokio::spawn(async move {
            if let Err(err) = scanner.run().await {
                eprintln!("[arRPC > process] stopped: {err:#}");
            }
        });
    }

    let mut ipc = IpcServer::new(bridge.clone()).with_data_dir(config.data_dir.clone());
    if let Some(state_file) = &state_file {
        ipc = ipc.with_state_file(state_file.clone());
    }
    let ipc_task = tokio::spawn(async move { ipc.run().await });

    let mut rpc = RpcServer::with_bind(
        bridge.clone(),
        config.websocket_host.clone(),
        config.websocket_ports.clone(),
    )
    .with_data_dir(config.data_dir.clone());
    if let Some(state_file) = &state_file {
        rpc = rpc.with_state_file(state_file.clone());
    }
    let rpc_task = tokio::spawn(async move { rpc.run().await });

    if config.parent_monitor_enabled {
        let initial_parent = parent_process_id();
        tokio::spawn(async move {
            while parent_process_id() == initial_parent {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            std::process::exit(0);
        });
    }

    match bridge_task {
        Some(bridge_task) => {
            tokio::select! {
                result = bridge_task => result.context("bridge task panicked")??,
                result = ipc_task => result.context("ipc task panicked")??,
                result = rpc_task => result.context("rpc task panicked")??,
                _ = tokio::signal::ctrl_c() => {}
            }
        }
        None => {
            tokio::select! {
                result = ipc_task => result.context("ipc task panicked")??,
                result = rpc_task => result.context("rpc task panicked")??,
                _ = tokio::signal::ctrl_c() => {}
            }
        }
    }

    bridge.cleanup_state_file().await;

    Ok(())
}

fn parent_process_id() -> u32 {
    #[cfg(target_os = "linux")]
    {
        let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else {
            return 0;
        };
        let Some(after_name) = stat.rsplit_once(") ") else {
            return 0;
        };
        after_name
            .1
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or_default()
    }
    #[cfg(target_os = "macos")]
    {
        let Ok(output) = std::process::Command::new("ps")
            .args(["-o", "ppid=", "-p", &std::process::id().to_string()])
            .output()
        else {
            return 0;
        };
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u32>()
            .unwrap_or_default()
    }
    #[cfg(target_os = "windows")]
    {
        let command = format!(
            "(Get-CimInstance Win32_Process -Filter \"ProcessId={}\").ParentProcessId",
            std::process::id()
        );
        let Ok(output) = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &command])
            .output()
        else {
            return 0;
        };
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u32>()
            .unwrap_or_default()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        0
    }
}
