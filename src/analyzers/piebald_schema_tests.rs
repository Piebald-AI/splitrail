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

/// Native KSUIDs for the fixture's legacy integer rows, as `_ksuid_map` records them.
/// Only shape matters to the analyzer: opaque TEXT keys that differ from the old IDs.
const KSUID_CHAT: &str = "38MTL3YQHcJ50adIYMEcBPnRkPH";
const KSUID_MESSAGES: [&str; 5] = [
    "38MTLQWq9LKkm4x1a9khmqx9fnK",
    "38MTLSiqfyJ1zjxQcQJjcNqgYGD",
    "38MTLTqvJmzBcRYqY1qTwdF6mHk",
    "38MTLV3hX7QyNnB2u0w5c5hYxQp",
    "38MTLWbXy3o1nm5u5o2qkkqL0Ps",
];

/// Build the typed-parts history after Piebald's KSUID flip: every key is TEXT and
/// `_ksuid_map` retains the old integers. Generation configs still exist here, as
/// they did between the flip and the later fold, so the full legacy history applies.
fn ksuid_fixture() -> (TempDir, DataSource) {
    let directory = tempfile::tempdir().unwrap();
    let source = DataSource {
        path: directory.path().join("app.db"),
    };
    let conn = Connection::open(&source.path).unwrap();
    let [m1, m2, m3, m4, m5] = KSUID_MESSAGES;
    conn.execute_batch(&format!(
        "CREATE TABLE _ksuid_map (table_name TEXT NOT NULL, old_id INTEGER NOT NULL,
                                  new_id TEXT NOT NULL, PRIMARY KEY (table_name, old_id),
                                  UNIQUE (table_name, new_id));
         CREATE TABLE projects (id TEXT PRIMARY KEY, directory TEXT);
         CREATE TABLE profiles (id TEXT PRIMARY KEY);
         CREATE TABLE chats (id TEXT PRIMARY KEY, title TEXT NOT NULL, model TEXT,
                             project_id TEXT, profile_id TEXT NOT NULL, created_at TEXT);
         CREATE TABLE messages (id TEXT PRIMARY KEY, parent_chat_id TEXT, role TEXT,
                                created_at TEXT, updated_at TEXT,
                                message_kind TEXT NOT NULL DEFAULT 'normal');
         CREATE TABLE message_parts (id TEXT PRIMARY KEY,
                                     parent_chat_message_id TEXT, part_type TEXT,
                                     part_subtype TEXT);
         CREATE TABLE message_generations (message_id TEXT PRIMARY KEY, profile_id TEXT,
                                           config_id TEXT, model TEXT,
                                           input_tokens BIGINT, output_tokens BIGINT,
                                           reasoning_tokens BIGINT, cache_read_tokens BIGINT,
                                           cache_write_tokens BIGINT);
         CREATE TABLE tool_execution_context (message_part_id TEXT PRIMARY KEY);
         CREATE TABLE override_gen_cfg_data_openai_responses
             (gen_cfg_id TEXT PRIMARY KEY, service_tier TEXT);
         CREATE TABLE override_gen_cfg_data_openai_completions
             (gen_cfg_id TEXT PRIMARY KEY, service_tier TEXT);

         INSERT INTO _ksuid_map VALUES ('chats', 1, '{KSUID_CHAT}'),
             ('messages', 1, '{m1}'), ('messages', 2, '{m2}'), ('messages', 3, '{m3}'),
             ('messages', 4, '{m4}'), ('messages', 5, '{m5}');
         INSERT INTO projects VALUES ('38MTKprojectKsuid0000000001', '/tmp/piebald-project');
         INSERT INTO profiles VALUES ('38MTKsourceProfile000000001');
         INSERT INTO chats VALUES ('{KSUID_CHAT}', 'Schema compatibility', 'gpt-5.4',
                                   '38MTKprojectKsuid0000000001',
                                   '38MTKsourceProfile000000001', '2026-05-01T12:00:00Z');
         INSERT INTO messages VALUES
             ('{m1}', '{KSUID_CHAT}', 'user', '2026-05-01T12:00:00Z', '2026-05-01T12:00:00Z', 'normal'),
             ('{m2}', '{KSUID_CHAT}', 'assistant', '2026-05-01T12:00:01Z', '2026-05-01T12:00:05Z', 'normal'),
             ('{m3}', '{KSUID_CHAT}', 'assistant', '2026-05-01T12:00:02Z', '2026-05-01T12:00:06Z', 'normal'),
             ('{m4}', '{KSUID_CHAT}', 'assistant', '2026-05-01T12:00:03Z', '2026-05-01T12:00:07Z', 'normal'),
             ('{m5}', '{KSUID_CHAT}', 'user', '2026-05-01T12:00:04Z', '2026-05-01T12:00:08Z',
              'context_container');
         INSERT INTO message_generations VALUES
             ('{m2}', '38MTKsourceProfile000000001', 'cfg20', 'gpt-5.5', 1000, 200, 30, 100, 50),
             ('{m3}', '38MTKsourceProfile000000001', 'cfg30', NULL, 500, 100, 10, 0, 0);
         INSERT INTO override_gen_cfg_data_openai_responses VALUES ('cfg20', 'priority');
         INSERT INTO override_gen_cfg_data_openai_completions VALUES
             ('cfg20', 'flex'), ('cfg30', 'flex');
         INSERT INTO message_parts VALUES ('p1', '{m2}', 'text', NULL),
             ('p2', '{m2}', 'tool', 'read_file'), ('p3', '{m2}', 'tool', 'mcp'),
             ('p4', '{m3}', 'tool', 'streaming'), ('p5', '{m5}', 'context', 'agent_rules');
         INSERT INTO tool_execution_context VALUES ('p2'), ('p3'), ('p4');"
    ))
    .unwrap();
    (directory, source)
}

