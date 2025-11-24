use super::*;
use crate::types::{ConversationMessage, MessageRole, Stats};
use chrono::{TimeZone, Utc};

#[test]
fn test_format_number_comma() {
    let options = NumberFormatOptions {
        use_comma: true,
        use_human: false,
        locale: "en".to_string(),
        decimal_places: 2,
    };

    assert_eq!(format_number(1000, &options), "1,000");
    assert_eq!(format_number(1000000, &options), "1,000,000");
    assert_eq!(format_number(123, &options), "123");
}

#[test]
fn test_format_number_human() {
    let options = NumberFormatOptions {
        use_comma: false,
        use_human: true,
        locale: "en".to_string(),
        decimal_places: 1,
    };

    assert_eq!(format_number(100, &options), "100");
    assert_eq!(format_number(1500, &options), "1.5k");
    assert_eq!(format_number(1_500_000, &options), "1.5m");
    assert_eq!(format_number(1_500_000_000, &options), "1.5b");
    assert_eq!(format_number(1_500_000_000_000, &options), "1.5t");
}

#[test]
fn test_format_number_plain() {
    let options = NumberFormatOptions {
        use_comma: false,
        use_human: false,
        locale: "en".to_string(),
        decimal_places: 2,
    };

    assert_eq!(format_number(1000, &options), "1000");
}

#[test]
fn test_format_date_for_display() {
    assert_eq!(format_date_for_display("unknown"), "Unknown");
    assert_eq!(format_date_for_display("invalid"), "invalid");
    
    // Test a specific past date
    assert_eq!(format_date_for_display("2023-01-15"), "1/15/2023");
    
    // Test today's date (dynamic)
    let today = chrono::Local::now().date_naive();
    let today_str = today.format("%Y-%m-%d").to_string();
    let expected = format!("{}/{}/{}*", today.month(), today.day(), today.year());
    assert_eq!(format_date_for_display(&today_str), expected);
}

#[test]
fn test_hash_text() {
    let text = "hello world";
    let hash = hash_text(text);
    assert_eq!(hash.len(), 64); // SHA256 hex string length
    assert_eq!(
        hash,
        "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
    );
}

#[tokio::test]
async fn test_get_messages_later_than() {
    let date_base = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
    let date_before = Utc.with_ymd_and_hms(2024, 12, 31, 23, 59, 59).unwrap();
    let date_after = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 1).unwrap();

    let msg_before = ConversationMessage {
        date: date_before,
        application: crate::types::Application::ClaudeCode,
        project_hash: "p".to_string(),
        conversation_hash: "c1".to_string(),
        local_hash: None,
        global_hash: "g1".to_string(),
        model: None,
        stats: Stats::default(),
        role: MessageRole::User,
        uuid: None,
        session_name: None,
    };

    let msg_after = ConversationMessage {
        date: date_after,
        conversation_hash: "c2".to_string(),
        global_hash: "g2".to_string(),
        ..msg_before.clone()
    };

    let messages = vec![msg_before, msg_after];
    let threshold = date_base.timestamp_millis();

    let result = get_messages_later_than(threshold, messages).await.unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].conversation_hash, "c2");
}

#[test]
fn test_aggregate_by_date_basic() {
    let date = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();
    let local_date_str = date.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string();

    let msg = ConversationMessage {
        date,
        application: crate::types::Application::ClaudeCode,
        project_hash: "p".to_string(),
        conversation_hash: "c1".to_string(),
        local_hash: None,
        global_hash: "g1".to_string(),
        model: Some("claude-3".to_string()),
        stats: Stats {
            input_tokens: 100,
            cost: 0.01,
            ..Stats::default()
        },
        role: MessageRole::Assistant,
        uuid: None,
        session_name: None,
    };

    let result = aggregate_by_date(&[msg]);
    
    assert!(result.contains_key(&local_date_str));
    let stats = &result[&local_date_str];
    assert_eq!(stats.ai_messages, 1);
    assert_eq!(stats.conversations, 1);
    assert_eq!(stats.stats.input_tokens, 100);
    assert_eq!(stats.stats.cost, 0.01);
}

#[test]
fn test_aggregate_by_date_gap_filling() {
    // Create messages 2 days apart
    let date1 = Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap();
    let date3 = Utc.with_ymd_and_hms(2025, 1, 3, 12, 0, 0).unwrap();

    let msg1 = ConversationMessage {
        date: date1,
        application: crate::types::Application::ClaudeCode,
        project_hash: "p".to_string(),
        conversation_hash: "c1".to_string(),
        local_hash: None,
        global_hash: "g1".to_string(),
        model: Some("model".to_string()),
        stats: Stats::default(),
        role: MessageRole::Assistant,
        uuid: None,
        session_name: None,
    };

    let msg3 = ConversationMessage {
        date: date3,
        conversation_hash: "c2".to_string(),
        global_hash: "g2".to_string(),
        ..msg1.clone()
    };

    let result = aggregate_by_date(&[msg1, msg3]);

    let date1_str = date1.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string();
    let date2_str = (date1 + chrono::Duration::days(1)).with_timezone(&chrono::Local).format("%Y-%m-%d").to_string();
    let date3_str = date3.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string();

    assert!(result.contains_key(&date1_str));
    assert!(result.contains_key(&date2_str)); // The gap should be filled
    assert!(result.contains_key(&date3_str));

    assert_eq!(result[&date1_str].ai_messages, 1);
    assert_eq!(result[&date2_str].ai_messages, 0); // Empty stats for gap
    assert_eq!(result[&date3_str].ai_messages, 1);
}
