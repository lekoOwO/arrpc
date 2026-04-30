use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use log::{debug, info};
use serde::Deserialize;
use serde_json::json;
#[cfg(any(target_os = "windows", target_os = "macos"))]
use tokio::process::Command;
use tokio::time;

use crate::{bridge::Bridge, ignore_list::IgnoreList};

#[cfg(target_os = "linux")]
const ANTI_CHEAT_EXECUTABLES: &[&str] = &[
    "easyanticheat",
    "eac_launcher",
    "easyanticheat_eos",
    "battleye",
    "beclient",
    "nprotect",
    "xigncode",
    "gameguard",
    "vanguard",
    "anticheattoolkit",
];

#[cfg(target_os = "windows")]
const SYSTEM_EXECUTABLES: &[&str] = &[
    "system",
    "registry",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "services.exe",
    "lsass.exe",
    "svchost.exe",
    "dwm.exe",
    "conhost.exe",
    "taskhost.exe",
    "winlogon.exe",
    "fontdrvhost.exe",
    "sihost.exe",
    "ctfmon.exe",
    "taskhostw.exe",
    "runtimebroker.exe",
    "searchindexer.exe",
    "searchprotocolhost.exe",
];

#[derive(Debug, Clone, Deserialize)]
struct DetectableApp {
    id: String,
    #[serde(default = "custom_game_name")]
    name: String,
    aliases: Option<Vec<String>>,
    executables: Option<Vec<DetectableExecutable>>,
}

#[derive(Debug, Clone, Deserialize)]
struct DetectableExecutable {
    name: String,
    #[serde(default)]
    is_launcher: bool,
    arguments: Option<String>,
}

#[derive(Debug, Clone)]
struct ProcessInfo {
    pid: u32,
    path: String,
    args: Vec<String>,
}

#[cfg(target_os = "windows")]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WindowsProcessRecord {
    process_id: Option<u32>,
    executable_path: Option<String>,
    command_line: Option<String>,
}

#[derive(Debug, Clone)]
struct ActiveProcess {
    name: String,
    pid: u32,
    started_at: u128,
}

#[derive(Debug, Clone, Default)]
struct SteamLookup {
    installs: Vec<(String, String)>,
}

impl SteamLookup {
    fn load() -> Self {
        let installs = steam_library_paths()
            .into_iter()
            .flat_map(|path| scan_steam_library(&path).unwrap_or_default())
            .collect();

        Self { installs }
    }

    fn resolve(&self, process: &ProcessInfo) -> Option<ProcessInfo> {
        if self.installs.is_empty() {
            return None;
        }

        let normalized_process_path = normalize_steam_path(&process.path);
        if is_steam_runtime_path(&normalized_process_path) {
            return None;
        }

        self.installs.iter().find_map(|(install_path, name)| {
            let normalized_install_path = normalize_steam_path(install_path);
            normalized_process_path
                .starts_with(&normalized_install_path)
                .then(|| ProcessInfo {
                    pid: process.pid,
                    path: format!("{install_path}/{name}.app_name"),
                    args: process.args.clone(),
                })
        })
    }
}

fn steam_library_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "windows")]
    {
        let base = std::env::var_os("ProgramFiles(x86)")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"));
        paths.push(base.join("Steam"));
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            paths.push(home.join(".steam/steam"));
            paths.push(home.join(".local/share/Steam"));
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            paths.push(home.join("Library/Application Support/Steam"));
        }
    }

    paths
}

fn scan_steam_library(steam_path: &Path) -> anyhow::Result<Vec<(String, String)>> {
    let mut installs = Vec::new();
    let vdf_path = steam_path.join("steamapps/libraryfolders.vdf");
    let content = std::fs::read_to_string(vdf_path)?;

    for library_path in parse_libraryfolders_vdf(&content) {
        let steamapps_path = PathBuf::from(library_path).join("steamapps");
        let Ok(entries) = std::fs::read_dir(&steamapps_path) else {
            continue;
        };

        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            if !file_name.starts_with("appmanifest_") || !file_name.ends_with(".acf") {
                continue;
            }
            if let Ok(raw) = std::fs::read_to_string(entry.path()) {
                if let Some((name, install_dir)) = parse_appmanifest(&raw) {
                    installs.push((
                        steamapps_path
                            .join("common")
                            .join(install_dir)
                            .to_string_lossy()
                            .to_string(),
                        name,
                    ));
                }
            }
        }
    }

    Ok(installs)
}

