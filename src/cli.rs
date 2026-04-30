use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use tokio_tungstenite::connect_async;

use crate::config::RuntimeConfig;

const DETECTABLE_URL: &str = "https://discord.com/api/v9/applications/detectable";
const FIXES_URL: &str =
    "https://gist.githubusercontent.com/Creationsss/2f25b7d76259b8fd2f23cf27cd538162/raw/detectable_fixes.json";
const VALID_PLATFORMS: &[&str] = &["win32", "linux", "darwin"];

#[derive(Debug, Deserialize)]
struct CliDetectableApp {
    id: String,
    name: String,
    executables: Option<Vec<CliDetectableExecutable>>,
}

#[derive(Debug, Deserialize)]
struct CliDetectableExecutable {
    os: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StateFileContent {
    timestamp: u64,
    activities: Vec<StateActivity>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StateActivity {
    socket_id: String,
    name: String,
    application_id: String,
    pid: u64,
    start_time: Option<u64>,
}

pub async fn list_database(config: &RuntimeConfig) -> anyhow::Result<()> {
    let db = load_detectable_db(config.data_dir.as_deref()).await?;
    println!("Total games in database: {}", db.len());

    let mut win32 = 0;
    let mut linux = 0;
    let mut darwin = 0;
    let mut multiplatform = 0;

    for app in &db {
        let Some(executables) = &app.executables else {
            continue;
        };
        let platforms = executables
            .iter()
            .filter_map(|exe| exe.os.as_deref())
            .collect::<std::collections::HashSet<_>>();
        if platforms.len() != 1 {
            multiplatform += 1;
        } else if platforms.contains("win32") {
            win32 += 1;
        } else if platforms.contains("linux") {
            linux += 1;
        } else if platforms.contains("darwin") {
            darwin += 1;
        }
    }

    println!("Games by platform:");
    println!("  Windows:        {win32}");
    println!("  Linux:          {linux}");
    println!("  macOS:          {darwin}");
    println!("  Multi-platform: {multiplatform}");
    println!();
    println!("Example games:");
    for (index, app) in db.iter().take(10).enumerate() {
        println!("  {}. {} ({})", index + 1, app.name, app.id);
    }

    Ok(())
}

pub async fn list_detected(config: &RuntimeConfig) -> anyhow::Result<()> {
    let Some(state) = read_recent_state_file().await? else {
        let detected = read_bridge_activities(config).await?;
        display_detected_games(&detected);
        return Ok(());
    };

    display_detected_games(&state.activities);
    Ok(())
}

pub async fn update_db(config: &RuntimeConfig) -> anyhow::Result<()> {
    let paths = database_paths(config);
    println!("Fetching detectable.json from Discord API...");

    let current = read_json_array(&paths.detectable).await.unwrap_or_default();
    let updated = reqwest::get(DETECTABLE_URL)
        .await?
        .error_for_status()?
        .json::<Vec<Value>>()
        .await?;
    write_json_pretty(&paths.detectable, &updated).await?;

    println!("Updated detectable.json");
    println!(
        "  {} -> {} games ({:+})",
        current.len(),
        updated.len(),
        updated.len() as isize - current.len() as isize
    );
    print_new_names(&current, &updated, "New games");

    println!();
    println!("Fetching detectable_fixes.json from upstream...");
    let current_fixes = read_json_array(&paths.fixes).await.unwrap_or_default();
    match reqwest::get(FIXES_URL).await {
        Ok(response) => match response.error_for_status() {
            Ok(response) => {
                let updated_fixes = response.json::<Vec<Value>>().await?;
                write_json_pretty(&paths.fixes, &updated_fixes).await?;
                println!("Updated detectable_fixes.json");
                println!(
                    "  {} -> {} entries ({:+})",
                    current_fixes.len(),
                    updated_fixes.len(),
                    updated_fixes.len() as isize - current_fixes.len() as isize
                );
                print_new_ids(&current_fixes, &updated_fixes, "New fixes");
            }
            Err(err) => {
                eprintln!("Failed to fetch detectable_fixes.json: {err}");
                println!("Keeping existing detectable_fixes.json");
            }
        },
        Err(err) => {
            eprintln!("Failed to fetch detectable_fixes.json: {err}");
            println!("Keeping existing detectable_fixes.json");
        }
    }

    println!();
    println!("Database update complete.");
    Ok(())
}

pub async fn validate_fixes(config: &RuntimeConfig) -> anyhow::Result<()> {
    println!("Validating detectable_fixes.json...");
    let paths = database_paths(config);
    let fixes = read_json_value(&paths.fixes).await.unwrap_or_else(|_| {
        serde_json::from_str(
            include_str!("../assets/detectable_fixes.json").trim_start_matches('\u{feff}'),
        )
        .unwrap_or(Value::Array(Vec::new()))
    });
    let db = read_json_array(&paths.detectable)
        .await
        .unwrap_or_else(|_| {
            serde_json::from_str(
                include_str!("../assets/detectable.json").trim_start_matches('\u{feff}'),
            )
            .unwrap_or_default()
        });
    let detectable_ids = db
        .iter()
        .filter_map(|entry| entry.get("id").and_then(Value::as_str))
        .collect::<HashSet<_>>();

    let mut errors = Vec::new();
    let Some(entries) = fixes.as_array() else {
        errors.push(ValidationError::new(
            "detectable_fixes.json",
            "Root must be an array",
        ));
        print_validation_errors(&errors);
        anyhow::bail!("detectable_fixes.json validation failed");
    };

    for (index, entry) in entries.iter().enumerate() {
        validate_fix_entry(index, entry, &detectable_ids, &mut errors);
    }

    if !errors.is_empty() {
        print_validation_errors(&errors);
        anyhow::bail!("detectable_fixes.json validation failed");
    }

    println!("detectable_fixes.json is valid.");
    Ok(())
}

fn display_detected_games(activities: &[StateActivity]) {
    println!("Currently detected games:");
    if activities.is_empty() {
        println!("  No games currently detected.");
        return;
    }

    for (index, activity) in activities.iter().enumerate() {
        println!("  {}. {}", index + 1, activity.name);
        println!("     App ID: {}", activity.application_id);
        println!("     PID: {}", activity.pid);
        println!("     Socket: {}", activity.socket_id);
        if let Some(start_time) = activity.start_time {
            println!("     Duration: {}", format_duration(start_time));
        }
        println!();
    }
}

async fn load_detectable_db(data_dir: Option<&Path>) -> anyhow::Result<Vec<CliDetectableApp>> {
    let raw = if let Some(data_dir) = data_dir {
        let path = data_dir.join("detectable.json");
        match tokio::fs::read_to_string(path).await {
            Ok(raw) => raw,
            Err(_) => include_str!("../assets/detectable.json").to_owned(),
        }
    } else {
        include_str!("../assets/detectable.json").to_owned()
    };

    Ok(serde_json::from_str(raw.trim_start_matches('\u{feff}'))?)
}

#[derive(Debug, Clone)]
struct DatabasePaths {
    detectable: PathBuf,
    fixes: PathBuf,
}

#[derive(Debug)]
struct ValidationError {
    path: String,
    message: String,
}

impl ValidationError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

fn database_paths(config: &RuntimeConfig) -> DatabasePaths {
    let base = config
        .data_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("assets"));
    DatabasePaths {
        detectable: base.join("detectable.json"),
        fixes: base.join("detectable_fixes.json"),
    }
}

async fn read_json_value(path: &Path) -> anyhow::Result<Value> {
    let raw = tokio::fs::read_to_string(path).await?;
    Ok(serde_json::from_str(raw.trim_start_matches('\u{feff}'))?)
}

async fn read_json_array(path: &Path) -> anyhow::Result<Vec<Value>> {
    let value = read_json_value(path).await?;
    Ok(value.as_array().cloned().unwrap_or_default())
}

async fn write_json_pretty(path: &Path, value: &[Value]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, serde_json::to_vec_pretty(value)?).await?;
    Ok(())
}

