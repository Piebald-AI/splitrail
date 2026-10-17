use std::io::Write;
use tempfile::NamedTempFile;

use crate::analyzer::Analyzer;
use crate::analyzers::codex_cli::*;
use crate::models::calculate_total_cost;

const SESSION_FILE_NAME: &str =
    "rollout-2026-07-22T12-34-56-243232f1-a7ab-44e6-b2c3-045b673746ea.jsonl";

fn write_test_session(path: &std::path::Path) {
    std::fs::write(path, test_session_content()).unwrap();
}

fn write_compressed_test_session(path: &std::path::Path) {
    let compressed = zstd::stream::encode_all(test_session_content().as_bytes(), 1).unwrap();
    std::fs::write(path, compressed).unwrap();
}

fn test_session_content() -> String {
    concat!(
        r#"{"timestamp":"2026-07-22T12:34:56.000Z","type":"session_meta","payload":{"id":"243232f1-a7ab-44e6-b2c3-045b673746ea","timestamp":"2026-07-22T12:34:56.000Z"}}"#,
        "\n",
        r#"{"timestamp":"2026-07-22T12:35:00.000Z","type":"turn_context","payload":{"model":"gpt-5-codex"}}"#,
        "\n",
        r#"{"timestamp":"2026-07-22T12:35:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Test archived sessions"}]}}"#,
        "\n",
        r#"{"timestamp":"2026-07-22T12:35:01.500Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Testing hash stability"}]}}"#,
        "\n",
        r#"{"timestamp":"2026-07-22T12:35:02.000Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":120,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5,"total_tokens":150}}}}"#,
        "\n"
    )
    .to_string()
}

#[test]
fn test_codex_data_dirs_include_active_and_archived_sessions() {
    let home = std::path::PathBuf::from("/home/test");

    assert_eq!(
        data_dirs_with_roots(None, Some(&home)),
        vec![
            home.join(".codex/sessions"),
            home.join(".codex/archived_sessions")
        ]
    );
}

#[test]
fn test_codex_data_dirs_respect_codex_home() {
    let home = std::path::Path::new("/home/test");
    let custom = std::path::Path::new("/custom/codex");

    assert_eq!(
        data_dirs_with_roots(Some(custom.as_os_str()), Some(home)),
        vec![custom.join("sessions"), custom.join("archived_sessions")]
    );
    assert_eq!(
        data_dirs_with_roots(Some(std::ffi::OsStr::new("")), Some(home)),
        vec![
            home.join(".codex/sessions"),
            home.join(".codex/archived_sessions")
        ]
    );
}

#[test]
fn test_archived_session_uses_original_active_path_as_identity() {
    let home = std::path::PathBuf::from("/home/test");
    let archived = home
        .join(".codex/archived_sessions")
        .join(SESSION_FILE_NAME);

    assert_eq!(
        canonical_session_path(&archived),
        home.join(".codex/sessions/2026/07/22")
            .join(SESSION_FILE_NAME)
    );
}

#[test]
fn test_active_and_archived_copies_are_discovered_once() {
    let temp_dir = tempfile::tempdir().unwrap();
    let active_dir = temp_dir.path().join(".codex/sessions/2026/07/22");
    let archived_dir = temp_dir.path().join(".codex/archived_sessions");
    std::fs::create_dir_all(&active_dir).unwrap();
    std::fs::create_dir_all(&archived_dir).unwrap();
    write_test_session(&active_dir.join(SESSION_FILE_NAME));
    write_test_session(&archived_dir.join(SESSION_FILE_NAME));

    let sources =
        discover_data_sources_in(vec![temp_dir.path().join(".codex/sessions"), archived_dir]);

    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].path, active_dir.join(SESSION_FILE_NAME));
}