fn parse_libraryfolders_vdf(content: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in content.lines() {
        let Some(path) = parse_vdf_pair(line, "path") else {
            continue;
        };
        paths.push(path.replace(r"\\", r"\"));
    }
    paths
}

fn parse_appmanifest(content: &str) -> Option<(String, String)> {
    let name = content
        .lines()
        .find_map(|line| parse_vdf_pair(line, "name"))?;
    let install_dir = content
        .lines()
        .find_map(|line| parse_vdf_pair(line, "installdir"))?;
    Some((name, install_dir))
}

fn parse_vdf_pair(line: &str, key: &str) -> Option<String> {
    let line = line.trim();
    let prefix = format!("\"{key}\"");
    let rest = line.strip_prefix(&prefix)?.trim();
    let value = rest.strip_prefix('"')?;
    let end = value.find('"')?;
    Some(value[..end].to_owned())
}

fn normalize_steam_path(path: &str) -> String {
    let mut path = path.replace('\\', "/");
    if path.len() > 2 && path.as_bytes()[1] == b':' {
        path = path[2..].to_owned();
    }
    path.to_lowercase()
}

fn is_steam_runtime_path(path: &str) -> bool {
    [
        "steamlinuxruntime",
        "proton",
        "pressure-vessel",
        "steam-runtime",
        "compatibilitytools.d",
    ]
    .iter()
    .any(|runtime| path.contains(runtime))
}

pub struct ProcessScanner {
    bridge: Bridge,
    interval: Duration,
    apps: Vec<DetectableApp>,
    ignore_list: IgnoreList,
    steam_lookup: SteamLookup,
    active: HashMap<String, ActiveProcess>,
    running: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
pub struct ProcessScannerOptions {
    pub data_dir: Option<PathBuf>,
    pub ignore_list: IgnoreList,
    pub steam_enabled: bool,
}

impl Default for ProcessScannerOptions {
    fn default() -> Self {
        Self {
            data_dir: None,
            ignore_list: IgnoreList::default(),
            steam_enabled: true,
        }
    }
}

impl ProcessScanner {
    pub fn new(bridge: Bridge, interval: Duration) -> anyhow::Result<Self> {
        Self::with_options(bridge, interval, ProcessScannerOptions::default())
    }

    pub fn with_options(
        bridge: Bridge,
        interval: Duration,
        options: ProcessScannerOptions,
    ) -> anyhow::Result<Self> {
        let apps = load_detectable_apps(options.data_dir.as_deref())?;

        Ok(Self {
            bridge,
            interval,
            apps,
            ignore_list: options.ignore_list,
            steam_lookup: if options.steam_enabled {
                SteamLookup::load()
            } else {
                SteamLookup::default()
            },
            active: HashMap::new(),
            running: Arc::new(AtomicBool::new(false)),
        })
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        self.scan_guarded().await?;

        let mut interval = time::interval(self.interval);
        loop {
            interval.tick().await;
            debug!("[arRPC > process] scanning...");
            self.scan_guarded().await?;
        }
    }

    async fn scan_guarded(&mut self) -> anyhow::Result<bool> {
        if self.running.swap(true, Ordering::AcqRel) {
            return Ok(false);
        }

        let result = self.scan_once().await;
        self.running.store(false, Ordering::Release);
        result.map(|_| true)
    }

    async fn scan_once(&mut self) -> anyhow::Result<()> {
        let processes = get_processes().await?;
        let mut seen = HashSet::new();

        for process in processes {
            let resolved_process = self.steam_lookup.resolve(&process).unwrap_or(process);
            let pid = resolved_process.pid;
            let Some(app) = detect_app(&self.apps, &resolved_process) else {
                continue;
            };
            if self
                .ignore_list
                .should_ignore(&app.id, &resolved_process.path, &app.name)
            {
                continue;
            }
            seen.insert(app.id.clone());
            let active = self
                .active
                .entry(app.id.clone())
                .or_insert_with(|| ActiveProcess {
                    name: app.name.clone(),
                    pid,
                    started_at: now_ms(),
                });
            if active.pid != pid || !seen.contains(&app.id) {
                info!(
                    "[arRPC > process] found game! {} ({}) pid: {}",
                    app.name, app.id, pid
                );
                active.pid = pid;
            }

            self.bridge
                .send(json!({
                    "activity": {
                        "application_id": app.id,
                        "name": active.name,
                        "timestamps": { "start": active.started_at }
                    },
                    "pid": pid,
                    "socketId": app.id
                }))
                .await;
        }

        let lost = self
            .active
            .keys()
            .filter(|id| !seen.contains(*id))
            .cloned()
            .collect::<Vec<_>>();

        for id in lost {
            if let Some(active) = self.active.remove(&id) {
                info!("[arRPC > process] lost game! {}", active.name);
                self.bridge
                    .send(json!({
                        "activity": null,
                        "pid": active.pid,
                        "socketId": id
                    }))
                    .await;
            }
        }

        Ok(())
    }
}

fn load_detectable_apps(data_dir: Option<&Path>) -> anyhow::Result<Vec<DetectableApp>> {
    let mut apps = parse_detectable_apps(if let Some(data_dir) = data_dir {
        let path = data_dir.join("detectable.json");
        std::fs::read_to_string(&path)
            .unwrap_or_else(|_| include_str!("../assets/detectable.json").to_owned())
    } else {
        include_str!("../assets/detectable.json").to_owned()
    })?;

    let fixes = parse_detectable_apps(if let Some(data_dir) = data_dir {
        let path = data_dir.join("detectable_fixes.json");
        std::fs::read_to_string(&path)
            .unwrap_or_else(|_| include_str!("../assets/detectable_fixes.json").to_owned())
    } else {
        include_str!("../assets/detectable_fixes.json").to_owned()
    })?;

    merge_detectable_fixes(&mut apps, fixes);
    Ok(apps)
}

fn parse_detectable_apps(raw: String) -> anyhow::Result<Vec<DetectableApp>> {
    serde_json::from_str(raw.trim_start_matches('\u{feff}'))
        .context("failed to parse detectable process database")
}

fn custom_game_name() -> String {
    "Custom Game".to_owned()
}

fn merge_detectable_fixes(apps: &mut Vec<DetectableApp>, fixes: Vec<DetectableApp>) {
    for fix in fixes {
        if let Some(existing) = apps.iter_mut().find(|app| app.id == fix.id) {
            if let Some(name) = non_empty_name(&fix.name) {
                existing.name = name.to_owned();
            }
            if let Some(aliases) = fix.aliases {
                existing.aliases = Some(aliases);
            }
            if let Some(mut executables) = fix.executables {
                existing
                    .executables
                    .get_or_insert_with(Vec::new)
                    .append(&mut executables);
            }
        } else {
            apps.push(fix);
        }
    }
}

fn non_empty_name(name: &str) -> Option<&str> {
    (!name.is_empty() && name != "Custom Game").then_some(name)
}

fn detect_app<'a>(apps: &'a [DetectableApp], process: &ProcessInfo) -> Option<&'a DetectableApp> {
    let candidates = path_candidates(&process.path);

    apps.iter().find(|app| {
        if matches_app_name_marker(&app.name, &candidates) {
            return true;
        }

        app.executables.as_ref().is_some_and(|executables| {
            executables
                .iter()
                .any(|exe| matches_executable(exe, &candidates, &process.args, false, true))
                || executables
                    .iter()
                    .any(|exe| matches_executable(exe, &candidates, &process.args, true, true))
                || executables
                    .iter()
                    .any(|exe| matches_executable(exe, &candidates, &process.args, false, false))
                || executables
                    .iter()
                    .any(|exe| matches_executable(exe, &candidates, &process.args, true, false))
        })
    })
}

fn matches_app_name_marker(app_name: &str, candidates: &[String]) -> bool {
    candidates.first().is_some_and(|candidate| {
        candidate
            .strip_suffix(".app_name")
            .is_some_and(|marker| marker == app_name.to_lowercase())
    })
}

fn matches_executable(
    exe: &DetectableExecutable,
    candidates: &[String],
    args: &[String],
    check_launcher: bool,
    strict_args: bool,
) -> bool {
    if exe.is_launcher != check_launcher {
        return false;
    }

    let Some(first_candidate) = candidates.first() else {
        return false;
    };

    let exact_match = exe.name.starts_with('>');
    let name_matches = if let Some(exact) = exe.name.strip_prefix('>') {
        first_candidate == exact
    } else {
        candidates.iter().any(|candidate| candidate == &exe.name)
    };

    if !name_matches {
        return false;
    }

    let Some(arguments) = exe.arguments.as_deref() else {
        return true;
    };

    let args_match = args_contain_string(args, arguments);
    if strict_args || exact_match {
        return args_match;
    }

    true
}

fn args_contain_string(args: &[String], target: &str) -> bool {
    let target = target.to_lowercase();

    if args.iter().any(|arg| arg.to_lowercase().contains(&target)) {
        return true;
    }

    for start in 0..args.len() {
        let mut combined = args[start].to_lowercase();
        for arg in args
            .iter()
            .skip(start + 1)
            .take(4.min(args.len().saturating_sub(start + 1)))
        {
            combined.push(' ');
            combined.push_str(&arg.to_lowercase());
            if combined.contains(&target) {
                return true;
            }
        }
    }

    false
}

fn path_candidates(path: &str) -> Vec<String> {
    let normalized = path.to_lowercase().replace('\\', "/");
    let parts = normalized.split('/').collect::<Vec<_>>();
    let mut candidates = Vec::new();

    for len in 1..=parts.len() {
        candidates.push(parts[parts.len() - len..].join("/"));
    }

    let originals = candidates.clone();
    for candidate in originals {
        candidates.push(candidate.replace("64", ""));
        candidates.push(candidate.replace(".x64", ""));
        candidates.push(candidate.replace("x64", ""));
        candidates.push(candidate.replace("_64", ""));
    }

    candidates
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

async fn get_processes() -> anyhow::Result<Vec<ProcessInfo>> {
    #[cfg(target_os = "windows")]
    {
        get_windows_processes().await
    }

    #[cfg(target_os = "linux")]
    {
        get_linux_processes().await
    }

    #[cfg(target_os = "macos")]
    {
        get_macos_processes().await
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        Ok(Vec::new())
    }
}

#[cfg(target_os = "windows")]
async fn get_windows_processes() -> anyhow::Result<Vec<ProcessInfo>> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_Process | Select-Object ProcessId,ExecutablePath,CommandLine | ConvertTo-Json -Compress",
        ])
        .output()
        .await
        .context("failed to run PowerShell process scan")?;

    parse_windows_processes(&output.stdout)
}

