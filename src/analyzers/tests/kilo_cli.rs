use crate::analyzer::Analyzer;
use crate::analyzers::kilo_cli::KiloCliAnalyzer;
use crate::analyzers::opencode::{
    OpenCodeCacheTokens, OpenCodeMessage, OpenCodeMessageTime, OpenCodeTokens,
    batch_load_step_finish_from_db, batch_load_tool_stats_from_db, build_conversation_message,
    compute_message_stats, load_projects_from_db, load_sessions_from_db,
};
use crate::types::{Application, MessageRole, Stats};
use crate::utils::hash_text;
use rusqlite::Connection;

// ===========================================================================
// Basic analyzer tests
// ===========================================================================

#[test]
fn test_kilo_cli_analyzer_creation() {
    let analyzer = KiloCliAnalyzer::new();
    assert_eq!(analyzer.display_name(), "Kilo CLI");
}

#[test]
fn test_kilo_cli_is_available() {
    let analyzer = KiloCliAnalyzer::new();
    // is_available depends on whether Kilo CLI data exists
    // Just verify it doesn't panic
    let _ = analyzer.is_available();
}

#[test]
fn test_kilo_cli_discover_data_sources_no_panic() {
    let analyzer = KiloCliAnalyzer::new();
    // Should return Ok even if no data exists
    let result = analyzer.discover_data_sources();
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_kilo_cli_get_stats_empty_sources() {
    let analyzer = KiloCliAnalyzer::new();
    let result = analyzer.get_stats_with_sources(vec![]);
    assert!(result.is_ok());
    assert!(result.unwrap().messages.is_empty());
}

// ===========================================================================
// SQLite data blob parsing tests
// ===========================================================================

#[test]
fn test_parse_kilo_sqlite_assistant_data_blob() {
    // Data blob as stored in `message.data` (no id/sessionID fields).
    // Uses Kilo-style providerID and model format.
    let json = r#"{
        "role": "assistant",
        "time": { "created": 1771083036854, "completed": 1771083042377 },
        "parentID": "msg_c5cc678a5001rnYk063tHohMCW",
        "modelID": "minimax/minimax-m2.5:free",
        "providerID": "kilo",
        "mode": "code",
        "agent": "code",
        "cost": 0.00111015,
        "tokens": {
            "total": 31524,
            "input": 362,
            "output": 57,
            "reasoning": 29,
            "cache": { "read": 31105, "write": 0 }
        },
        "finish": "tool-calls"
    }"#;
    let mut bytes = json.as_bytes().to_vec();
    let mut msg: OpenCodeMessage =
        simd_json::from_slice(&mut bytes).expect("should parse data blob");

    // Inject DB columns.
    msg.id = "msg_kilo_test_id".to_string();
    msg.session_id = "ses_kilo_test_session".to_string();

    assert_eq!(msg.id, "msg_kilo_test_id");
    assert_eq!(msg.session_id, "ses_kilo_test_session");
    assert_eq!(msg.role, "assistant");
    assert_eq!(msg.model_name().unwrap(), "minimax/minimax-m2.5:free");
    assert_eq!(msg.cost, Some(0.00111015));

    let tokens = msg.tokens.as_ref().unwrap();
    assert_eq!(tokens.input, 362);
    assert_eq!(tokens.output, 57);
    assert_eq!(tokens.reasoning, 29);
    assert_eq!(tokens.cache.read, 31105);
    assert_eq!(tokens.cache.write, 0);
}

#[test]
fn test_parse_kilo_sqlite_user_data_blob() {
    let json = r#"{
        "role": "user",
        "time": { "created": 1771082996482 },
        "summary": { "title": "Codebase explanation", "diffs": [] },
        "agent": "code",
        "model": { "providerID": "kilo", "modelID": "minimax/minimax-m2.5:free" }
    }"#;
    let mut bytes = json.as_bytes().to_vec();
    let mut msg: OpenCodeMessage =
        simd_json::from_slice(&mut bytes).expect("should parse user data blob");

    msg.id = "msg_kilo_user".to_string();
    msg.session_id = "ses_kilo_test".to_string();

    assert_eq!(msg.role, "user");
    assert_eq!(msg.model_name().unwrap(), "minimax/minimax-m2.5:free");
    assert!(msg.tokens.is_none());
}

