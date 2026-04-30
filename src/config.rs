use std::{env, ops::RangeInclusive, path::PathBuf};

use anyhow::Context;

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const BRIDGE_PORTS: RangeInclusive<u16> = 1337..=1347;
pub const BRIDGE_PORTS_HYPERV: RangeInclusive<u16> = 60000..=60020;
pub const WEBSOCKET_PORTS: RangeInclusive<u16> = 6463..=6472;
pub const WEBSOCKET_PORTS_HYPERV: RangeInclusive<u16> = 60100..=60120;

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub debug: bool,
    pub bridge_enabled: bool,
    pub steam_enabled: bool,
    pub process_scanning_enabled: bool,
    pub state_file_enabled: bool,
    pub parent_monitor_enabled: bool,
    pub bridge_host: String,
    pub bridge_ports: Vec<u16>,
    pub websocket_host: String,
    pub websocket_ports: Vec<u16>,
    pub data_dir: Option<PathBuf>,
    pub ignore_list_file: Option<PathBuf>,
    pub list_database: bool,
    pub list_detected: bool,
    pub update_db: bool,
    pub validate_fixes: bool,
}

impl RuntimeConfig {
    pub fn from_env_and_args() -> anyhow::Result<Self> {
        Self::parse(env::vars(), env::args())
    }

    pub fn parse(
        vars: impl IntoIterator<Item = (String, String)>,
        args: impl IntoIterator<Item = String>,
    ) -> anyhow::Result<Self> {
        let vars = vars
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>();
        let args = args.into_iter().collect::<Vec<_>>();

        let bridge_host = vars
            .get("ARRPC_BRIDGE_HOST")
            .cloned()
            .unwrap_or_else(|| DEFAULT_HOST.to_owned());
        let websocket_host = vars
            .get("ARRPC_WEBSOCKET_HOST")
            .cloned()
            .unwrap_or_else(|| DEFAULT_HOST.to_owned());

        let bridge_ports = if let Some(port) = vars.get("ARRPC_BRIDGE_PORT") {
            vec![parse_port("ARRPC_BRIDGE_PORT", port)?]
        } else {
            BRIDGE_PORTS.chain(BRIDGE_PORTS_HYPERV).collect()
        };

        Ok(Self {
            debug: vars.contains_key("ARRPC_DEBUG")
                || args.iter().any(|arg| arg == "--debug" || arg == "-d"),
            bridge_enabled: !vars.contains_key("ARRPC_NO_BRIDGE"),
            steam_enabled: !vars.contains_key("ARRPC_NO_STEAM"),
            process_scanning_enabled: !vars.contains_key("ARRPC_NO_PROCESS_SCANNING")
                && !args.iter().any(|arg| arg == "--no-process-scanning"),
            state_file_enabled: vars.contains_key("ARRPC_STATE_FILE"),
            parent_monitor_enabled: vars.contains_key("ARRPC_PARENT_MONITOR"),
            bridge_host,
            bridge_ports,
            websocket_host,
            websocket_ports: WEBSOCKET_PORTS.chain(WEBSOCKET_PORTS_HYPERV).collect(),
            data_dir: vars.get("ARRPC_DATA_DIR").map(PathBuf::from),
            ignore_list_file: vars.get("ARRPC_IGNORE_LIST_FILE").map(PathBuf::from),
            list_database: args.iter().any(|arg| arg == "--list-database"),
            list_detected: args.iter().any(|arg| arg == "--list-detected"),
            update_db: args
                .iter()
                .any(|arg| arg == "--update-db" || arg == "update-db"),
            validate_fixes: args
                .iter()
                .any(|arg| arg == "--validate-fixes" || arg == "validate-fixes"),
        })
    }
}

fn parse_port(name: &str, value: &str) -> anyhow::Result<u16> {
    value
        .parse::<u16>()
        .with_context(|| format!("invalid {name}: {value}"))
}

#[cfg(test)]
mod tests {
    use super::RuntimeConfig;

    #[test]
    fn config_accepts_arrpc_bun_environment_surface() {
        let config = RuntimeConfig::parse(
            [
                ("ARRPC_NO_BRIDGE".to_owned(), "1".to_owned()),
                ("ARRPC_NO_STEAM".to_owned(), "1".to_owned()),
                ("ARRPC_STATE_FILE".to_owned(), "1".to_owned()),
                ("ARRPC_BRIDGE_HOST".to_owned(), "0.0.0.0".to_owned()),
                ("ARRPC_BRIDGE_PORT".to_owned(), "1444".to_owned()),
                ("ARRPC_WEBSOCKET_HOST".to_owned(), "localhost".to_owned()),
                ("ARRPC_DATA_DIR".to_owned(), "/tmp/arrpc".to_owned()),
                (
                    "ARRPC_IGNORE_LIST_FILE".to_owned(),
                    "/tmp/ignore.json".to_owned(),
                ),
            ],
            [
                "arrpc".to_owned(),
                "--list-detected".to_owned(),
                "validate-fixes".to_owned(),
            ],
        )
        .unwrap();

        assert!(!config.bridge_enabled);
        assert!(!config.steam_enabled);
        assert!(config.state_file_enabled);
        assert_eq!(config.bridge_host, "0.0.0.0");
        assert_eq!(config.bridge_ports, vec![1444]);
        assert_eq!(config.websocket_host, "localhost");
        assert!(config.data_dir.is_some());
        assert!(config.ignore_list_file.is_some());
        assert!(config.list_detected);
        assert!(config.validate_fixes);
    }
}
