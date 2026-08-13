use crate::analyzer::{Analyzer, DataSource};
use crate::contribution_cache::ContributionStrategy;
use crate::models::calculate_total_cost_for_context_at;
use crate::types::{Application, ConversationMessage, MessageRole, Stats};
use crate::utils::hash_text;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use simd_json::prelude::*;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub struct DeepSeekHarnessAnalyzer;

impl DeepSeekHarnessAnalyzer {
    pub fn new() -> Self {
        Self
    }

    fn data_dir() -> Option<PathBuf> {
        data_dir_with_roots(
            std::env::var_os("DSH_HOME").as_deref(),
            dirs::home_dir().as_deref(),
        )
    }

    fn session_paths() -> impl Iterator<Item = PathBuf> {
        Self::data_dir()
            .filter(|dir| dir.is_dir())
            .into_iter()
            .flat_map(|dir| WalkDir::new(dir).min_depth(3).max_depth(3).into_iter())
            .filter_map(|entry| entry.ok())
            .map(walkdir::DirEntry::into_path)
            .filter(|path| is_dsh_session_path(path))
    }
}

fn data_dir_with_roots(dsh_home: Option<&OsStr>, home_dir: Option<&Path>) -> Option<PathBuf> {
    let configured_home = dsh_home
        .filter(|path| !path.to_string_lossy().trim().is_empty())
        .map(PathBuf::from);

    let dsh_home = match configured_home {
        Some(path) if path == Path::new("~") => home_dir?.to_path_buf(),
        Some(path) => {
            let text = path.to_string_lossy();
            if let Some(relative) = text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) {
                home_dir?.join(relative)
            } else {
                path
            }
        }
        None => home_dir?.join(".dsh"),
    };

    Some(dsh_home.join("sessions"))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DshEvent {
    #[serde(rename = "type", default)]
    event_type: String,
    #[serde(default)]
    seq: Option<u64>,
    #[serde(default)]
    time: Option<i64>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    parent_session: Option<String>,
    #[serde(default)]
    seed_length: Option<u64>,
    #[serde(default)]
    data: Option<DshEventData>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DshEventData {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    source: Option<DshSource>,
    #[serde(default)]
    content: Option<simd_json::OwnedValue>,
    #[serde(default)]
    message: Option<DshMessage>,
    #[serde(default)]
    usage: Option<DshUsage>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    turn: Option<u64>,
    #[serde(default)]
    step: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct DshSource {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct DshMessage {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    source: Option<DshSource>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DshUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_tokens: u64,
    #[serde(default)]
    cache_write_tokens: u64,
    #[serde(default)]
    reasoning_tokens: u64,
}

fn is_dsh_session_path(path: &Path) -> bool {
    path.is_file()
        && path
            .file_name()
            .is_some_and(|name| name == "session.jsonl.zstd")
        && path
            .parent()
            .and_then(Path::parent)
            .is_some_and(|workspace_dir| workspace_dir.parent().is_some())
}

fn truncate_session_name(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    let truncated: String = text.chars().take(50).collect();
    Some(if truncated.chars().count() < text.chars().count() {
        format!("{truncated}...")
    } else {
        truncated
    })
}

fn text_from_content(content: &simd_json::OwnedValue) -> Option<String> {
    match content {
        simd_json::OwnedValue::String(text) => Some(text.to_string()),
        simd_json::OwnedValue::Array(items) => items.iter().find_map(|item| {
            item.get("text")
                .and_then(|text| text.as_str())
                .map(ToOwned::to_owned)
        }),
        simd_json::OwnedValue::Object(object) => object
            .get("text")
            .and_then(|text| text.as_str())
            .map(ToOwned::to_owned),
        _ => None,
    }
}

fn add_tool_stats(stats: &mut Stats, tool_name: &str) {
    stats.tool_calls += 1;
    match tool_name {
        "read" | "read_file" => stats.files_read += 1,
        "edit" | "edit_file" | "apply_patch" => stats.files_edited += 1,
        "write" | "write_file" => stats.files_added += 1,
        "bash" | "shell" | "run_terminal_command" => stats.terminal_commands += 1,
        "glob" | "find" => stats.file_searches += 1,
        "grep" | "rg" | "search" => stats.file_content_searches += 1,
        _ => {}
    }
}

fn event_date(timestamp_ms: Option<i64>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    timestamp_ms
        .and_then(DateTime::from_timestamp_millis)
        .unwrap_or(fallback)
}

fn parse_session_reader<R: Read>(path: &Path, reader: R) -> Result<Vec<ConversationMessage>> {
    let file_path = path.to_string_lossy();
    let fallback_session_id = path
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_path.into_owned());
    let fallback_project = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut session_id = fallback_session_id;
    let mut project_id = fallback_project;
    let mut parent_session = None;
    let mut seed_length = None;
    let mut session_name = None;
    let mut fallback_session_name = None;
    let mut messages = Vec::new();
    let mut assistant_by_step = HashMap::new();
    let file_modified_at = path
        .metadata()
        .and_then(|metadata| metadata.modified())
        .map(DateTime::<Utc>::from)
        .unwrap_or(DateTime::UNIX_EPOCH);
    let mut last_event_date = None;

    for (line_index, line) in BufReader::new(reader).split(b'\n').enumerate() {
        let line = match line {
            Ok(line) => line,
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                crate::utils::warn_once(format!(
                    "WARNING: DeepSeek Harness session `{}` has a truncated tail; preserving {} parsed messages",
                    path.display(),
                    messages.len()
                ));
                break;
            }
            Err(error) => return Err(error.into()),
        };
        if line.iter().all(|byte| byte.is_ascii_whitespace()) {
            continue;
        }

        let mut bytes = line;
        let event = match simd_json::from_slice::<DshEvent>(&mut bytes) {
            Ok(event) => event,
            Err(error) => {
                crate::utils::warn_once(format!(
                    "WARNING: Failed to parse DeepSeek Harness session `{}` line {}: {error}",
                    path.display(),
                    line_index + 1
                ));
                continue;
            }
        };
        let date = event_date(event.time, last_event_date.unwrap_or(file_modified_at));
        if event
            .time
            .and_then(DateTime::from_timestamp_millis)
            .is_some()
        {
            last_event_date = Some(date);
        }

        if event.event_type == "session" {
            if let Some(id) = event.id {
                session_id = id;
            }
            if let Some(cwd) = event.cwd {
                project_id = cwd;
            }
            parent_session = event.parent_session;
            seed_length = event.seed_length;
            continue;
        }

        let Some(data) = event.data else {
            continue;
        };

        match event.event_type.as_str() {
            "session/title" => {
                if let Some(title) = data.title.and_then(|title| truncate_session_name(&title)) {
                    session_name = Some(title);
                }
            }
            "user/message"
                if data
                    .source
                    .as_ref()
                    .is_some_and(|source| source.kind == "user") =>
            {
                if fallback_session_name.is_none() {
                    fallback_session_name = data
                        .content
                        .as_ref()
                        .and_then(text_from_content)
                        .and_then(|text| truncate_session_name(&text));
                }

                let canonical_session = if event
                    .seq
                    .zip(seed_length)
                    .is_some_and(|(seq, seed)| seq < seed)
                {
                    parent_session.as_deref().unwrap_or(&session_id)
                } else {
                    &session_id
                };
                let message_id = data
                    .id
                    .unwrap_or_else(|| format!("{canonical_session}:user:{line_index}"));
                messages.push(ConversationMessage {
                    application: Application::DeepSeekHarness,
                    date,
                    project_hash: hash_text(&project_id),
                    conversation_hash: hash_text(canonical_session),
                    local_hash: Some(message_id.clone()),
                    global_hash: hash_text(&format!("deepseek-harness:{message_id}")),
                    model: None,
                    stats: Stats::default(),
                    role: MessageRole::User,
                    uuid: Some(message_id),
                    session_name: None,
                });
            }
            "assistant/message" => {
                let Some(message) = data.message else {
                    continue;
                };
                let canonical_session = if event
                    .seq
                    .zip(seed_length)
                    .is_some_and(|(seq, seed)| seq < seed)
                {
                    parent_session.as_deref().unwrap_or(&session_id)
                } else {
                    &session_id
                };
                let message_id = message
                    .id
                    .unwrap_or_else(|| format!("{canonical_session}:assistant:{line_index}"));
                let model = message.source.and_then(|source| source.model);
                let usage = data.usage.unwrap_or_default();
                let context_tokens = usage
                    .input_tokens
                    .saturating_add(usage.cache_read_tokens)
                    .saturating_add(usage.cache_write_tokens);
                let cost = model.as_deref().map_or(0.0, |model| {
                    calculate_total_cost_for_context_at(
                        model,
                        usage.input_tokens,
                        usage.output_tokens,
                        usage.cache_write_tokens,
                        usage.cache_read_tokens,
                        context_tokens,
                        Some(date),
                    )
                });

                let message_index = messages.len();
                if let Some(key) = data.turn.zip(data.step) {
                    assistant_by_step.insert(key, message_index);
                }
                messages.push(ConversationMessage {
                    application: Application::DeepSeekHarness,
                    date,
                    project_hash: hash_text(&project_id),
                    conversation_hash: hash_text(canonical_session),
                    local_hash: Some(message_id.clone()),
                    global_hash: hash_text(&format!("deepseek-harness:{message_id}")),
                    model,
                    stats: Stats {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        reasoning_tokens: usage.reasoning_tokens,
                        cache_creation_tokens: usage.cache_write_tokens,
                        cache_read_tokens: usage.cache_read_tokens,
                        cached_tokens: usage
                            .cache_read_tokens
                            .saturating_add(usage.cache_write_tokens),
                        cost,
                        ..Default::default()
                    },
                    role: MessageRole::Assistant,
                    uuid: Some(message_id),
                    session_name: None,
                });
            }
            "tool/call" => {
                if let Some(message_index) = data
                    .turn
                    .zip(data.step)
                    .and_then(|key| assistant_by_step.get(&key).copied())
                    && let Some(tool_name) = data.name
                {
                    add_tool_stats(&mut messages[message_index].stats, &tool_name);
                }
            }
            _ => {}
        }
    }

    let session_name = session_name.or(fallback_session_name);
    for message in &mut messages {
        message.session_name.clone_from(&session_name);
    }

    Ok(messages)
}

pub fn parse_deepseek_harness_file(path: &Path) -> Result<Vec<ConversationMessage>> {
    let decoder = zstd::stream::read::Decoder::new(File::open(path)?)?;
    parse_session_reader(path, decoder)
}

#[async_trait]
impl Analyzer for DeepSeekHarnessAnalyzer {
    fn display_name(&self) -> &'static str {
        "DeepSeek Harness"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        Self::data_dir()
            .map(|dir| format!("{}/*/*/session.jsonl.zstd", dir.to_string_lossy()))
            .into_iter()
            .collect()
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let sources = Self::session_paths()
            .map(|path| DataSource { path })
            .collect();
        Ok(sources)
    }

    fn is_available(&self) -> bool {
        Self::session_paths().next().is_some()
    }

    fn parse_source(&self, source: &DataSource) -> Result<Vec<ConversationMessage>> {
        parse_deepseek_harness_file(&source.path)
    }

    fn get_watch_directories(&self) -> Vec<PathBuf> {
        Self::data_dir()
            .filter(|dir| dir.is_dir())
            .into_iter()
            .collect()
    }

    fn is_valid_data_path(&self, path: &Path) -> bool {
        is_dsh_session_path(path)
    }

    fn contribution_strategy(&self) -> ContributionStrategy {
        ContributionStrategy::SingleSession
    }

    fn requires_full_reload_for_source_change(&self) -> bool {
        // Forked DSH sessions contain a seeded copy of the parent history.
        // Reload all sources so UUID-based deduplication remains correct.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn resolves_default_and_configured_data_dirs() {
        let home = Path::new("/home/tester");

        assert_eq!(
            data_dir_with_roots(None, Some(home)),
            Some(home.join(".dsh/sessions"))
        );
        assert_eq!(
            data_dir_with_roots(Some(OsStr::new("/custom/dsh")), Some(home)),
            Some(PathBuf::from("/custom/dsh/sessions"))
        );
        assert_eq!(
            data_dir_with_roots(Some(OsStr::new("~/custom-dsh")), Some(home)),
            Some(home.join("custom-dsh/sessions"))
        );
        assert_eq!(data_dir_with_roots(Some(OsStr::new("  ")), None), None);
    }

    #[test]
    fn parses_usage_tools_and_only_direct_user_messages() {
        let input = concat!(
            r#"{"type":"session","id":"session-1","createdAt":1786629150000,"cwd":"/tmp/project"}"#,
            "\n",
            r#"{"type":"user/message","seq":1,"time":1786629151000,"data":{"id":"instructions","source":{"kind":"agent-instructions"},"content":"ignore"}}"#,
            "\n",
            r#"{"type":"user/message","seq":2,"time":1786629152000,"data":{"id":"user-1","source":{"kind":"user"},"content":[{"type":"text","text":"Implement DSH support"}]}}"#,
            "\n",
            r#"{"type":"assistant/message","seq":3,"time":1786629153000,"data":{"turn":1,"step":1,"message":{"id":"assistant-1","source":{"kind":"model","model":"deepseek-v4-flash"}},"usage":{"inputTokens":100,"outputTokens":20,"cacheReadTokens":30,"cacheWriteTokens":4,"reasoningTokens":5}}}"#,
            "\n",
            r#"{"type":"tool/call","seq":4,"time":1786629153001,"data":{"turn":1,"step":1,"name":"read"}}"#,
            "\n",
            r#"{"type":"tool/call","seq":5,"time":1786629153002,"data":{"turn":1,"step":1,"name":"bash"}}"#,
            "\n",
            r#"{"type":"session/title","seq":6,"time":1786629154000,"data":{"title":"DeepSeek Harness title"}}"#,
            "\n",
        );

        let messages = parse_session_reader(
            Path::new("/tmp/workspace/session-1/session.jsonl.zstd"),
            Cursor::new(input),
        )
        .expect("session should parse");

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, MessageRole::User);
        assert_eq!(messages[1].role, MessageRole::Assistant);
        assert_eq!(messages[1].model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(messages[1].stats.input_tokens, 100);
        assert_eq!(messages[1].stats.output_tokens, 20);
        assert_eq!(messages[1].stats.cache_read_tokens, 30);
        assert_eq!(messages[1].stats.cache_creation_tokens, 4);
        assert_eq!(messages[1].stats.reasoning_tokens, 5);
        assert_eq!(messages[1].stats.tool_calls, 2);
        assert_eq!(messages[1].stats.files_read, 1);
        assert_eq!(messages[1].stats.terminal_commands, 1);
        assert_eq!(
            messages[1].session_name.as_deref(),
            Some("DeepSeek Harness title")
        );
    }

    #[test]
    fn seeded_fork_messages_keep_the_parent_conversation() {
        let input = concat!(
            r#"{"type":"session","id":"child","cwd":"/tmp/project","parentSession":"parent","seedLength":10}"#,
            "\n",
            r#"{"type":"assistant/message","seq":9,"time":1786629153000,"data":{"turn":1,"step":1,"message":{"id":"seeded","source":{"kind":"model","model":"deepseek-v4-flash"}},"usage":{}}}"#,
            "\n",
            r#"{"type":"assistant/message","seq":10,"time":1786629154000,"data":{"turn":2,"step":1,"message":{"id":"new","source":{"kind":"model","model":"deepseek-v4-flash"}},"usage":{}}}"#,
            "\n",
        );

        let messages = parse_session_reader(
            Path::new("/tmp/workspace/child/session.jsonl.zstd"),
            Cursor::new(input),
        )
        .expect("session should parse");

        assert_eq!(messages[0].conversation_hash, hash_text("parent"));
        assert_eq!(messages[1].conversation_hash, hash_text("child"));
    }

    #[test]
    fn missing_event_time_uses_the_last_known_timestamp() {
        let input = concat!(
            r#"{"type":"user/message","time":1786629152000,"data":{"id":"user-1","source":{"kind":"user"},"content":"hello"}}"#,
            "\n",
            r#"{"type":"assistant/message","data":{"message":{"id":"assistant-1","source":{"kind":"model","model":"deepseek-v4-flash"}},"usage":{}}}"#,
            "\n",
        );

        let messages = parse_session_reader(
            Path::new("/tmp/workspace/session-1/session.jsonl.zstd"),
            Cursor::new(input),
        )
        .expect("session should parse");

        assert_eq!(messages[0].date, messages[1].date);
        assert_eq!(messages[0].date.timestamp_millis(), 1_786_629_152_000);
    }

    #[test]
    fn missing_event_time_uses_stable_file_modification_time() {
        let file = tempfile::NamedTempFile::new().expect("temporary file should be created");
        let modified_at = file
            .as_file()
            .metadata()
            .and_then(|metadata| metadata.modified())
            .map(DateTime::<Utc>::from)
            .expect("temporary file should have a modification time");
        let input = concat!(
            r#"{"type":"user/message","data":{"id":"user-1","source":{"kind":"user"},"content":"hello"}}"#,
            "\n",
        );

        let first = parse_session_reader(file.path(), Cursor::new(input))
            .expect("first parse should succeed");
        let second = parse_session_reader(file.path(), Cursor::new(input))
            .expect("second parse should succeed");

        assert_eq!(first[0].date, modified_at);
        assert_eq!(second[0].date, modified_at);
    }

    #[test]
    fn truncated_final_zstd_frame_preserves_complete_messages() {
        let first_event = concat!(
            r#"{"type":"user/message","time":1786629152000,"data":{"id":"user-1","source":{"kind":"user"},"content":"hello"}}"#,
            "\n",
        );
        let second_event = concat!(
            r#"{"type":"assistant/message","time":1786629153000,"data":{"message":{"id":"assistant-1","source":{"kind":"model","model":"deepseek-v4-flash"}},"usage":{}}}"#,
            "\n",
        );
        let mut compressed = zstd::stream::encode_all(Cursor::new(first_event), 1)
            .expect("first frame should encode");
        let mut truncated = zstd::stream::encode_all(Cursor::new(second_event), 1)
            .expect("second frame should encode");
        truncated.truncate(truncated.len() - 2);
        compressed.extend(truncated);

        let decoder = zstd::stream::read::Decoder::new(Cursor::new(compressed))
            .expect("decoder should initialize");
        let messages = parse_session_reader(
            Path::new("/tmp/workspace/session-1/session.jsonl.zstd"),
            decoder,
        )
        .expect("complete messages should survive a truncated tail");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].uuid.as_deref(), Some("user-1"));
    }

    #[test]
    fn invalid_zstd_tail_still_returns_an_error() {
        let first_event = concat!(
            r#"{"type":"user/message","time":1786629152000,"data":{"id":"user-1","source":{"kind":"user"},"content":"hello"}}"#,
            "\n",
        );
        let mut compressed =
            zstd::stream::encode_all(Cursor::new(first_event), 1).expect("frame should encode");
        compressed.extend([1, 2, 3, 4, 5]);

        let decoder = zstd::stream::read::Decoder::new(Cursor::new(compressed))
            .expect("decoder should initialize");
        let result = parse_session_reader(
            Path::new("/tmp/workspace/session-1/session.jsonl.zstd"),
            decoder,
        );

        assert!(result.is_err());
    }
}