#[test]
fn test_parse_kilo_sqlite_minimal_data_blob() {
    // Minimal blob with just the role field.
    let json = r#"{ "role": "user" }"#;
    let mut bytes = json.as_bytes().to_vec();
    let msg: OpenCodeMessage =
        simd_json::from_slice(&mut bytes).expect("should parse minimal blob");
    assert_eq!(msg.role, "user");
    assert!(msg.id.is_empty());
    assert!(msg.session_id.is_empty());
}

// ===========================================================================
// Stats computation tests (with Kilo CLI identity)
// ===========================================================================

#[test]
fn test_kilo_compute_message_stats_assistant_with_cost() {
    let msg = OpenCodeMessage {
        role: "assistant".to_string(),
        cost: Some(0.00111015),
        tokens: Some(OpenCodeTokens {
            input: 362,
            output: 57,
            reasoning: 29,
            cache: OpenCodeCacheTokens {
                read: 31105,
                write: 0,
            },
        }),
        model_id: Some("minimax/minimax-m2.5:free".to_string()),
        ..Default::default()
    };
    let stats = compute_message_stats(&msg, Stats::default());
    assert_eq!(stats.input_tokens, 362);
    assert_eq!(stats.output_tokens, 57);
    assert_eq!(stats.reasoning_tokens, 29);
    assert_eq!(stats.cache_read_tokens, 31105);
    assert_eq!(stats.cache_creation_tokens, 0);
    assert_eq!(stats.cached_tokens, 31105); // read + write
    // Explicit cost wins.
    assert!((stats.cost - 0.00111015).abs() < f64::EPSILON);
    assert_eq!(stats.tool_calls, 1); // at least 1 for model call
}

#[test]
fn test_kilo_compute_message_stats_user() {
    let msg = OpenCodeMessage {
        role: "user".to_string(),
        ..Default::default()
    };
    let stats = compute_message_stats(&msg, Stats::default());
    assert_eq!(stats.input_tokens, 0);
    assert_eq!(stats.output_tokens, 0);
    assert_eq!(stats.cost, 0.0);
}

#[test]
fn test_kilo_compute_message_stats_preserves_tool_stats() {
    let msg = OpenCodeMessage {
        role: "assistant".to_string(),
        tokens: Some(OpenCodeTokens {
            input: 100,
            output: 50,
            ..Default::default()
        }),
        model_id: Some("test-model".to_string()),
        ..Default::default()
    };
    let tool_stats = Stats {
        tool_calls: 5,
        files_read: 3,
        ..Default::default()
    };

    let stats = compute_message_stats(&msg, tool_stats);
    assert_eq!(stats.tool_calls, 5);
    assert_eq!(stats.files_read, 3);
}

// ===========================================================================
// build_conversation_message tests (with Kilo CLI identity)
// ===========================================================================

#[test]
fn test_kilo_build_conversation_message_with_project() {
    let msg = OpenCodeMessage {
        id: "msg_kilo_123".to_string(),
        session_id: "ses_kilo_456".to_string(),
        role: "assistant".to_string(),
        time: OpenCodeMessageTime {
            created: Some(1771083156415),
            ..Default::default()
        },
        model_id: Some("minimax/minimax-m2.5:free".to_string()),
        ..Default::default()
    };

    let conv = build_conversation_message(
        msg,
        Some("Kilo Test Session".to_string()),
        Some("/code/tweakcc"),
        None,
        Stats::default(),
        Application::KiloCli,
        "kilo_cli",
    );

    assert_eq!(conv.application, Application::KiloCli);
    assert_eq!(conv.role, MessageRole::Assistant);
    assert_eq!(conv.session_name.as_deref(), Some("Kilo Test Session"));
    assert_eq!(conv.project_hash, hash_text("/code/tweakcc"));
    assert_eq!(conv.conversation_hash, hash_text("ses_kilo_456"));
    assert_eq!(
        conv.global_hash,
        hash_text("kilo_cli_ses_kilo_456_msg_kilo_123")
    );
    assert_eq!(conv.model.as_deref(), Some("minimax/minimax-m2.5:free"));
}