#[test]
fn test_valid_codex_data_paths_must_be_inside_watched_directories() {
    let temp_dir = tempfile::tempdir().unwrap();
    let sessions_dir = temp_dir.path().join(".codex/sessions");
    let unrelated_dir = temp_dir.path().join("unrelated");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    std::fs::create_dir_all(&unrelated_dir).unwrap();
    let session_path = sessions_dir.join("session.jsonl");
    let unrelated_path = unrelated_dir.join("session.jsonl");
    std::fs::write(&session_path, "").unwrap();
    std::fs::write(&unrelated_path, "").unwrap();

    let data_dirs = vec![sessions_dir];
    assert!(is_valid_data_path_in(&session_path, &data_dirs));
    assert!(!is_valid_data_path_in(&unrelated_path, &data_dirs));

    let compressed_path = data_dirs[0].join("session.jsonl.zst");
    std::fs::write(&compressed_path, "").unwrap();
    assert!(is_valid_data_path_in(&compressed_path, &data_dirs));
    assert!(!is_valid_data_path_in(
        &data_dirs[0].join("session.jsonl.gz"),
        &data_dirs
    ));
}

#[test]
fn test_compressed_session_is_discovered_and_deduped_with_plain_copy() {
    let temp_dir = tempfile::tempdir().unwrap();
    let active_dir = temp_dir.path().join(".codex/sessions/2026/07/22");
    std::fs::create_dir_all(&active_dir).unwrap();
    let plain_path = active_dir.join(SESSION_FILE_NAME);
    let compressed_path = active_dir.join(format!("{SESSION_FILE_NAME}.zst"));
    write_test_session(&plain_path);
    write_compressed_test_session(&compressed_path);
    std::fs::write(active_dir.join("session.jsonl.gz"), "").unwrap();

    let sources = discover_data_sources_in(vec![temp_dir.path().join(".codex/sessions")]);
    assert_eq!(sources.len(), 1);
    assert!(sources[0].path == plain_path || sources[0].path == compressed_path);
}

#[test]
fn test_compressed_session_parses_like_plain() {
    let temp_dir = tempfile::tempdir().unwrap();
    let home = temp_dir.path();
    let active_dir = home.join(".codex/sessions/2026/07/22");
    let archived_dir = home.join(".codex/archived_sessions");
    std::fs::create_dir_all(&active_dir).unwrap();
    std::fs::create_dir_all(&archived_dir).unwrap();
    let plain_path = active_dir.join(SESSION_FILE_NAME);
    let compressed_path = archived_dir.join(format!("{SESSION_FILE_NAME}.zst"));
    write_test_session(&plain_path);
    write_compressed_test_session(&compressed_path);

    // Both files describe the same session, so the compressed copy must produce
    // the same identities and token stats as the plain one.
    assert_eq!(
        canonical_session_path(&compressed_path),
        canonical_session_path(&plain_path)
    );
    let (plain_messages, plain_model) = parse_codex_cli_jsonl_file(&plain_path).unwrap();
    let (compressed_messages, compressed_model) =
        parse_codex_cli_jsonl_file(&compressed_path).unwrap();

    assert_eq!(plain_model, compressed_model);
    assert_eq!(plain_messages.len(), compressed_messages.len());
    for (plain, compressed) in plain_messages.iter().zip(&compressed_messages) {
        assert_eq!(plain.role, compressed.role);
        assert_eq!(plain.conversation_hash, compressed.conversation_hash);
        assert_eq!(plain.global_hash, compressed.global_hash);
        assert_eq!(plain.stats.input_tokens, compressed.stats.input_tokens);
        assert_eq!(plain.stats.output_tokens, compressed.stats.output_tokens);
        assert_eq!(plain.stats.cached_tokens, compressed.stats.cached_tokens);
    }
}