#[cfg(target_os = "windows")]
fn parse_windows_processes(raw: &[u8]) -> anyhow::Result<Vec<ProcessInfo>> {
    let value = serde_json::from_slice::<serde_json::Value>(raw)?;
    let records = match value {
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<WindowsProcessRecord>, _>>()?,
        serde_json::Value::Object(_) => vec![serde_json::from_value(value)?],
        serde_json::Value::Null => Vec::new(),
        _ => anyhow::bail!("unexpected PowerShell process JSON shape"),
    };

    let mut processes = Vec::new();

    for record in records {
        let Some(pid) = record.process_id else {
            continue;
        };
        let Some(path) = record.executable_path.filter(|path| !path.is_empty()) else {
            continue;
        };
        if executable_name(&path)
            .map(|name| SYSTEM_EXECUTABLES.contains(&name.to_lowercase().as_str()))
            .unwrap_or(false)
        {
            continue;
        }

        let command_line = record.command_line.unwrap_or_default();
        let args = parse_windows_command_line(&command_line);

        processes.push(ProcessInfo { pid, path, args });
    }

    Ok(processes)
}

#[cfg(target_os = "windows")]
fn parse_windows_command_line(command_line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut chars = command_line.chars().peekable();
    let mut in_quotes = false;

    while let Some(ch) = chars.next() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ' ' | '\t' if !in_quotes => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            '\\' => {
                let mut slash_count = 1;
                while chars.peek() == Some(&'\\') {
                    chars.next();
                    slash_count += 1;
                }

                if chars.peek() == Some(&'"') {
                    current.extend(std::iter::repeat_n('\\', slash_count / 2));
                    if slash_count % 2 == 0 {
                        chars.next();
                        in_quotes = !in_quotes;
                    } else {
                        chars.next();
                        current.push('"');
                    }
                } else {
                    current.extend(std::iter::repeat_n('\\', slash_count));
                }
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        args.push(current);
    }

    args
}

