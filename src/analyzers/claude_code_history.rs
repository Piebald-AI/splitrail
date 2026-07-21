use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::types::ConversationMessage;

const HISTORY_FILE_NAME: &str = "claude-code-messages.sqlite3";

pub(crate) fn merge_session(
    live_messages: Vec<ConversationMessage>,
    conversation_hash: &str,
) -> Vec<ConversationMessage> {
    let path = match history_path() {
        Ok(path) => path,
        Err(error) => {
            warn_history_error("locate", None, &error);
            return live_messages;
        }
    };

    let mut messages = live_messages;
    if let Err(error) = merge_at(
        &path,
        &mut messages,
        &[conversation_hash.to_string()],
        false,
    ) {
        warn_history_error("update", Some(&path), &error);
    }
    messages
}

pub(crate) fn remove_session(conversation_hash: &str) -> Result<()> {
    let path = history_path()?;
    if !path.exists() {
        return Ok(());
    }
    let connection = Connection::open(&path).context("Failed to open Claude Code history store")?;
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .context("Failed to configure Claude Code history store")?;
    connection
        .execute(
            "DELETE FROM messages WHERE conversation_hash = ?1",
            [conversation_hash],
        )
        .context("Failed to remove deleted Claude Code session")?;
    Ok(())
}

pub(crate) fn merge_grouped(
    grouped: Vec<(PathBuf, Vec<ConversationMessage>)>,
    prune_missing: bool,
) -> Vec<(PathBuf, Vec<ConversationMessage>)> {
    let path = match history_path() {
        Ok(path) => path,
        Err(error) => {
            warn_history_error("locate", None, &error);
            return grouped;
        }
    };
    let conversation_hashes: Vec<_> = grouped
        .iter()
        .map(|(source_path, _)| crate::utils::hash_text(&source_path.to_string_lossy()))
        .collect();

    let mut grouped = grouped;
    if let Err(error) = merge_at(&path, &mut grouped, &conversation_hashes, prune_missing) {
        warn_history_error("update", Some(&path), &error);
    }
    grouped
}

fn history_path() -> Result<PathBuf> {
    let state_root = dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .context("Could not find platform state directory")?;
    Ok(state_root.join("splitrail").join(HISTORY_FILE_NAME))
}

fn merge_at<T>(
    path: &Path,
    grouped: &mut T,
    conversation_hashes: &[String],
    prune_missing: bool,
) -> Result<()>
where
    T: MessageGroups,
{
    let Some(parent) = path.parent() else {
        anyhow::bail!("Claude Code history path has no parent directory");
    };
    create_private_directory(parent)?;

    let mut connection =
        Connection::open(path).context("Failed to open Claude Code history store")?;
    set_private_file_permissions(path)?;
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .context("Failed to configure Claude Code history store")?;
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS messages (
                global_hash TEXT PRIMARY KEY NOT NULL,
                conversation_hash TEXT NOT NULL,
                payload BLOB NOT NULL
            );
            CREATE INDEX IF NOT EXISTS messages_conversation_hash
                ON messages(conversation_hash);",
        )
        .context("Failed to initialize Claude Code history store")?;

    let transaction = connection
        .transaction()
        .context("Failed to begin Claude Code history transaction")?;
    transaction
        .execute("DROP TABLE IF EXISTS current_conversations", [])
        .context("Failed to reset Claude Code history discovery set")?;
    transaction
        .execute(
            "CREATE TEMP TABLE current_conversations (
                conversation_hash TEXT PRIMARY KEY NOT NULL
            )",
            [],
        )
        .context("Failed to create Claude Code history discovery set")?;
    for conversation_hash in conversation_hashes {
        transaction
            .execute(
                "INSERT INTO current_conversations (conversation_hash) VALUES (?1)",
                [conversation_hash],
            )
            .context("Failed to record discovered Claude Code session")?;
    }
    if prune_missing && !conversation_hashes.is_empty() {
        transaction
            .execute(
                "DELETE FROM messages
                 WHERE conversation_hash NOT IN (SELECT conversation_hash FROM current_conversations)",
                [],
            )
            .context("Failed to prune deleted Claude Code sessions")?;
    }
    for messages in grouped.groups() {
        for message in messages {
            let mut stored = message.clone();
            stored.session_name = None;
            let payload = simd_json::to_vec(&stored)
                .context("Failed to serialize Claude Code history entry")?;
            transaction
                .execute(
                    "INSERT INTO messages (global_hash, conversation_hash, payload)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(global_hash) DO UPDATE SET
                         conversation_hash = excluded.conversation_hash,
                         payload = excluded.payload",
                    params![stored.global_hash, stored.conversation_hash, payload],
                )
                .context("Failed to persist Claude Code history entry")?;
        }
    }
    transaction
        .commit()
        .context("Failed to commit Claude Code history transaction")?;

    let live_hashes: HashSet<_> = grouped
        .groups()
        .flat_map(|messages| messages.iter().map(|message| message.global_hash.clone()))
        .collect();
    let mut retained_by_conversation: HashMap<String, Vec<ConversationMessage>> = HashMap::new();
    let mut statement = connection
        .prepare("SELECT payload FROM messages WHERE conversation_hash = ?1")
        .context("Failed to prepare Claude Code history query")?;
    for conversation_hash in conversation_hashes {
        let rows = statement
            .query_map([conversation_hash], |row| row.get::<_, Vec<u8>>(0))
            .context("Failed to query Claude Code history")?;
        for payload in rows {
            let mut payload = payload.context("Failed to read Claude Code history entry")?;
            match simd_json::from_slice::<ConversationMessage>(&mut payload) {
                Ok(message) if !live_hashes.contains(&message.global_hash) => {
                    retained_by_conversation
                        .entry(message.conversation_hash.clone())
                        .or_default()
                        .push(message);
                }
                Ok(_) => {}
                Err(error) => crate::utils::warn_once(format!(
                    "Skipping invalid Claude Code history entry: {error}"
                )),
            }
        }
    }

    grouped.extend_groups(&mut retained_by_conversation, conversation_hashes);
    Ok(())
}