fn print_new_names(current: &[Value], updated: &[Value], label: &str) {
    let current_names = current
        .iter()
        .filter_map(|entry| entry.get("name").and_then(Value::as_str))
        .collect::<HashSet<_>>();
    let new_names = updated
        .iter()
        .filter_map(|entry| entry.get("name").and_then(Value::as_str))
        .filter(|name| !current_names.contains(name))
        .take(6)
        .collect::<Vec<_>>();

    if new_names.is_empty() {
        return;
    }

    let shown = new_names.iter().take(5).copied().collect::<Vec<_>>();
    println!("  {label}: {}", shown.join(", "));
    if new_names.len() > 5 {
        println!("  ... and more");
    }
}

fn print_new_ids(current: &[Value], updated: &[Value], label: &str) {
    let current_ids = current
        .iter()
        .filter_map(|entry| entry.get("id").and_then(Value::as_str))
        .collect::<HashSet<_>>();
    let new_ids = updated
        .iter()
        .filter_map(|entry| entry.get("id").and_then(Value::as_str))
        .filter(|id| !current_ids.contains(id))
        .collect::<Vec<_>>();

    if !new_ids.is_empty() {
        println!("  {label}: {}", new_ids.join(", "));
    }
}

fn validate_fix_entry(
    index: usize,
    entry: &Value,
    detectable_ids: &HashSet<&str>,
    errors: &mut Vec<ValidationError>,
) {
    let base = format!("[{index}]");
    let Some(object) = entry.as_object() else {
        errors.push(ValidationError::new(base, "Entry must be an object"));
        return;
    };

    let Some(id) = object.get("id") else {
        errors.push(ValidationError::new(&base, "Missing required field: id"));
        return;
    };
    let Some(id) = id.as_str() else {
        errors.push(ValidationError::new(
            format!("{base}.id"),
            "Must be a string",
        ));
        return;
    };

    if let Some(executables) = object.get("executables") {
        let Some(executables) = executables.as_array() else {
            errors.push(ValidationError::new(
                format!("{base}.executables"),
                "Must be an array",
            ));
            return;
        };

        for (exe_index, exe) in executables.iter().enumerate() {
            validate_fix_executable(&format!("{base}.executables[{exe_index}]"), exe, errors);
        }
    }

    validate_optional_string(&base, object, "name", errors);
    validate_optional_bool(&base, object, "hook", errors);
    validate_optional_bool(&base, object, "overlay", errors);
    validate_optional_bool(&base, object, "overlay_warn", errors);
    validate_optional_bool(&base, object, "overlay_compatibility_hook", errors);
    validate_optional_string(&base, object, "icon_hash", errors);

    if let Some(aliases) = object.get("aliases") {
        validate_string_array(&format!("{base}.aliases"), aliases, errors);
    }
    if let Some(themes) = object.get("themes") {
        validate_string_array(&format!("{base}.themes"), themes, errors);
    }
    if let Some(overlay_methods) = object.get("overlay_methods") {
        if !overlay_methods.is_null() && !overlay_methods.is_number() {
            errors.push(ValidationError::new(
                format!("{base}.overlay_methods"),
                "Must be a number or null",
            ));
        }
    }

    if errors.is_empty() {
        if detectable_ids.contains(id) {
            println!("Entry {index}: Patches existing game (ID: {id})");
        } else {
            println!("Entry {index}: Adds new game (ID: {id})");
            if !object.contains_key("name") {
                println!("Warning: New game without 'name' field will use \"Custom Game\"");
            }
        }
    }
}