#[test]
fn test_compressed_archived_session_matches_uncompressed_identity() {
    let temp_dir = tempfile::tempdir().unwrap();
    let home = temp_dir.path();
    let archived_dir = home.join(".codex/archived_sessions");
    std::fs::create_dir_all(&archived_dir).unwrap();
    let archived_path = archived_dir.join(format!("{SESSION_FILE_NAME}.zst"));
    write_compressed_test_session(&archived_path);

    assert_eq!(
        canonical_session_path(&archived_path),
        home.join(".codex/sessions/2026/07/22")
            .join(SESSION_FILE_NAME)
    );

    let (messages, _) = parse_codex_cli_jsonl_file(&archived_path).unwrap();
    assert!(!messages.is_empty());
    let expected_hash =
        crate::utils::hash_text(&canonical_session_path(&archived_path).to_string_lossy());
    assert_eq!(messages[0].conversation_hash, expected_hash);
}

#[test]
fn test_truncated_compressed_session_keeps_complete_messages() {
    let temp_dir = tempfile::tempdir().unwrap();
    let path = temp_dir.path().join(format!("{SESSION_FILE_NAME}.zst"));
    let head = concat!(
        r#"{"timestamp":"2026-07-22T12:34:56.000Z","type":"session_meta","payload":{"id":"243232f1-a7ab-44e6-b2c3-045b673746ea","timestamp":"2026-07-22T12:34:56.000Z"}}"#,
        "\n",
        r#"{"timestamp":"2026-07-22T12:35:00.000Z","type":"turn_context","payload":{"model":"gpt-5-codex"}}"#,
        "\n",
        r#"{"timestamp":"2026-07-22T12:35:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Test truncated frames"}]}}"#,
        "\n",
    );
    let tail = concat!(
        r#"{"timestamp":"2026-07-22T12:35:02.000Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":120,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5,"total_tokens":150}}}}"#,
        "\n",
    );

    let mut compressed = zstd::stream::encode_all(head.as_bytes(), 1).unwrap();
    let mut truncated = zstd::stream::encode_all(tail.as_bytes(), 1).unwrap();
    truncated.truncate(truncated.len() - 2);
    compressed.extend(truncated);
    std::fs::write(&path, compressed).unwrap();

    let (messages, _) = parse_codex_cli_jsonl_file(&path).unwrap();
    assert_eq!(messages.len(), 1);
    assert!(matches!(messages[0].role, crate::types::MessageRole::User));
}

#[test]
fn test_message_hashes_remain_stable_after_archiving() {
    let temp_dir = tempfile::tempdir().unwrap();
    // Built one component at a time: a single join with embedded forward slashes
    // keeps them in the string on Windows, while canonical_session_path rebuilds
    // the archived path with native separators. conversation_hash hashes the
    // string form, so the two sides would hash different strings.
    let active_dir = temp_dir
        .path()
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("07")
        .join("22");
    let archived_dir = temp_dir.path().join(".codex").join("archived_sessions");
    std::fs::create_dir_all(&active_dir).unwrap();
    std::fs::create_dir_all(&archived_dir).unwrap();
    let active_path = active_dir.join(SESSION_FILE_NAME);
    let archived_path = archived_dir.join(SESSION_FILE_NAME);
    write_test_session(&active_path);
    write_test_session(&archived_path);

    let (active_messages, _) = parse_codex_cli_jsonl_file(&active_path).unwrap();
    let (archived_messages, _) = parse_codex_cli_jsonl_file(&archived_path).unwrap();

    assert_eq!(active_messages.len(), archived_messages.len());
    assert_eq!(
        active_messages
            .iter()
            .filter(|message| matches!(message.role, crate::types::MessageRole::Assistant))
            .count(),
        2
    );
    for (active, archived) in active_messages.iter().zip(&archived_messages) {
        assert_eq!(active.conversation_hash, archived.conversation_hash);
        assert_eq!(active.global_hash, archived.global_hash);
    }
}

