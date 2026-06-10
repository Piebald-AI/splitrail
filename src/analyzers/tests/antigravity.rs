use chrono::{TimeZone, Utc};
use rusqlite::Connection;
use tempfile::NamedTempFile;

use crate::analyzer::{Analyzer, DataSource};
use crate::analyzers::antigravity::AntigravityCliAnalyzer;
use crate::types::MessageRole;

fn encode_varint(mut val: u64, out: &mut Vec<u8>) {
    loop {
        let mut b = (val & 0x7F) as u8;
        val >>= 7;
        if val != 0 {
            b |= 0x80;
            out.push(b);
        } else {
            out.push(b);
            break;
        }
    }
}

fn encode_proto_string(field_number: u32, s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let tag = (field_number << 3) | 2;
    encode_varint(tag as u64, &mut out);
    encode_varint(s.len() as u64, &mut out);
    out.extend_from_slice(s.as_bytes());
    out
}

fn encode_proto_timestamp(field_number: u32, seconds: i64, nanos: u32) -> Vec<u8> {
    let mut inner = Vec::new();
    // Field 1, wire type 0 (Varint)
    encode_varint(1 << 3, &mut inner);
    encode_varint(seconds as u64, &mut inner);
    // Field 2, wire type 0 (Varint)
    encode_varint(2 << 3, &mut inner);
    encode_varint(nanos as u64, &mut inner);

    let mut out = Vec::new();
    encode_varint(((field_number << 3) | 2) as u64, &mut out);
    encode_varint(inner.len() as u64, &mut out);
    out.extend(inner);
    out
}

#[test]
fn test_antigravity_cli_parse_source() {
    let temp_file = NamedTempFile::new().unwrap();
    let conn = Connection::open(temp_file.path()).unwrap();

    conn.execute(
        "CREATE TABLE steps (idx INTEGER, step_type INTEGER, step_payload BLOB)",
        [],
    )
    .unwrap();

    // Prepare step 1 (User message)
    // Field 3: user text, Field 4: nested timestamp
    let mut payload1 = Vec::new();
    payload1.extend(encode_proto_string(3, "How does splitrail work?"));
    payload1.extend(encode_proto_timestamp(4, 1779246143, 0));

    // Prepare step 2 (Assistant message)
    // Field 3: assistant response text, Field 4: nested timestamp
    let mut payload2 = Vec::new();
    payload2.extend(encode_proto_string(3, "Splitrail parses local databases."));
    payload2.extend(encode_proto_timestamp(4, 1779246150, 0));

    conn.execute(
        "INSERT INTO steps (idx, step_type, step_payload) VALUES (?, ?, ?)",
        rusqlite::params![0, 14, payload1],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO steps (idx, step_type, step_payload) VALUES (?, ?, ?)",
        rusqlite::params![1, 15, payload2],
    )
    .unwrap();

    // Parse using AntigravityCliAnalyzer
    let analyzer = AntigravityCliAnalyzer::new();
    let source = DataSource {
        path: temp_file.path().to_path_buf(),
    };
    let messages = analyzer.parse_source(&source).unwrap();

    assert_eq!(messages.len(), 2);

    // Verify User Message
    let user_msg = &messages[0];
    assert_eq!(user_msg.role, MessageRole::User);
    assert_eq!(user_msg.date, Utc.timestamp_opt(1779246143, 0).unwrap());
    assert_eq!(user_msg.model, None);
    assert_eq!(user_msg.stats.output_tokens, 0);
    assert!(user_msg.session_name.is_some());

    // Verify Assistant Message
    let assistant_msg = &messages[1];
    assert_eq!(assistant_msg.role, MessageRole::Assistant);
    assert_eq!(
        assistant_msg.date,
        Utc.timestamp_opt(1779246150, 0).unwrap()
    );
    assert_eq!(assistant_msg.model, Some("gemini-2.5-flash".to_string()));
    assert!(assistant_msg.stats.output_tokens > 0);
    assert!(assistant_msg.stats.cost > 0.0);
    assert!(assistant_msg.session_name.is_some());
}