fn validate_fix_executable(path: &str, exe: &Value, errors: &mut Vec<ValidationError>) {
    let Some(object) = exe.as_object() else {
        errors.push(ValidationError::new(path, "Must be an object"));
        return;
    };

    match object.get("name") {
        Some(value) if value.as_str().is_some_and(|name| !name.trim().is_empty()) => {}
        Some(value) if !value.is_string() => errors.push(ValidationError::new(
            format!("{path}.name"),
            "Must be a string",
        )),
        Some(_) => errors.push(ValidationError::new(
            format!("{path}.name"),
            "Cannot be empty",
        )),
        None => errors.push(ValidationError::new(
            format!("{path}.name"),
            "Missing required field: name",
        )),
    }

    validate_optional_bool(path, object, "is_launcher", errors);
    validate_optional_string(path, object, "arguments", errors);
    if let Some(os) = object.get("os") {
        match os.as_str() {
            Some(os) if VALID_PLATFORMS.contains(&os) => {}
            Some(_) => errors.push(ValidationError::new(
                format!("{path}.os"),
                format!("Must be one of: {}", VALID_PLATFORMS.join(", ")),
            )),
            None => errors.push(ValidationError::new(
                format!("{path}.os"),
                "Must be a string",
            )),
        }
    }
}

fn validate_optional_bool(
    base: &str,
    object: &serde_json::Map<String, Value>,
    key: &str,
    errors: &mut Vec<ValidationError>,
) {
    if object.get(key).is_some_and(|value| !value.is_boolean()) {
        errors.push(ValidationError::new(
            format!("{base}.{key}"),
            "Must be a boolean",
        ));
    }
}

fn validate_optional_string(
    base: &str,
    object: &serde_json::Map<String, Value>,
    key: &str,
    errors: &mut Vec<ValidationError>,
) {
    if object.get(key).is_some_and(|value| !value.is_string()) {
        errors.push(ValidationError::new(
            format!("{base}.{key}"),
            "Must be a string",
        ));
    }
}

fn validate_string_array(path: &str, value: &Value, errors: &mut Vec<ValidationError>) {
    let Some(values) = value.as_array() else {
        errors.push(ValidationError::new(path, "Must be an array"));
        return;
    };

    for (index, value) in values.iter().enumerate() {
        if !value.is_string() {
            errors.push(ValidationError::new(
                format!("{path}[{index}]"),
                "Must be a string",
            ));
        }
    }
}

fn print_validation_errors(errors: &[ValidationError]) {
    eprintln!("Validation failed with errors:");
    for error in errors {
        eprintln!("  {}: {}", error.path, error.message);
    }
}