#[test]
fn test_parse_codex_cli_new_wrapper_format() {
    // Create a temporary file with the new wrapper format
    let mut temp_file = NamedTempFile::new().unwrap();

    // Session metadata
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:27.465Z","type":"session_meta","payload":{{"id":"243232f1-a7ab-44e6-b2c3-045b673746ea","timestamp":"2025-09-18T00:16:27.461Z","cwd":"/home/test","originator":"codex_cli_rs","cli_version":"0.38.0","instructions":null,"git":{{"commit_hash":"e4b91cc29da68a6fee1edaf44ce50a64bfbdce63","branch":"main","repository_url":"https://github.com/test/repo.git"}}}}}}"#
    ).unwrap();

    // Turn context with model info
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:36.676Z","type":"turn_context","payload":{{"cwd":"/home/test","approval_policy":"on-request","model":"gpt-5-codex","summary":"auto"}}}}"#
    ).unwrap();

    // User message
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:36.675Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"Hey"}}]}}}}"#
    ).unwrap();

    // Token count event
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:38.851Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":2629,"cached_input_tokens":2560,"output_tokens":14,"reasoning_output_tokens":0,"total_tokens":2643}},"last_token_usage":{{"input_tokens":2629,"cached_input_tokens":2560,"output_tokens":14,"reasoning_output_tokens":0,"total_tokens":2643}},"model_context_window":272000}}}}}}"#
    ).unwrap();

    // Assistant message
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:38.852Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Hey! How can I help today?"}}]}}}}"#
    ).unwrap();

    let (messages, _model) = parse_codex_cli_jsonl_file(temp_file.path()).unwrap();

    // Should have parsed user and assistant messages
    assert!(messages.len() >= 2);

    // Session name should prefer the first user message over generic summaries like "auto"
    assert!(
        messages
            .iter()
            .any(|msg| msg.session_name.as_deref() == Some("Hey"))
    );
    // Find the assistant message
    let assistant_msg = messages
        .iter()
        .find(|msg| matches!(msg.role, crate::types::MessageRole::Assistant))
        .unwrap();

    assert_eq!(assistant_msg.model, Some("gpt-5-codex".to_string()));
    assert_eq!(assistant_msg.stats.input_tokens, 69); // 2629 - 2560 (cached tokens subtracted)
    assert_eq!(assistant_msg.stats.output_tokens, 14); // Codex output_tokens already include reasoning
    assert_eq!(assistant_msg.stats.reasoning_tokens, 0);
    assert_eq!(assistant_msg.stats.cached_tokens, 2560);
    assert_eq!(assistant_msg.project_path.as_deref(), Some("/home/test"));
    assert_eq!(
        assistant_msg.project_hash,
        crate::utils::hash_text("https://github.com/test/repo.git")
    );
    let serialized = simd_json::to_string(assistant_msg).unwrap();
    assert!(!serialized.contains("projectPath"));
}

#[test]
fn test_codex_cli_turn_context_updates_project_path() {
    let mut temp_file = NamedTempFile::new().unwrap();
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:00:00.000Z","type":"session_meta","payload":{{"id":"session","timestamp":"2025-09-18T00:00:00.000Z","cwd":"/home/test/old"}}}}"#
    )
    .unwrap();
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:00:01.000Z","type":"turn_context","payload":{{"cwd":"/home/test/new/./project","model":"gpt-5-codex"}}}}"#
    )
    .unwrap();
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:00:02.000Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"Hello"}}]}}}}"#
    )
    .unwrap();

    let (messages, _) = parse_codex_cli_jsonl_file(temp_file.path()).unwrap();

    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].project_path.as_deref(),
        Some("/home/test/new/project")
    );
    assert_eq!(
        messages[0].project_hash,
        crate::utils::hash_text("/home/test/new/project")
    );
}