#[test]
fn test_kilo_build_conversation_message_fallback_project_hash() {
    let msg = OpenCodeMessage {
        id: "msg_a".to_string(),
        session_id: "ses_b".to_string(),
        role: "user".to_string(),
        ..Default::default()
    };

    let conv = build_conversation_message(
        msg,
        None,
        None,
        Some("ses_b"),
        Stats::default(),
        Application::KiloCli,
        "kilo_cli",
    );
    assert_eq!(conv.project_hash, hash_text("ses_b"));
}

// ===========================================================================
// global_hash tests — Kilo CLI uses "kilo_cli_" prefix, NOT "opencode_"
// ===========================================================================

#[test]
fn test_kilo_global_hash_uses_kilo_prefix() {
    let session_id = "ses_3a33896dbffeieFOLryTAxfy7D";
    let message_id = "msg_c5cc84bbf001a3bQs6VdR97IUK";

    let expected = hash_text(&format!("kilo_cli_{session_id}_{message_id}"));

    let msg = OpenCodeMessage {
        id: message_id.to_string(),
        session_id: session_id.to_string(),
        role: "assistant".to_string(),
        ..Default::default()
    };
    let conv = build_conversation_message(
        msg,
        None,
        None,
        None,
        Stats::default(),
        Application::KiloCli,
        "kilo_cli",
    );

    assert_eq!(conv.global_hash, expected);
}

#[test]
fn test_kilo_global_hash_differs_from_opencode() {
    // Verify that the same message parsed by Kilo CLI produces a different
    // global_hash than OpenCode, preventing cross-tool deduplication collision.
    let session_id = "ses_shared";
    let message_id = "msg_shared";

    let kilo_hash = hash_text(&format!("kilo_cli_{session_id}_{message_id}"));
    let opencode_hash = hash_text(&format!("opencode_{session_id}_{message_id}"));

    assert_ne!(kilo_hash, opencode_hash);
}

#[test]
fn test_kilo_global_hash_matches_json_and_sqlite_paths() {
    // The global_hash for a message must be identical whether parsed from
    // a JSON file or from the SQLite database, so deduplication works.
    let session_id = "ses_3a33896dbffeieFOLryTAxfy7D";
    let message_id = "msg_c5cc84bbf001a3bQs6VdR97IUK";

    let expected = hash_text(&format!("kilo_cli_{session_id}_{message_id}"));

    // Simulate JSON path.
    let json_msg = OpenCodeMessage {
        id: message_id.to_string(),
        session_id: session_id.to_string(),
        role: "assistant".to_string(),
        ..Default::default()
    };
    let json_conv = build_conversation_message(
        json_msg,
        None,
        None,
        None,
        Stats::default(),
        Application::KiloCli,
        "kilo_cli",
    );

    // Simulate SQLite path (id and session_id injected from DB columns).
    let mut sqlite_msg = OpenCodeMessage {
        role: "assistant".to_string(),
        ..Default::default()
    };
    sqlite_msg.id = message_id.to_string();
    sqlite_msg.session_id = session_id.to_string();
    let sqlite_conv = build_conversation_message(
        sqlite_msg,
        None,
        None,
        None,
        Stats::default(),
        Application::KiloCli,
        "kilo_cli",
    );

    assert_eq!(json_conv.global_hash, expected);
    assert_eq!(sqlite_conv.global_hash, expected);
    assert_eq!(json_conv.global_hash, sqlite_conv.global_hash);
}

// ===========================================================================
// In-memory SQLite integration tests
// ===========================================================================