trait MessageGroups {
    fn groups(&self) -> impl Iterator<Item = &[ConversationMessage]>;
    fn extend_groups(
        &mut self,
        retained: &mut HashMap<String, Vec<ConversationMessage>>,
        conversation_hashes: &[String],
    );
}

impl MessageGroups for Vec<(PathBuf, Vec<ConversationMessage>)> {
    fn groups(&self) -> impl Iterator<Item = &[ConversationMessage]> {
        self.iter().map(|(_, messages)| messages.as_slice())
    }

    fn extend_groups(
        &mut self,
        retained: &mut HashMap<String, Vec<ConversationMessage>>,
        conversation_hashes: &[String],
    ) {
        for ((_, messages), conversation_hash) in self.iter_mut().zip(conversation_hashes) {
            if let Some(mut history) = retained.remove(conversation_hash) {
                messages.append(&mut history);
            }
        }
    }
}

impl MessageGroups for Vec<ConversationMessage> {
    fn groups(&self) -> impl Iterator<Item = &[ConversationMessage]> {
        std::iter::once(self.as_slice())
    }

    fn extend_groups(
        &mut self,
        retained: &mut HashMap<String, Vec<ConversationMessage>>,
        conversation_hashes: &[String],
    ) {
        if let Some(conversation_hash) = conversation_hashes.first()
            && let Some(mut history) = retained.remove(conversation_hash)
        {
            self.append(&mut history);
        }
    }
}

fn create_private_directory(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).context("Failed to create Claude Code history directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .context("Failed to secure Claude Code history directory")?;
    }
    Ok(())
}