#[test]
fn test_parse_codex_cli_wrapper_format_no_tokens() {
    // Test wrapper format without token information
    let mut temp_file = NamedTempFile::new().unwrap();

    // Turn context with model info
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:36.676Z","type":"turn_context","payload":{{"model":"gpt-4o"}}}}"#
    ).unwrap();

    // Assistant message without preceding token count
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:38.852Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Hello!"}}]}}}}"#
    ).unwrap();

    let (messages, _model) = parse_codex_cli_jsonl_file(temp_file.path()).unwrap();

    // Should have parsed the assistant message
    assert!(!messages.is_empty());

    let assistant_msg = messages
        .iter()
        .find(|msg| matches!(msg.role, crate::types::MessageRole::Assistant))
        .unwrap();

    assert_eq!(assistant_msg.model, Some("gpt-4o".to_string()));
    // Should have default stats when no token info is available
    assert_eq!(assistant_msg.stats.input_tokens, 0);
    assert_eq!(assistant_msg.stats.output_tokens, 0);
}

#[test]
fn test_codex_cli_fallback_session_name_from_first_user_message() {
    let mut temp_file = NamedTempFile::new().unwrap();

    // Turn context without summary
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:36.676Z","type":"turn_context","payload":{{"cwd":"/home/test","approval_policy":"on-request","model":"gpt-5-codex"}}}}"#
    )
    .unwrap();

    // User message with input_text
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:36.675Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"This is a Codex CLI session title that should be truncated for display."}}]}}}}"#
    )
    .unwrap();

    let (messages, _model) = parse_codex_cli_jsonl_file(temp_file.path()).unwrap();

    // Fallback session name should be derived from the first user message
    let names: Vec<String> = messages
        .iter()
        .filter_map(|msg| msg.session_name.clone())
        .collect();

    assert!(!names.is_empty());
    let name = &names[0];
    assert!(name.starts_with("This is a Codex CLI session title"));
    assert!(name.ends_with("..."));
    assert_eq!(name.chars().count(), 53); // 50 chars + "..."
}

#[test]
fn test_codex_cli_session_name_skips_recommended_plugins_context() {
    let mut temp_file = NamedTempFile::new().unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2026-07-30T00:00:00.000Z","type":"turn_context","payload":{{"model":"gpt-5.6-sol"}}}}"#
    )
    .unwrap();
    writeln!(
        temp_file,
        r#"{{"timestamp":"2026-07-30T00:00:01.000Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"<recommended_plugins>\nHere is a list of plugins that are available but not installed.\n</recommended_plugins>"}}]}}}}"#
    )
    .unwrap();
    writeln!(
        temp_file,
        r#"{{"timestamp":"2026-07-30T00:00:02.000Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"分析下这两个网关 issue"}}]}}}}"#
    )
    .unwrap();

    let (messages, _model) = parse_codex_cli_jsonl_file(temp_file.path()).unwrap();

    assert!(
        messages
            .iter()
            .filter_map(|message| message.session_name.as_deref())
            .all(|name| !name.starts_with("<recommended_plugins>"))
    );
    assert_eq!(
        messages
            .last()
            .and_then(|message| message.session_name.as_deref()),
        Some("分析下这两个网关 issue")
    );
}