/// Create an in-memory DB matching the Kilo CLI SQLite schema.
/// (Identical to OpenCode's schema — Kilo is a fork.)
fn create_kilo_test_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute_batch(
        "
        CREATE TABLE project (
            id TEXT PRIMARY KEY,
            worktree TEXT NOT NULL,
            vcs TEXT,
            name TEXT,
            icon_url TEXT,
            icon_color TEXT,
            time_created INTEGER NOT NULL,
            time_updated INTEGER NOT NULL,
            time_initialized INTEGER,
            sandboxes TEXT NOT NULL DEFAULT '[]',
            commands TEXT
        );

        CREATE TABLE workspace (
            id TEXT PRIMARY KEY,
            branch TEXT,
            project_id TEXT NOT NULL REFERENCES project(id) ON DELETE CASCADE,
            type TEXT NOT NULL,
            name TEXT,
            directory TEXT,
            extra TEXT
        );

        CREATE TABLE session (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL REFERENCES project(id) ON DELETE CASCADE,
            workspace_id TEXT,
            parent_id TEXT,
            slug TEXT NOT NULL DEFAULT '',
            directory TEXT NOT NULL,
            title TEXT NOT NULL,
            version TEXT NOT NULL DEFAULT '',
            share_url TEXT,
            summary_additions INTEGER,
            summary_deletions INTEGER,
            summary_files INTEGER,
            summary_diffs TEXT,
            revert TEXT,
            permission TEXT,
            time_created INTEGER NOT NULL,
            time_updated INTEGER NOT NULL,
            time_compacting INTEGER,
            time_archived INTEGER
        );

        CREATE TABLE message (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
            time_created INTEGER NOT NULL,
            time_updated INTEGER NOT NULL,
            data TEXT NOT NULL
        );

        CREATE TABLE part (
            id TEXT PRIMARY KEY,
            message_id TEXT NOT NULL REFERENCES message(id) ON DELETE CASCADE,
            session_id TEXT NOT NULL,
            time_created INTEGER NOT NULL,
            time_updated INTEGER NOT NULL,
            data TEXT NOT NULL
        );

        CREATE INDEX message_session_time_created_id_idx
            ON message (session_id, time_created, id);
        CREATE INDEX part_message_id_id_idx ON part (message_id, id);
        CREATE INDEX part_session_idx ON part (session_id);
        CREATE INDEX session_project_idx ON session (project_id);
        CREATE INDEX session_workspace_idx ON session (workspace_id);
        CREATE INDEX session_parent_idx ON session (parent_id);
        ",
    )
    .unwrap();

    conn
}

#[test]
fn test_kilo_load_projects_from_db() {
    let conn = create_kilo_test_db();
    conn.execute(
        "INSERT INTO project (id, worktree, vcs, time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params!["0b4651dc870efaaf627a2dadd5613224e4343b32", "/code/tweakcc", "git", 1771082700000i64, 1771082700000i64],
    )
    .unwrap();

    let projects = load_projects_from_db(&conn);
    assert_eq!(projects.len(), 1);
    assert_eq!(
        projects["0b4651dc870efaaf627a2dadd5613224e4343b32"].worktree,
        "/code/tweakcc"
    );
}

#[test]
fn test_kilo_load_sessions_from_db() {
    let conn = create_kilo_test_db();
    conn.execute(
        "INSERT INTO project (id, worktree, time_created, time_updated) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params!["proj_kilo", "/code/project", 0i64, 0i64],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO session (id, project_id, title, directory, time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params!["ses_kilo_1", "proj_kilo", "Kilo Session", "/code/project", 1771082700000i64, 1771082700000i64],
    )
    .unwrap();

    let sessions = load_sessions_from_db(&conn);
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions["ses_kilo_1"].title.as_deref(),
        Some("Kilo Session")
    );
    assert_eq!(sessions["ses_kilo_1"].project_id, "proj_kilo");
}