#[cfg(target_os = "linux")]
async fn get_linux_processes() -> anyhow::Result<Vec<ProcessInfo>> {
    let mut entries = tokio::fs::read_dir("/proc").await?;
    let mut processes = Vec::new();

    while let Some(entry) = entries.next_entry().await? {
        let file_name = entry.file_name();
        let Some(pid_text) = file_name.to_str() else {
            continue;
        };
        let Ok(pid) = pid_text.parse::<u32>() else {
            continue;
        };

        let proc_path = entry.path();
        let cmdline_path = proc_path.join("cmdline");
        let Ok(raw) = tokio::fs::read(&cmdline_path).await else {
            continue;
        };
        if raw.is_empty() {
            continue;
        }

        if let Ok(stat) = tokio::fs::read_to_string(proc_path.join("stat")).await {
            if process_is_stopped(&stat) {
                continue;
            }
        }

        let parts = raw
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).to_string())
            .collect::<Vec<_>>();

        let Some(path) = parts.first() else {
            continue;
        };
        let mut path = path.clone();
        if let Ok(exe_path) = tokio::fs::read_link(proc_path.join("exe")).await {
            let exe_path = exe_path.to_string_lossy().to_string();
            if !exe_path.contains("(deleted)") && !is_wine_host_path(&exe_path) {
                path = exe_path;
            }
        }

        if is_anti_cheat_process(&path, &String::from_utf8_lossy(&raw)) {
            continue;
        }

        processes.push(ProcessInfo {
            pid,
            path,
            args: parts.into_iter().skip(1).collect(),
        });
    }

    Ok(processes)
}