#[test]
fn test_parse_codex_cli_counts_tool_calls() {
    let mut temp_file = NamedTempFile::new().unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:20:00.000Z","type":"turn_context","payload":{{"model":"gpt-5-codex"}}}}"#
    )
    .unwrap();

    // Simulate user input to start the conversation
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:20:01.000Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"Do the thing"}}]}}}}"#
    )
    .unwrap();

    // Multiple tool calls, including a duplicate call_id that should be deduplicated
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:20:02.000Z","type":"response_item","payload":{{"type":"function_call","name":"shell","arguments":"{{\"command\":[\"bash\",\"-lc\",\"echo first\"],\"workdir\":\"/tmp\"}}","call_id":"call_duplicate"}}}}"#
    )
    .unwrap();
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:20:03.000Z","type":"response_item","payload":{{"type":"function_call","name":"shell","arguments":"{{\"command\":[\"bash\",\"-lc\",\"echo first\"],\"workdir\":\"/tmp\"}}","call_id":"call_duplicate"}}}}"#
    )
    .unwrap();
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:20:04.000Z","type":"response_item","payload":{{"type":"function_call","name":"shell","arguments":"{{\"command\":[\"bash\",\"-lc\",\"echo second\"],\"workdir\":\"/tmp\"}}","call_id":"call_unique"}}}}"#
    )
    .unwrap();

    // Token usage emitted for the assistant response
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:20:05.000Z","type":"event_msg","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":120,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5,"total_tokens":155}}}}}}}}"#
    )
    .unwrap();

    // Assistant message content (ignored for stats but included for completeness)
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:20:06.000Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"All set!"}}]}}}}"#
    )
    .unwrap();

    let (messages, _model) = parse_codex_cli_jsonl_file(temp_file.path()).unwrap();

    let assistant_msg = messages
        .iter()
        .find(|msg| matches!(msg.role, crate::types::MessageRole::Assistant))
        .unwrap();

    assert_eq!(assistant_msg.stats.tool_calls, 2);
    assert_eq!(assistant_msg.stats.terminal_commands, 2);
    assert_eq!(assistant_msg.stats.input_tokens, 100);
    assert_eq!(assistant_msg.stats.output_tokens, 30);
    assert_eq!(assistant_msg.stats.reasoning_tokens, 5);
}

#[test]
fn test_parse_codex_cli_maps_tool_calls_to_activity_stats() {
    let mut temp_file = NamedTempFile::new().unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2026-10-17T12:00:00.000Z","type":"turn_context","payload":{{"model":"gpt-5-codex"}}}}"#
    )
    .unwrap();
    writeln!(
        temp_file,
        r#"{{"timestamp":"2026-10-17T12:00:01.000Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"Inspect the repo"}}]}}}}"#
    )
    .unwrap();
    writeln!(
        temp_file,
        r#"{{"timestamp":"2026-10-17T12:00:02.000Z","type":"response_item","payload":{{"type":"function_call","name":"exec_command","arguments":"{{}}","call_id":"call_exec"}}}}"#
    )
    .unwrap();
    writeln!(
        temp_file,
        r#"{{"timestamp":"2026-10-17T12:00:03.000Z","type":"response_item","payload":{{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch","call_id":"call_patch"}}}}"#
    )
    .unwrap();
    writeln!(
        temp_file,
        r#"{{"timestamp":"2026-10-17T12:00:04.000Z","type":"response_item","payload":{{"type":"function_call","name":"read_file","arguments":"{{}}","call_id":"call_read"}}}}"#
    )
    .unwrap();
    writeln!(
        temp_file,
        r#"{{"timestamp":"2026-10-17T12:00:05.000Z","type":"response_item","payload":{{"type":"custom_tool_call","name":"write_file","input":"hello","call_id":"call_write"}}}}"#
    )
    .unwrap();
    writeln!(
        temp_file,
        r#"{{"timestamp":"2026-10-17T12:00:06.000Z","type":"event_msg","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":120,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5,"total_tokens":155}}}}}}}}"#
    )
    .unwrap();

    let (messages, _model) = parse_codex_cli_jsonl_file(temp_file.path()).unwrap();
    let assistant_msg = messages
        .iter()
        .find(|msg| matches!(msg.role, crate::types::MessageRole::Assistant))
        .unwrap();

    assert_eq!(assistant_msg.stats.tool_calls, 4);
    assert_eq!(assistant_msg.stats.terminal_commands, 1);
    assert_eq!(assistant_msg.stats.files_edited, 1);
    assert_eq!(assistant_msg.stats.files_read, 1);
    assert_eq!(assistant_msg.stats.files_added, 1);
}