fn set_private_file_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .context("Failed to secure Claude Code history store")?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn warn_history_error(action: &str, path: Option<&Path>, error: &anyhow::Error) {
    let location = path
        .map(|path| format!(" {}", path.display()))
        .unwrap_or_default();
    crate::utils::warn_once(format!(
        "Could not {action} Claude Code history store{location}: {error}"
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Application, MessageRole, Stats};
    use chrono::{TimeZone, Utc};
    use rusqlite::OptionalExtension;
    use tempfile::tempdir;

    fn message(
        hash: &str,
        conversation: &str,
        local_hash: &str,
        input_tokens: u64,
    ) -> ConversationMessage {
        ConversationMessage {
            application: Application::ClaudeCode,
            date: Utc.with_ymd_and_hms(2025, 8, 2, 14, 5, 17).unwrap(),
            project_hash: "project".to_string(),
            conversation_hash: conversation.to_string(),
            local_hash: Some(local_hash.to_string()),
            global_hash: hash.to_string(),
            model: Some("claude-sonnet-4-20250514".to_string()),
            stats: Stats {
                input_tokens,
                ..Stats::default()
            },
            role: MessageRole::Assistant,
            uuid: Some(hash.to_string()),
            session_name: Some("Session prompt".to_string()),
        }
    }

    #[test]
    fn retains_messages_removed_from_rewritten_session() {
        let directory = tempdir().unwrap();
        let path = directory.path().join(HISTORY_FILE_NAME);
        let conversation = "session".to_string();
        let mut initial = vec![
            message("first", &conversation, "local-first", 10),
            message("second", &conversation, "local-second", 20),
        ];
        merge_at(
            &path,
            &mut initial,
            std::slice::from_ref(&conversation),
            false,
        )
        .unwrap();

        let mut after_rewrite = vec![message("second", &conversation, "local-second", 20)];
        merge_at(
            &path,
            &mut after_rewrite,
            std::slice::from_ref(&conversation),
            false,
        )
        .unwrap();
        assert_eq!(after_rewrite.len(), 2);
        assert!(
            after_rewrite
                .iter()
                .any(|message| message.global_hash == "first")
        );
    }

    #[test]
    fn live_message_updates_matching_history_record() {
        let directory = tempdir().unwrap();
        let path = directory.path().join(HISTORY_FILE_NAME);
        let conversation = "session".to_string();
        let mut initial = vec![message("same", &conversation, "local", 10)];
        merge_at(
            &path,
            &mut initial,
            std::slice::from_ref(&conversation),
            false,
        )
        .unwrap();

        let mut merged = vec![message("same", &conversation, "local", 25)];
        merge_at(
            &path,
            &mut merged,
            std::slice::from_ref(&conversation),
            false,
        )
        .unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].stats.input_tokens, 25);

        let connection = Connection::open(&path).unwrap();
        let mut payload: Vec<u8> = connection
            .query_row(
                "SELECT payload FROM messages WHERE global_hash = 'same'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap()
            .unwrap();
        let stored = simd_json::from_slice::<ConversationMessage>(&mut payload).unwrap();
        assert_eq!(stored.stats.input_tokens, 25);
        assert_eq!(stored.session_name, None);
    }

    #[test]
    fn empty_discovery_does_not_prune_retained_sessions() {
        let directory = tempdir().unwrap();
        let path = directory.path().join(HISTORY_FILE_NAME);
        let conversation = "session".to_string();
        let mut initial = vec![message("retained", &conversation, "local-retained", 10)];
        merge_at(
            &path,
            &mut initial,
            std::slice::from_ref(&conversation),
            false,
        )
        .unwrap();

        let mut empty: Vec<ConversationMessage> = Vec::new();
        merge_at(&path, &mut empty, &[], true).unwrap();

        let connection = Connection::open(&path).unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn only_restores_currently_discovered_sessions() {
        let directory = tempdir().unwrap();
        let path = directory.path().join(HISTORY_FILE_NAME);
        let first = "first-session".to_string();
        let deleted = "deleted-session".to_string();
        let mut initial = vec![
            (
                PathBuf::from("first.jsonl"),
                vec![message("first", &first, "local-first", 10)],
            ),
            (
                PathBuf::from("deleted.jsonl"),
                vec![message("deleted", &deleted, "local-deleted", 20)],
            ),
        ];
        merge_at(&path, &mut initial, &[first.clone(), deleted], true).unwrap();

        let mut current = vec![(PathBuf::from("first.jsonl"), Vec::new())];
        merge_at(&path, &mut current, std::slice::from_ref(&first), true).unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].1.len(), 1);
        assert_eq!(current[0].1[0].global_hash, "first");
    }

    #[test]
    fn removing_session_prevents_history_resurrection() {
        let directory = tempdir().unwrap();
        let path = directory.path().join(HISTORY_FILE_NAME);
        let conversation = "session".to_string();
        let mut initial = vec![message("old", &conversation, "local-old", 10)];
        merge_at(
            &path,
            &mut initial,
            std::slice::from_ref(&conversation),
            false,
        )
        .unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "DELETE FROM messages WHERE conversation_hash = ?1",
                [&conversation],
            )
            .unwrap();

        let mut recreated: Vec<ConversationMessage> = Vec::new();
        merge_at(
            &path,
            &mut recreated,
            std::slice::from_ref(&conversation),
            false,
        )
        .unwrap();
        assert!(recreated.is_empty());
    }

    #[test]
    fn corrupt_payload_does_not_hide_other_retained_messages() {
        let directory = tempdir().unwrap();
        let path = directory.path().join(HISTORY_FILE_NAME);
        let conversation = "session".to_string();
        let mut initial = vec![message("valid", &conversation, "local-valid", 10)];
        merge_at(
            &path,
            &mut initial,
            std::slice::from_ref(&conversation),
            false,
        )
        .unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "INSERT INTO messages (global_hash, conversation_hash, payload)
                 VALUES ('corrupt', ?1, X'FF')",
                [&conversation],
            )
            .unwrap();

        let mut restored: Vec<ConversationMessage> = Vec::new();
        merge_at(
            &path,
            &mut restored,
            std::slice::from_ref(&conversation),
            false,
        )
        .unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].global_hash, "valid");
    }

    #[test]
    fn raw_records_are_restored_before_local_hash_deduplication() {
        let directory = tempdir().unwrap();
        let path = directory.path().join(HISTORY_FILE_NAME);
        let conversation = "session".to_string();
        let mut initial = vec![
            message("uuid-a", &conversation, "shared-local", 10),
            message("uuid-b", &conversation, "shared-local", 20),
        ];
        merge_at(
            &path,
            &mut initial,
            std::slice::from_ref(&conversation),
            false,
        )
        .unwrap();

        let mut restored = vec![message("uuid-a", &conversation, "shared-local", 10)];
        merge_at(
            &path,
            &mut restored,
            std::slice::from_ref(&conversation),
            false,
        )
        .unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(
            restored
                .iter()
                .map(|message| message.stats.input_tokens)
                .sum::<u64>(),
            30
        );
    }
}