#[cfg(target_os = "linux")]
fn process_is_stopped(stat: &str) -> bool {
    stat.rsplit_once(") ")
        .and_then(|(_, rest)| rest.chars().next())
        .is_some_and(|state| state == 'T' || state == 't')
}

#[cfg(target_os = "linux")]
fn is_wine_host_path(path: &str) -> bool {
    path.contains("/wine") || path.contains("/wine64")
}

#[cfg(target_os = "linux")]
fn is_anti_cheat_process(path: &str, command_line: &str) -> bool {
    let path = path.to_lowercase();
    let command_line = command_line.to_lowercase();
    ANTI_CHEAT_EXECUTABLES
        .iter()
        .any(|name| path.contains(name) || command_line.contains(name))
}

#[cfg(target_os = "macos")]
async fn get_macos_processes() -> anyhow::Result<Vec<ProcessInfo>> {
    let output = Command::new("ps")
        .args(["-awwxo", "pid=,args="])
        .output()
        .await
        .context("failed to run ps")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut processes = Vec::new();

    for line in stdout.lines() {
        let line = line.trim_start();
        let Some((pid_text, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = pid_text.parse::<u32>() else {
            continue;
        };

        let command_line = rest.trim_start();
        if command_line.is_empty() || command_line.starts_with('[') || command_line.starts_with('<')
        {
            continue;
        }

        let (path, args) = parse_macos_command_line(command_line);
        if path.is_empty() {
            continue;
        }

        processes.push(ProcessInfo { pid, path, args });
    }

    Ok(processes)
}

#[cfg(target_os = "macos")]
fn parse_macos_command_line(command_line: &str) -> (String, Vec<String>) {
    let lower = command_line.to_lowercase();
    if let Some(app_index) = lower.find(".app") {
        let mut path_end = app_index + 4;
        if command_line[path_end..].starts_with("/Contents/MacOS/") {
            let contents_start = path_end + "/Contents/MacOS/".len();
            path_end = command_line.len();
            for (offset, ch) in command_line[contents_start..].char_indices() {
                if ch == ' ' {
                    let index = contents_start + offset;
                    let next = command_line[index..].trim_start();
                    if next.starts_with('-') || next.starts_with('+') || next.is_empty() {
                        path_end = index;
                        break;
                    }
                }
            }
        }

        let mut app_path = command_line[..app_index + 4].to_owned();
        let rest = command_line[path_end..].trim();
        let args = split_shellish(rest);

        if let Some(home) = std::env::var_os("HOME") {
            let parallels_dir = PathBuf::from(home)
                .join("Applications (Parallels)")
                .to_string_lossy()
                .to_string();
            if app_path.starts_with(&parallels_dir) {
                if app_path.ends_with(".exe.app") {
                    app_path.truncate(app_path.len() - 4);
                } else {
                    app_path.push_str("_name");
                }
            }
        }

        return (app_path, args);
    }

    if let Some((path, rest)) = split_windows_exe_from_command(command_line) {
        return (path, split_shellish(rest.trim()));
    }

    let mut parts = split_shellish(command_line);
    if parts.is_empty() {
        return (String::new(), Vec::new());
    }
    let path = parts.remove(0);
    (path, parts)
}

#[cfg(target_os = "macos")]
fn split_windows_exe_from_command(command_line: &str) -> Option<(String, &str)> {
    let exe_end = command_line.to_lowercase().find(".exe")? + 4;
    let before = &command_line[..exe_end];
    let drive_index = before
        .char_indices()
        .rev()
        .find(|(_, ch)| *ch == ':' || *ch == '\\')
        .map(|(index, ch)| {
            if ch == ':' {
                index.saturating_sub(1)
            } else {
                index + 1
            }
        })
        .unwrap_or(0);
    let path = before[drive_index..].to_owned();
    if path.contains('\\') {
        Some((path, &command_line[exe_end..]))
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn split_shellish(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

#[allow(dead_code)]
fn executable_name(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::{
        detect_app, merge_detectable_fixes, parse_detectable_apps, path_candidates, DetectableApp,
        DetectableExecutable, ProcessInfo, SteamLookup,
    };

    #[test]
    fn candidates_include_suffix_paths_and_64_bit_variants() {
        let candidates = path_candidates("C:\\Games\\Example64\\game_x64.exe");

        assert!(candidates.contains(&"game_x64.exe".to_owned()));
        assert!(candidates.contains(&"game_.exe".to_owned()));
        assert!(candidates.contains(&"games/example64/game_x64.exe".to_owned()));
    }

    #[test]
    fn detection_matches_executable_and_required_arguments() {
        let apps = vec![DetectableApp {
            id: "1".to_owned(),
            name: "Example".to_owned(),
            aliases: None,
            executables: Some(vec![DetectableExecutable {
                name: "game.exe".to_owned(),
                is_launcher: false,
                arguments: Some("--rpc".to_owned()),
            }]),
        }];

        let process = ProcessInfo {
            pid: 10,
            path: "C:/Games/game.exe".to_owned(),
            args: vec!["--rpc".to_owned()],
        };

        let app = detect_app(&apps, &process).expect("app should be detected");
        assert_eq!(app.name, "Example");
    }

    #[test]
    fn custom_fixes_merge_executables_into_existing_app() {
        let mut apps = vec![DetectableApp {
            id: "1".to_owned(),
            name: "Example".to_owned(),
            aliases: None,
            executables: None,
        }];
        merge_detectable_fixes(
            &mut apps,
            vec![DetectableApp {
                id: "1".to_owned(),
                name: String::new(),
                aliases: None,
                executables: Some(vec![DetectableExecutable {
                    name: "fixed.exe".to_owned(),
                    is_launcher: false,
                    arguments: None,
                }]),
            }],
        );

        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].executables.as_ref().unwrap()[0].name, "fixed.exe");
    }

    #[test]
    fn custom_fix_without_name_defaults_to_custom_game() {
        let fixes = parse_detectable_apps(
            r#"[{"id":"custom","executables":[{"name":"custom.exe","is_launcher":false}]}]"#
                .to_owned(),
        )
        .unwrap();

        assert_eq!(fixes[0].name, "Custom Game");
    }

    #[test]
    fn detection_matches_launcher_executables() {
        let apps = vec![DetectableApp {
            id: "1".to_owned(),
            name: "Example".to_owned(),
            aliases: None,
            executables: Some(vec![DetectableExecutable {
                name: "launcher.exe".to_owned(),
                is_launcher: true,
                arguments: None,
            }]),
        }];

        let process = ProcessInfo {
            pid: 10,
            path: "C:/Games/launcher.exe".to_owned(),
            args: Vec::new(),
        };

        let app = detect_app(&apps, &process).expect("launcher should be detected");
        assert_eq!(app.name, "Example");
    }

    #[test]
    fn detection_falls_back_to_loose_argument_matching() {
        let apps = vec![DetectableApp {
            id: "1".to_owned(),
            name: "Example".to_owned(),
            aliases: None,
            executables: Some(vec![DetectableExecutable {
                name: "game.exe".to_owned(),
                is_launcher: false,
                arguments: Some("--required".to_owned()),
            }]),
        }];

        let process = ProcessInfo {
            pid: 10,
            path: "C:/Games/game.exe".to_owned(),
            args: Vec::new(),
        };

        let app = detect_app(&apps, &process).expect("loose pass should detect");
        assert_eq!(app.name, "Example");
    }

    #[test]
    fn exact_executable_still_requires_arguments() {
        let apps = vec![DetectableApp {
            id: "1".to_owned(),
            name: "Example".to_owned(),
            aliases: None,
            executables: Some(vec![DetectableExecutable {
                name: ">game.exe".to_owned(),
                is_launcher: false,
                arguments: Some("--required".to_owned()),
            }]),
        }];

        let process = ProcessInfo {
            pid: 10,
            path: "C:/Games/game.exe".to_owned(),
            args: Vec::new(),
        };

        assert!(detect_app(&apps, &process).is_none());
    }

    #[test]
    fn steam_lookup_resolves_process_to_app_name_marker() {
        let lookup = SteamLookup {
            installs: vec![(
                "C:/Steam/steamapps/common/Game".to_owned(),
                "Game".to_owned(),
            )],
        };
        let process = ProcessInfo {
            pid: 42,
            path: "C:/Steam/steamapps/common/Game/bin/game.exe".to_owned(),
            args: Vec::new(),
        };

        let resolved = lookup.resolve(&process).unwrap();
        assert_eq!(
            resolved.path,
            "C:/Steam/steamapps/common/Game/Game.app_name"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn parses_windows_powershell_process_json() {
        let processes = super::parse_windows_processes(
            br#"[{"ProcessId":10,"ExecutablePath":"C:\\Games\\game.exe","CommandLine":"\"C:\\Games\\game.exe\" --rpc"},{"ProcessId":11,"ExecutablePath":null,"CommandLine":null}]"#,
        )
        .unwrap();

        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].pid, 10);
        assert_eq!(processes[0].path, "C:\\Games\\game.exe");
        assert!(processes[0].args.iter().any(|arg| arg == "--rpc"));
    }
}