#[test]
fn test_kilo_batch_load_tool_stats_from_db() {
    let conn = create_kilo_test_db();

    // Insert project + session + message first (for FK constraints).
    conn.execute(
        "INSERT INTO project (id, worktree, time_created, time_updated) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params!["proj_1", "/tmp", 0i64, 0i64],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO session (id, project_id, title, directory, time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params!["ses_1", "proj_1", "s", "/tmp", 0i64, 0i64],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params!["msg_1", "ses_1", 0i64, 0i64, r#"{"role":"assistant"}"#],
    )
    .unwrap();

    // Insert tool parts (Kilo format — identical to OpenCode).
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            "prt_1", "msg_1", "ses_1", 0i64, 0i64,
            r#"{"type":"tool","tool":"read","callID":"call-uuid-1","state":{"status":"completed","input":{"filePath":"/code/tweakcc/src/index.tsx"},"output":"contents","title":"Read file","metadata":{},"time":{"start":0,"end":1}}}"#
        ],
    ).unwrap();
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            "prt_2", "msg_1", "ses_1", 0i64, 0i64,
            r#"{"type":"tool","tool":"glob","callID":"call-uuid-2","state":{"status":"completed","input":{},"output":"files","title":"Glob","metadata":{"count":5},"time":{"start":0,"end":1}}}"#
        ],
    ).unwrap();
    // Non-tool part (should be ignored).
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            "prt_3", "msg_1", "ses_1", 0i64, 0i64,
            r#"{"type":"text","text":"Hello from Kilo"}"#
        ],
    ).unwrap();

    let stats = batch_load_tool_stats_from_db(&conn);
    let msg_stats = &stats["msg_1"];
    assert_eq!(msg_stats.tool_calls, 2);
    assert_eq!(msg_stats.files_read, 6); // 1 from read + 5 from glob count
    assert_eq!(msg_stats.file_searches, 1);
}

#[test]
fn test_kilo_batch_load_tool_stats_empty_db() {
    let conn = create_kilo_test_db();
    let stats = batch_load_tool_stats_from_db(&conn);
    assert!(stats.is_empty());
}

#[test]
fn test_kilo_batch_load_step_finish_from_db() {
    let conn = create_kilo_test_db();

    // Scaffold project/session/message.
    conn.execute(
        "INSERT INTO project (id, worktree, time_created, time_updated) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params!["p1", "/tmp", 0i64, 0i64],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params!["s1", "p1", "slug", "/tmp", "t", "1.0", 0i64, 0i64],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params!["msg_sf", "s1", 0i64, 0i64, r#"{"role":"assistant"}"#],
    )
    .unwrap();

    // Two step-finish parts with token data.
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            "prt_sf1",
            "msg_sf",
            "s1",
            0i64,
            0i64,
            r#"{"type":"step-finish","reason":"tool-calls","cost":0.01,"tokens":{"total":5000,"input":1000,"output":500,"reasoning":100,"cache":{"read":3000,"write":400}}}"#
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            "prt_sf2",
            "msg_sf",
            "s1",
            0i64,
            0i64,
            r#"{"type":"step-finish","reason":"stop","cost":0.02,"tokens":{"total":3000,"input":800,"output":300,"reasoning":50,"cache":{"read":1500,"write":350}}}"#
        ],
    )
    .unwrap();

    let agg = batch_load_step_finish_from_db(&conn);
    let msg_agg = &agg["msg_sf"];
    assert_eq!(msg_agg.input, 1800); // 1000 + 800
    assert_eq!(msg_agg.output, 800); // 500 + 300
    assert_eq!(msg_agg.reasoning, 150); // 100 + 50
    assert_eq!(msg_agg.cache_read, 4500); // 3000 + 1500
    assert_eq!(msg_agg.cache_write, 750); // 400 + 350
    assert!((msg_agg.cost - 0.03).abs() < f64::EPSILON); // 0.01 + 0.02
}

// ===========================================================================
// Full end-to-end in-memory SQLite test
// ===========================================================================