async fn read_recent_state_file() -> anyhow::Result<Option<StateFileContent>> {
    for index in 0..=9 {
        let path = std::env::temp_dir().join(format!("arrpc-state-{index}"));
        let Ok(raw) = tokio::fs::read_to_string(path).await else {
            continue;
        };
        let Ok(state) = serde_json::from_str::<StateFileContent>(&raw) else {
            continue;
        };
        if now_ms().saturating_sub(state.timestamp) < 10_000 {
            return Ok(Some(state));
        }
    }

    Ok(None)
}

async fn read_bridge_activities(config: &RuntimeConfig) -> anyhow::Result<Vec<StateActivity>> {
    let mut last_error = None;
    for port in &config.bridge_ports {
        let url = format!("ws://{}:{port}", config.bridge_host);
        match tokio::time::timeout(Duration::from_secs(1), connect_async(&url)).await {
            Ok(Ok((mut ws, _))) => {
                let mut activities = std::collections::HashMap::new();
                let deadline = tokio::time::sleep(Duration::from_millis(500));
                tokio::pin!(deadline);

                loop {
                    tokio::select! {
                        _ = &mut deadline => break,
                        message = ws.next() => {
                            let Some(message) = message else { break; };
                            let message = message?;
                            if !message.is_text() {
                                continue;
                            }
                            let value = serde_json::from_str::<Value>(message.to_text()?)?;
                            if let Some(activity) = state_activity_from_bridge_message(&value) {
                                activities.insert(activity.socket_id.clone(), activity);
                            } else if let Some(socket_id) = value.get("socketId").and_then(Value::as_str) {
                                activities.remove(socket_id);
                            }
                        }
                    }
                }

                return Ok(activities.into_values().collect());
            }
            Ok(Err(err)) => last_error = Some(err.to_string()),
            Err(_) => last_error = Some("connection timed out".to_owned()),
        }
    }

    if let Some(err) = last_error {
        eprintln!("Could not connect to arrpc bridge: {err}");
    } else {
        eprintln!("Could not connect to arrpc bridge.");
    }
    Ok(Vec::new())
}

fn state_activity_from_bridge_message(value: &Value) -> Option<StateActivity> {
    let activity = value.get("activity")?;
    if activity.is_null() {
        return None;
    }
    Some(StateActivity {
        socket_id: value.get("socketId")?.as_str()?.to_owned(),
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
        pid: value.get("pid").and_then(Value::as_u64).unwrap_or_default(),
        start_time: activity
            .get("timestamps")
            .and_then(|timestamps| timestamps.get("start"))
            .and_then(Value::as_u64),
    })
}

fn format_duration(start_time: u64) -> String {
    let elapsed = Duration::from_millis(now_ms().saturating_sub(start_time));
    let hours = elapsed.as_secs() / 3600;
    let minutes = (elapsed.as_secs() % 3600) / 60;
    let seconds = elapsed.as_secs() % 60;

    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[allow(dead_code)]
fn active_payload_from_state(value: &Value) -> bool {
    value
        .get("activity")
        .is_some_and(|activity| !activity.is_null())
}

#[cfg(test)]
mod tests {
    use super::format_duration;
    use serde_json::json;

    #[test]
    fn duration_format_matches_cli_style() {
        let now = super::now_ms();
        assert_eq!(format_duration(now.saturating_sub(5_000)), "5s");
        assert_eq!(format_duration(now.saturating_sub(65_000)), "1m 5s");
    }

    #[test]
    fn bridge_message_maps_to_detected_activity() {
        let activity = super::state_activity_from_bridge_message(&json!({
            "socketId": "abc",
            "pid": 42,
            "activity": {
                "name": "Example",
                "application_id": "123",
                "timestamps": { "start": 1700000000000u64 }
            }
        }))
        .unwrap();

        assert_eq!(activity.socket_id, "abc");
        assert_eq!(activity.name, "Example");
        assert_eq!(activity.application_id, "123");
        assert_eq!(activity.pid, 42);
    }

    #[test]
    fn validate_fix_entry_rejects_bad_platform() {
        let mut errors = Vec::new();
        let ids = std::collections::HashSet::new();

        super::validate_fix_entry(
            0,
            &json!({
                "id": "custom",
                "executables": [
                    { "name": "game.exe", "os": "plan9" }
                ]
            }),
            &ids,
            &mut errors,
        );

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].path, "[0].executables[0].os");
    }

    #[test]
    fn validate_fix_entry_accepts_minimal_patch() {
        let mut errors = Vec::new();
        let ids = ["123"].into_iter().collect();

        super::validate_fix_entry(
            0,
            &json!({
                "id": "123",
                "executables": [
                    { "name": "game.exe", "os": "win32", "is_launcher": false }
                ]
            }),
            &ids,
            &mut errors,
        );

        assert!(errors.is_empty());
    }
}
