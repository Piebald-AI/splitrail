//! On-disk fixtures exercise the public read-only parser against both Piebald
//! layouts. The typed fixture deliberately omits the dropped columns and table:
//! retaining them would let accidental legacy queries pass unnoticed.

use super::*;
use tempfile::TempDir;

/// Create equivalent usage histories in either layout. User and metadata-less
/// assistant rows exercise outer joins; a NULL generation model exercises the
/// existing chat-model fallback. Both provider config tables must remain usable.
fn fixture(schema: PiebaldSchema) -> (TempDir, DataSource) {
    let directory = tempfile::tempdir().unwrap();
    let source = DataSource {
        path: directory.path().join("app.db"),
    };
    let conn = Connection::open(&source.path).unwrap();
    conn.execute_batch(
        "CREATE TABLE projects (id INTEGER PRIMARY KEY, directory TEXT);
         CREATE TABLE chats (id INTEGER PRIMARY KEY, title TEXT, model TEXT,
                             project_id INTEGER, created_at TEXT);
         CREATE TABLE messages (id INTEGER PRIMARY KEY, parent_chat_id INTEGER,
                                role TEXT, created_at TEXT, updated_at TEXT);
         CREATE TABLE message_parts (id INTEGER PRIMARY KEY,
                                     parent_chat_message_id INTEGER, part_type TEXT);
         CREATE TABLE override_gen_cfg_data_openai_responses
             (gen_cfg_id INTEGER PRIMARY KEY, service_tier TEXT);
         CREATE TABLE override_gen_cfg_data_openai_completions
             (gen_cfg_id INTEGER PRIMARY KEY, service_tier TEXT);
         INSERT INTO projects VALUES (1, '/tmp/piebald-project');
         INSERT INTO chats VALUES (1, 'Schema compatibility', 'gpt-5.4', 1,
                                   '2026-05-01T12:00:00Z');
         INSERT INTO messages VALUES
             (1, 1, 'user', '2026-05-01T12:00:00Z', '2026-05-01T12:00:00Z'),
             (2, 1, 'assistant', '2026-05-01T12:00:01Z', '2026-05-01T12:00:05Z'),
             (3, 1, 'assistant', '2026-05-01T12:00:02Z', '2026-05-01T12:00:06Z'),
             (4, 1, 'assistant', '2026-05-01T12:00:03Z', '2026-05-01T12:00:07Z');
         INSERT INTO override_gen_cfg_data_openai_responses VALUES (20, 'priority');
         INSERT INTO override_gen_cfg_data_openai_completions VALUES (20, 'flex'), (30, 'flex');
         INSERT INTO message_parts VALUES (1, 2, 'text'), (2, 2, 'tool'),
                                         (3, 2, 'tool'), (4, 3, 'tool');",
    )
    .unwrap();

    let usage_columns = "model TEXT, config_id INTEGER, input_tokens INTEGER,
                         output_tokens INTEGER, reasoning_tokens INTEGER,
                         cache_read_tokens INTEGER, cache_write_tokens INTEGER";
    match schema {
        PiebaldSchema::Legacy => {
            // Splitting this fixed fixture declaration is safe: no user input is SQL.
            for column in usage_columns.split(',') {
                conn.execute_batch(&format!("ALTER TABLE messages ADD COLUMN {column};"))
                    .unwrap();
            }
            conn.execute_batch(
                "UPDATE messages SET model = 'gpt-5.5', config_id = 20,
                     input_tokens = 1000, output_tokens = 200, reasoning_tokens = 30,
                     cache_read_tokens = 100, cache_write_tokens = 50 WHERE id = 2;
                 UPDATE messages SET config_id = 30,
                     input_tokens = 500, output_tokens = 100, reasoning_tokens = 10,
                     cache_read_tokens = 0, cache_write_tokens = 0 WHERE id = 3;
                 CREATE TABLE message_part_tool_call (message_part_id INTEGER PRIMARY KEY);
                 INSERT INTO message_part_tool_call VALUES (2), (3), (4);",
            )
            .unwrap();
        }
        PiebaldSchema::TypedParts => {
            conn.execute_batch(&format!(
                "ALTER TABLE messages ADD COLUMN message_kind TEXT NOT NULL DEFAULT 'normal';
                 CREATE TABLE message_generations (message_id INTEGER PRIMARY KEY, {usage_columns});
                 INSERT INTO message_generations VALUES
                     (2, 'gpt-5.5', 20, 1000, 200, 30, 100, 50),
                     (3, NULL, 30, 500, 100, 10, 0, 0);
                 CREATE TABLE tool_execution_context (message_part_id INTEGER PRIMARY KEY);
                 INSERT INTO tool_execution_context VALUES (2), (3), (4);
                 ALTER TABLE message_parts ADD COLUMN part_subtype TEXT;
                 UPDATE message_parts SET part_subtype = 'read_file' WHERE id = 2;
                 UPDATE message_parts SET part_subtype = 'mcp' WHERE id = 3;
                 UPDATE message_parts SET part_subtype = 'streaming' WHERE id = 4;
                 INSERT INTO messages VALUES
                     (5, 1, 'user', '2026-05-01T12:00:04Z', '2026-05-01T12:00:08Z', 'context_container');
                 INSERT INTO message_parts VALUES (5, 5, 'context', 'agent_rules');"
            ))
            .unwrap();
        }
    }
    (directory, source)
}