#[test]
fn test_kilo_sqlite_end_to_end_in_memory() {
    // Build a full in-memory DB and verify message conversion with Kilo identity.
    let conn = create_kilo_test_db();

    conn.execute(
        "INSERT INTO project (id, worktree, vcs, time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params!["0b4651dc870efaaf627a2dadd5613224e4343b32", "/code/tweakcc", "git", 1771082700000i64, 1771082700000i64],
    ).unwrap();

    conn.execute(
        "INSERT INTO session (id, project_id, title, directory, time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            "ses_3a33896dbffeieFOLryTAxfy7D",
            "0b4651dc870efaaf627a2dadd5613224e4343b32",
            "Codebase explanation and overview",
            "/code/tweakcc",
            1771083098404i64,
            1771083200000i64,
        ],
    ).unwrap();

    // User message (data blob has no id/sessionID).
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            "msg_c5cc84bb4001bKJ6xO4CN6ri8O",
            "ses_3a33896dbffeieFOLryTAxfy7D",
            1771083156409i64,
            1771083156409i64,
            r#"{"role":"user","time":{"created":1771083156409},"summary":{"title":"Count correction: only 2 items","diffs":[]},"agent":"code","model":{"providerID":"kilo","modelID":"z-ai/glm-5:free"}}"#
        ],
    ).unwrap();

    // Assistant message with tokens and cost.
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            "msg_c5cc84bbf001a3bQs6VdR97IUK",
            "ses_3a33896dbffeieFOLryTAxfy7D",
            1771083156415i64,
            1771083174019i64,
            r#"{"role":"assistant","time":{"created":1771083156415,"completed":1771083174019},"parentID":"msg_c5cc84bb4001bKJ6xO4CN6ri8O","modelID":"z-ai/glm-5:free","providerID":"kilo","mode":"code","agent":"code","cost":0.017154,"tokens":{"total":57407,"input":1207,"output":1569,"reasoning":281,"cache":{"read":54631,"write":0}},"finish":"stop"}"#
        ],
    ).unwrap();

    // A tool part for the assistant message.
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            "prt_tool1",
            "msg_c5cc84bbf001a3bQs6VdR97IUK",
            "ses_3a33896dbffeieFOLryTAxfy7D",
            1771083160000i64,
            1771083161000i64,
            r#"{"type":"tool","tool":"read","callID":"call-uuid-1","state":{"status":"completed","input":{"filePath":"/code/tweakcc/src/index.tsx"},"output":"fn main(){}","title":"Read file","metadata":{},"time":{"start":1771083160000,"end":1771083161000}}}"#
        ],
    ).unwrap();

    // Query and convert using our helpers.
    let db_projects = load_projects_from_db(&conn);
    let db_sessions = load_sessions_from_db(&conn);
    let tool_stats_map = batch_load_tool_stats_from_db(&conn);

    assert_eq!(db_projects.len(), 1);
    assert_eq!(db_sessions.len(), 1);
    assert_eq!(tool_stats_map.len(), 1);

    // Parse messages.
    let mut stmt = conn
        .prepare("SELECT id, session_id, time_created, data FROM message ORDER BY time_created")
        .unwrap();
    let messages: Vec<crate::types::ConversationMessage> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0).unwrap(),
                row.get::<_, String>(1).unwrap(),
                row.get::<_, i64>(2).unwrap(),
                row.get::<_, String>(3).unwrap(),
            ))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .filter_map(|(id, session_id, time_created, data)| {
            let mut bytes = data.into_bytes();
            let mut msg = simd_json::from_slice::<OpenCodeMessage>(&mut bytes).ok()?;
            msg.id = id.clone();
            msg.session_id = session_id.clone();
            if msg.time.created.is_none() || msg.time.created == Some(0) {
                msg.time.created = Some(time_created);
            }

            let session = db_sessions.get(&session_id);
            let project = session.and_then(|s| db_projects.get(&s.project_id));
            let tool_stats = tool_stats_map.get(&id).cloned().unwrap_or_default();
            let stats = compute_message_stats(&msg, tool_stats);
            let session_title = session.and_then(|s| s.title.clone());
            let worktree = project.map(|p| p.worktree.as_str());
            let fallback = Some(session_id.as_str());

            Some(build_conversation_message(
                msg,
                session_title,
                worktree,
                fallback,
                stats,
                Application::KiloCli,
                "kilo_cli",
            ))
        })
        .collect();

    assert_eq!(messages.len(), 2);

    // Verify user message.
    let user_msg = &messages[0];
    assert_eq!(user_msg.role, MessageRole::User);
    assert_eq!(user_msg.application, Application::KiloCli);
    assert_eq!(
        user_msg.session_name.as_deref(),
        Some("Codebase explanation and overview")
    );
    assert_eq!(user_msg.project_hash, hash_text("/code/tweakcc"));
    assert_eq!(user_msg.stats.input_tokens, 0);

    // Verify assistant message.
    let asst_msg = &messages[1];
    assert_eq!(asst_msg.role, MessageRole::Assistant);
    assert_eq!(asst_msg.application, Application::KiloCli);
    assert_eq!(asst_msg.model.as_deref(), Some("z-ai/glm-5:free"));
    assert_eq!(asst_msg.stats.input_tokens, 1207);
    assert_eq!(asst_msg.stats.output_tokens, 1569);
    assert_eq!(asst_msg.stats.reasoning_tokens, 281);
    assert_eq!(asst_msg.stats.cache_read_tokens, 54631);
    assert_eq!(asst_msg.stats.cache_creation_tokens, 0);
    assert!((asst_msg.stats.cost - 0.017154).abs() < f64::EPSILON);
    // 1 tool call from the "read" part.
    assert_eq!(asst_msg.stats.tool_calls, 1);
    assert_eq!(asst_msg.stats.files_read, 1);

    // Verify global hash uses Kilo CLI prefix.
    assert_eq!(
        asst_msg.global_hash,
        hash_text("kilo_cli_ses_3a33896dbffeieFOLryTAxfy7D_msg_c5cc84bbf001a3bQs6VdR97IUK")
    );
}

