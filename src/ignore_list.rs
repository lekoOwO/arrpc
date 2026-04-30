use std::{collections::HashSet, path::Path};

use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub struct IgnoreList {
    entries: HashSet<String>,
}

impl IgnoreList {
    pub async fn from_file(path: &Path) -> anyhow::Result<Self> {
        let raw = tokio::fs::read_to_string(path).await?;
        let values = serde_json::from_str::<Vec<Value>>(&raw)?;
        let entries = values
            .into_iter()
            .filter_map(|value| value.as_str().map(Self::normalize))
            .collect();

        Ok(Self { entries })
    }

    pub fn should_ignore(&self, app_id: &str, executable: &str, game_name: &str) -> bool {
        if self.entries.is_empty() {
            return false;
        }

        let app_id = Self::normalize(app_id);
        if self.entries.contains(&app_id) {
            return true;
        }

        let executable = executable
            .rsplit(['/', '\\'])
            .next()
            .map(Self::normalize)
            .unwrap_or_default();
        if self.entries.contains(&executable) {
            return true;
        }

        let game_name = Self::normalize(game_name);
        self.entries
            .iter()
            .any(|entry| game_name.contains(entry) || entry.contains(&game_name))
    }

    fn normalize(value: &str) -> String {
        value.trim().to_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::IgnoreList;

    #[test]
    fn ignore_list_matches_id_executable_and_name() {
        let list = IgnoreList {
            entries: ["123", "game.exe", "example"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        };

        assert!(list.should_ignore("123", "other.exe", "Other"));
        assert!(list.should_ignore("456", "C:/Games/game.exe", "Other"));
        assert!(list.should_ignore("456", "other.exe", "Example Game"));
        assert!(!list.should_ignore("456", "other.exe", "Other"));
    }
}