/// Assert accounting, provider tier precedence, identity, and zero-usage rows
/// independently of schema parity, so a shared regression cannot pass both sides.
fn assert_history(messages: &[ConversationMessage]) {
    assert_eq!(messages.len(), 4);
    let user = &messages[0];
    assert_eq!(user.role, MessageRole::User);
    assert!(user.model.is_none());
    assert_eq!(user.stats.input_tokens, 0);
    assert_eq!(user.stats.tool_calls, 0);

    let assistant = &messages[1];
    assert_eq!(assistant.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(assistant.stats.input_tokens, 900);
    assert_eq!(assistant.stats.output_tokens, 200);
    assert_eq!(assistant.stats.reasoning_tokens, 30);
    assert_eq!(assistant.stats.cache_read_tokens, 100);
    assert_eq!(assistant.stats.cache_creation_tokens, 50);
    assert_eq!(assistant.stats.tool_calls, 2);
    assert_eq!(
        assistant.project_path.as_deref(),
        Some("/tmp/piebald-project")
    );
    assert_eq!(
        assistant.session_name.as_deref(),
        Some("Schema compatibility")
    );
    assert_eq!(
        assistant.date,
        parse_timestamp("2026-05-01T12:00:05Z").unwrap()
    );
    assert_eq!(
        assistant.global_hash,
        hash_text("piebald_2026-05-01T12:00:01Z_2")
    );
    let priority_cost = calculate_total_cost_for_service_tier_at(
        "gpt-5.5",
        ServiceTier::Priority,
        900,
        230,
        50,
        100,
        Some(assistant.date),
    );
    assert!(priority_cost > 0.0);
    assert_eq!(assistant.stats.cost, priority_cost);

    let fallback = &messages[2];
    assert_eq!(fallback.model.as_deref(), Some("gpt-5.4"));
    assert_eq!(fallback.stats.tool_calls, 1);
    let flex_cost = calculate_total_cost_for_service_tier_at(
        "gpt-5.4",
        ServiceTier::Flex,
        500,
        110,
        0,
        0,
        Some(fallback.date),
    );
    assert!(flex_cost > 0.0);
    assert_eq!(fallback.stats.cost, flex_cost);
    assert_eq!(messages[3].stats.input_tokens, 0);
    assert_eq!(messages[3].stats.cost, 0.0);
    assert_eq!(messages[3].stats.tool_calls, 0);
}

#[test]
fn legacy_schema_preserves_usage_and_tools() {
    let (_directory, source) = fixture(PiebaldSchema::Legacy);
    assert_history(&PiebaldAnalyzer::new().parse_source(&source).unwrap());
}

#[test]
fn typed_schema_preserves_usage_and_tools_without_context_containers() {
    let (_directory, source) = fixture(PiebaldSchema::TypedParts);
    assert_history(&PiebaldAnalyzer::new().parse_source(&source).unwrap());
}

#[test]
fn schema_migration_preserves_normalized_history() {
    let (_legacy_directory, legacy) = fixture(PiebaldSchema::Legacy);
    let (_typed_directory, typed) = fixture(PiebaldSchema::TypedParts);
    let analyzer = PiebaldAnalyzer::new();
    let old = analyzer.parse_source(&legacy).unwrap();
    let new = analyzer.parse_source(&typed).unwrap();
    assert_eq!(
        simd_json::to_string(&old).unwrap(),
        simd_json::to_string(&new).unwrap()
    );
}

#[test]
fn broken_typed_schema_is_an_error_not_empty_history_or_legacy_fallback() {
    let (_directory, source) = fixture(PiebaldSchema::TypedParts);
    let conn = Connection::open(&source.path).unwrap();
    // Only this disposable fixture is damaged; production databases are read-only.
    conn.execute_batch("ALTER TABLE message_generations RENAME COLUMN model TO missing_model;")
        .unwrap();
    let error = PiebaldAnalyzer::new().parse_source(&source).unwrap_err();
    assert!(error.to_string().contains("g.model"), "{error}");
}

#[test]
fn additive_schema_keeps_legacy_tools_until_finalization() {
    let (_directory, source) = fixture(PiebaldSchema::Legacy);
    let analyzer = PiebaldAnalyzer::new();
    let before = analyzer.parse_source(&source).unwrap();
    let conn = Connection::open(&source.path).unwrap();
    // Piebald commits these additions before its separate backfill transaction.
    // The copied generations exist, but legacy calls remain authoritative until
    // finalization: the new execution table is still empty if backfill fails.
    conn.execute_batch(
        "CREATE TABLE message_generations AS
             SELECT id AS message_id, model, config_id, input_tokens, output_tokens,
                    reasoning_tokens, cache_read_tokens, cache_write_tokens
             FROM messages WHERE role = 'assistant';
         CREATE TABLE tool_execution_context (message_part_id INTEGER PRIMARY KEY);
         ALTER TABLE messages ADD COLUMN message_kind TEXT NOT NULL DEFAULT 'normal';",
    )
    .unwrap();
    let intermediate = analyzer.parse_source(&source).unwrap();
    assert_history(&intermediate);
    assert_eq!(
        simd_json::to_string(&before).unwrap(),
        simd_json::to_string(&intermediate).unwrap()
    );
}