#[test]
fn test_kilo_step_finish_fallback_when_message_has_zero_tokens() {
    let conn = create_kilo_test_db();

    conn.execute(
        "INSERT INTO project (id, worktree, time_created, time_updated) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params!["p1", "/code/test", 0i64, 0i64],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params!["s1", "p1", "slug", "/code/test", "t", "1.0", 0i64, 0i64],
    )
    .unwrap();

    // Assistant message with zero tokens at message level.
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            "msg_z",
            "s1",
            1770000100000i64,
            1770000110000i64,
            r#"{"role":"assistant","time":{"created":1770000100000,"completed":1770000110000},"modelID":"z-ai/glm-5:free","providerID":"kilo","cost":0,"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"finish":"stop"}"#
        ],
    )
    .unwrap();

    // Step-finish part with actual token data.
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            "prt_sf",
            "msg_z",
            "s1",
            0i64,
            0i64,
            r#"{"type":"step-finish","reason":"stop","cost":0.05,"tokens":{"total":10000,"input":5000,"output":3000,"reasoning":500,"cache":{"read":1000,"write":500}}}"#
        ],
    )
    .unwrap();

    // Query and convert using our helpers — simulating parse_sqlite_messages logic.
    let db_projects = load_projects_from_db(&conn);
    let db_sessions = load_sessions_from_db(&conn);
    let tool_stats_map = batch_load_tool_stats_from_db(&conn);
    let step_finish_map = batch_load_step_finish_from_db(&conn);

    let mut stmt = conn
        .prepare("SELECT id, session_id, time_created, data FROM message")
        .unwrap();

    let messages: Vec<crate::types::ConversationMessage> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0).unwrap(),
                row.get::<_, String>(1).unwrap(),
                row.get::<_, i64>(2).unwrap(),
                row.get::<_, String>(3).unwrap(),
            ))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .filter_map(|(id, session_id, _time_created, data)| {
            let mut bytes = data.into_bytes();
            let mut msg = simd_json::from_slice::<OpenCodeMessage>(&mut bytes).ok()?;
            msg.id = id.clone();
            msg.session_id = session_id.clone();

            // Apply step-finish fallback (same logic as parse_sqlite_messages).
            if msg.role == "assistant" {
                let msg_has_tokens = msg.tokens.as_ref().is_some_and(|t| {
                    t.input > 0
                        || t.output > 0
                        || t.reasoning > 0
                        || t.cache.read > 0
                        || t.cache.write > 0
                });
                if !msg_has_tokens
                    && let Some(agg) = step_finish_map.get(&id)
                    && (agg.input > 0
                        || agg.output > 0
                        || agg.reasoning > 0
                        || agg.cache_read > 0
                        || agg.cache_write > 0)
                {
                    msg.tokens = Some(OpenCodeTokens {
                        input: agg.input,
                        output: agg.output,
                        reasoning: agg.reasoning,
                        cache: OpenCodeCacheTokens {
                            read: agg.cache_read,
                            write: agg.cache_write,
                        },
                    });
                    if agg.cost > 0.0 && msg.cost.is_none_or(|c| c == 0.0) {
                        msg.cost = Some(agg.cost);
                    }
                }
            }

            let session = db_sessions.get(&session_id);
            let project = session.and_then(|s| db_projects.get(&s.project_id));
            let tool_stats = tool_stats_map.get(&id).cloned().unwrap_or_default();
            let stats = compute_message_stats(&msg, tool_stats);
            let session_title = session.and_then(|s| s.title.clone());
            let worktree = project.map(|p| p.worktree.as_str());
            let fallback = Some(session_id.as_str());

            Some(build_conversation_message(
                msg,
                session_title,
                worktree,
                fallback,
                stats,
                Application::KiloCli,
                "kilo_cli",
            ))
        })
        .collect();

    assert_eq!(messages.len(), 1);
    let msg = &messages[0];
    // Tokens should come from step-finish fallback.
    assert_eq!(msg.stats.input_tokens, 5000);
    assert_eq!(msg.stats.output_tokens, 3000);
    assert_eq!(msg.stats.reasoning_tokens, 500);
    assert_eq!(msg.stats.cache_read_tokens, 1000);
    assert_eq!(msg.stats.cache_creation_tokens, 500);
    assert!((msg.stats.cost - 0.05).abs() < f64::EPSILON);
}