#[test]
fn test_parse_codex_cli_missing_model() {
    let mut temp_file = NamedTempFile::new().unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:27.465Z","type":"session_meta","payload":{{"id":"b51f4ba9-0bf5-40b0-8e70-4f339c4b4f52","timestamp":"2025-09-18T00:16:27.461Z","cwd":"/home/test","originator":"codex_cli_rs","cli_version":"0.38.0","instructions":null,"git":null}}}}"#
    )
    .unwrap();

    // Token usage without any turn context to provide model information
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:30.000Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":20,"reasoning_output_tokens":10,"total_tokens":1020}},"last_token_usage":{{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":20,"reasoning_output_tokens":10,"total_tokens":1020}},"model_context_window":272000}}}}}}"#
    )
    .unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:31.000Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"No model info available."}}]}}}}"#
    )
    .unwrap();

    let (messages, _model) = parse_codex_cli_jsonl_file(temp_file.path()).unwrap();

    let assistant_msg = messages
        .iter()
        .find(|msg| matches!(msg.role, crate::types::MessageRole::Assistant))
        .unwrap();

    assert_eq!(assistant_msg.model, Some("gpt-5".to_string()));
    assert_eq!(assistant_msg.stats.input_tokens, 600); // 1000 - 400 cached
    assert_eq!(assistant_msg.stats.output_tokens, 20);
    assert_eq!(assistant_msg.stats.reasoning_tokens, 10);
    assert_eq!(assistant_msg.stats.cached_tokens, 400);
    let expected_cost = calculate_total_cost("gpt-5", 600, 20, 0, 400);
    assert!((assistant_msg.stats.cost - expected_cost).abs() < f64::EPSILON);
}

#[test]
fn test_parse_codex_cli_turn_context_metadata_model() {
    let mut temp_file = NamedTempFile::new().unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T01:00:00.000Z","type":"session_meta","payload":{{"id":"e7b89af6-58bf-4b3e-8118-8cef1cf9f7cd","timestamp":"2025-09-18T01:00:00.000Z","cwd":"/home/test","originator":"codex_cli_rs","cli_version":"0.38.0"}}}}"#
    )
    .unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T01:00:05.000Z","type":"turn_context","payload":{{"metadata":{{"model":"gpt-5-mini"}}}}}}"#
    )
    .unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T01:00:06.000Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Hello!"}}]}}}}"#
    )
    .unwrap();

    let (messages, _model) = parse_codex_cli_jsonl_file(temp_file.path()).unwrap();

    let assistant_msg = messages
        .iter()
        .find(|msg| matches!(msg.role, crate::types::MessageRole::Assistant))
        .unwrap();

    assert_eq!(assistant_msg.model, Some("gpt-5-mini".to_string()));
}

#[test]
fn test_parse_codex_cli_event_model_backfill() {
    let mut temp_file = NamedTempFile::new().unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T02:00:00.000Z","type":"session_meta","payload":{{"id":"95b7e78c-3a23-4f3a-bc06-2ce7acc82941","timestamp":"2025-09-18T02:00:00.000Z"}}}}"#
    )
    .unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T02:00:02.000Z","type":"event_msg","payload":{{"type":"token_count","info":{{"model":"gpt-5-codex","total_token_usage":{{"input_tokens":120,"cached_input_tokens":0,"output_tokens":25,"reasoning_output_tokens":5,"total_tokens":145}}}}}}}}"#
    )
    .unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T02:00:03.000Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Hi!"}}]}}}}"#
    )
    .unwrap();

    let (messages, _model) = parse_codex_cli_jsonl_file(temp_file.path()).unwrap();

    let assistant_msg = messages
        .iter()
        .find(|msg| matches!(msg.role, crate::types::MessageRole::Assistant))
        .unwrap();

    assert_eq!(assistant_msg.model, Some("gpt-5-codex".to_string()));
    assert_eq!(assistant_msg.stats.input_tokens, 120);
    assert_eq!(assistant_msg.stats.output_tokens, 25);
    assert_eq!(assistant_msg.stats.reasoning_tokens, 5);
    assert_eq!(assistant_msg.stats.cached_tokens, 0);
    let expected_cost = calculate_total_cost("gpt-5-codex", 120, 25, 0, 0);
    assert!((assistant_msg.stats.cost - expected_cost).abs() < f64::EPSILON);
}