/// Apply Piebald's generation-config fold to a KSUID fixture: the per-turn configs
/// and their override tables disappear, and settings move onto profiles. The chat
/// points at a hidden clone carrying its own tier, while generations keep naming
/// the visible source profile, whose settings must not be used.
fn fold_generation_configs(source: &DataSource) {
    // Only this disposable fixture is modified; production databases are read-only.
    let conn = Connection::open(&source.path).unwrap();
    conn.execute_batch(&format!(
        "ALTER TABLE message_generations DROP COLUMN config_id;
         DROP TABLE override_gen_cfg_data_openai_responses;
         DROP TABLE override_gen_cfg_data_openai_completions;
         CREATE TABLE profile_settings_openai_responses
             (profile_id TEXT PRIMARY KEY, service_tier TEXT);
         CREATE TABLE profile_settings_openai_completions
             (profile_id TEXT PRIMARY KEY, service_tier TEXT);
         INSERT INTO profiles VALUES ('38MTKhiddenCloneProfile0001');
         UPDATE chats SET profile_id = '38MTKhiddenCloneProfile0001' WHERE id = '{KSUID_CHAT}';
         INSERT INTO profile_settings_openai_responses VALUES
             ('38MTKhiddenCloneProfile0001', 'priority'),
             ('38MTKsourceProfile000000001', 'flex');
         INSERT INTO profile_settings_openai_completions VALUES
             ('38MTKhiddenCloneProfile0001', 'flex');"
    ))
    .unwrap();
}

#[test]
fn ksuid_flip_preserves_history_and_pre_flip_identities() {
    let (_legacy_directory, legacy) = fixture(PiebaldSchema::Legacy);
    let (_ksuid_directory, ksuid) = ksuid_fixture();
    let analyzer = PiebaldAnalyzer::new();
    let flipped = analyzer.parse_source(&ksuid).unwrap();
    assert_history(&flipped);
    // Byte-identical output, including global hashes, proves pre-flip messages are
    // neither lost nor re-identified, so Splitrail Cloud will not count them twice.
    assert_eq!(
        simd_json::to_string(&analyzer.parse_source(&legacy).unwrap()).unwrap(),
        simd_json::to_string(&flipped).unwrap()
    );
}

#[test]
fn post_flip_rows_use_ksuids_and_drafts_are_ignored() {
    let (_directory, source) = ksuid_fixture();
    let conn = Connection::open(&source.path).unwrap();
    conn.execute_batch(&format!(
        "INSERT INTO messages VALUES
             ('3KSoWEjm2TfxHgblgGUuZQhwtq6', '{KSUID_CHAT}', 'assistant',
              '2026-10-09T15:39:19.371208769+00:00', '2026-10-09T15:39:20+00:00', 'normal'),
             ('3KSoWdraftMessage0000000001', NULL, 'user',
              '2026-10-09T15:40:00+00:00', '2026-10-09T15:40:00+00:00', 'normal');
         INSERT INTO message_generations VALUES ('3KSoWEjm2TfxHgblgGUuZQhwtq6',
             NULL, NULL, 'gpt-5.5', 10, 20, 0, 0, 0);"
    ))
    .unwrap();

    let messages = PiebaldAnalyzer::new().parse_source(&source).unwrap();
    // The NULL-parent draft is not a conversation turn and must not fail the parse.
    assert_eq!(messages.len(), 5);
    let new = messages.last().unwrap();
    assert_eq!(new.uuid.as_deref(), Some("3KSoWEjm2TfxHgblgGUuZQhwtq6"));
    assert_eq!(
        new.global_hash,
        hash_text("piebald_2026-10-09T15:39:19.371208769+00:00_3KSoWEjm2TfxHgblgGUuZQhwtq6")
    );
    // The chat itself was mapped, so its session identity is still the old integer.
    assert_eq!(new.conversation_hash, "1");
    assert_eq!(new.stats.output_tokens, 20);
}

#[test]
fn folded_generation_configs_take_tier_from_the_chat_profile() {
    let (_directory, source) = ksuid_fixture();
    fold_generation_configs(&source);
    let messages = PiebaldAnalyzer::new().parse_source(&source).unwrap();
    assert_eq!(messages.len(), 4);

    // The fold erased per-turn tiers, so both turns now use the hidden clone's
    // Responses tier. The source profile's `flex` must not leak in.
    for (message, model, input, output, write, read) in [
        (&messages[1], "gpt-5.5", 900, 230, 50, 100),
        (&messages[2], "gpt-5.4", 500, 110, 0, 0),
    ] {
        let expected = calculate_total_cost_for_service_tier_at(
            model,
            ServiceTier::Priority,
            input,
            output,
            write,
            read,
            Some(message.date),
        );
        assert!(expected > 0.0);
        assert_eq!(message.stats.cost, expected, "{model}");
    }
    assert_eq!(messages[1].stats.tool_calls, 2);
    assert_eq!(messages[2].stats.tool_calls, 1);
    assert_eq!(
        messages[1].global_hash,
        hash_text("piebald_2026-05-01T12:00:01Z_2")
    );
}