// ===========================================================================
// is_valid_data_path tests
// ===========================================================================

#[test]
fn test_kilo_is_valid_data_path_db_files() {
    let analyzer = KiloCliAnalyzer::new();

    // Main DB file should be accepted.
    let tmp = std::env::temp_dir().join("kilo.db");
    std::fs::write(&tmp, "fake").unwrap();
    assert!(analyzer.is_valid_data_path(&tmp));
    std::fs::remove_file(&tmp).unwrap();

    // Channel-specific DB files should be accepted.
    let tmp2 = std::env::temp_dir().join("kilo-canary.db");
    std::fs::write(&tmp2, "fake").unwrap();
    assert!(analyzer.is_valid_data_path(&tmp2));
    std::fs::remove_file(&tmp2).unwrap();

    // Reject WAL/SHM journal files.
    let tmp3 = std::env::temp_dir().join("kilo.db-wal");
    std::fs::write(&tmp3, "fake").unwrap();
    assert!(!analyzer.is_valid_data_path(&tmp3));
    std::fs::remove_file(&tmp3).unwrap();
}

// ===========================================================================
// Glob patterns test
// ===========================================================================

#[test]
fn test_kilo_glob_patterns_include_sqlite() {
    let analyzer = KiloCliAnalyzer::new();
    let patterns = analyzer.get_data_glob_patterns();

    // Should include both JSON and SQLite patterns.
    let has_json = patterns.iter().any(|p| p.contains("storage/message"));
    let has_db = patterns.iter().any(|p| p.contains("kilo.db"));
    let has_channel_db = patterns.iter().any(|p| p.contains("kilo-*.db"));

    assert!(has_json, "should have JSON glob pattern");
    assert!(has_db, "should have SQLite glob pattern");
    assert!(
        has_channel_db,
        "should have channel-specific SQLite glob pattern"
    );
}