#[test]
fn test_codex_availability() {
    let analyzer = CodexCliAnalyzer::new();

    println!(
        "Codex CLI data patterns: {:?}",
        analyzer.get_data_glob_patterns()
    );

    let sources = analyzer.discover_data_sources();
    println!("Discovered sources: {:?}", sources);

    let is_available = analyzer.is_available();
    println!("Is available: {}", is_available);

    // For debugging - let's check if the home directory exists
    if let Some(home_dir) = dirs::home_dir() {
        let codex_path = home_dir.join(".codex/sessions");
        println!("Codex path exists: {}", codex_path.exists());
        if codex_path.exists() {
            println!("Contents: {:?}", std::fs::read_dir(&codex_path));
        }
    }

    // Don't assert - just print for debugging
}

#[test]
fn test_get_fallback_model_with_roots() {
    let dir = tempfile::tempdir().unwrap();
    let codex_dir = dir.path().join(".codex");
    std::fs::create_dir(&codex_dir).unwrap();
    let config_path = codex_dir.join("config.toml");

    // Test case 1: no config file
    let model = get_fallback_model_with_roots(None, Some(dir.path()));
    assert_eq!(model, "gpt-5");

    // Test case 2: empty config file
    let mut file = std::fs::File::create(&config_path).unwrap();
    file.write_all(b"").unwrap();
    drop(file);
    let model = get_fallback_model_with_roots(None, Some(dir.path()));
    assert_eq!(model, "gpt-5");

    // Test case 3: model configured in config file
    let mut file = std::fs::File::create(&config_path).unwrap();
    file.write_all(b"model = \"gpt-5.5\"\n").unwrap();
    drop(file);
    let model = get_fallback_model_with_roots(None, Some(dir.path()));
    assert_eq!(model, "gpt-5.5");

    // Test case 4: empty string in config
    let mut file = std::fs::File::create(&config_path).unwrap();
    file.write_all(b"model = \"\"\n").unwrap();
    drop(file);
    let model = get_fallback_model_with_roots(None, Some(dir.path()));
    assert_eq!(model, "gpt-5");

    // Test case 5: None home directory
    let model = get_fallback_model_with_roots(None, None);
    assert_eq!(model, "gpt-5");

    // Test case 6: whitespace-only model
    let mut file = std::fs::File::create(&config_path).unwrap();
    file.write_all(b"model = \"   \"\n").unwrap();
    drop(file);
    let model = get_fallback_model_with_roots(None, Some(dir.path()));
    assert_eq!(model, "gpt-5");

    // Test case 7: invalid TOML syntax
    let mut file = std::fs::File::create(&config_path).unwrap();
    file.write_all(b"model = \n").unwrap();
    drop(file);
    let model = get_fallback_model_with_roots(None, Some(dir.path()));
    assert_eq!(model, "gpt-5");

    // Test case 8: config file exists but model key is omitted
    let mut file = std::fs::File::create(&config_path).unwrap();
    file.write_all(b"other_key = \"value\"\n").unwrap();
    drop(file);
    let model = get_fallback_model_with_roots(None, Some(dir.path()));
    assert_eq!(model, "gpt-5");
}

#[test]
fn test_fallback_model_respects_codex_home() {
    let dir = tempfile::tempdir().unwrap();
    let custom = dir.path().join("custom-codex");
    std::fs::create_dir(&custom).unwrap();
    std::fs::write(custom.join("config.toml"), "model = \"gpt-5.5\"\n").unwrap();

    assert_eq!(
        get_fallback_model_with_roots(Some(custom.as_os_str()), Some(dir.path())),
        "gpt-5.5"
    );
}
