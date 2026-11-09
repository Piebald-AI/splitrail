use super::*;
use crate::types::{Application, MessageRole, ToolCall};

const BASE: i64 = 1_780_000_000;

fn at(offset_secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(BASE + offset_secs, 0).expect("valid timestamp")
}

fn message(
    offset_secs: i64,
    role: MessageRole,
    id: &str,
    session: &str,
    model: Option<&str>,
    stats: Stats,
) -> ConversationMessage {
    ConversationMessage {
        application: Application::CodexCli,
        date: at(offset_secs),
        project_hash: "project-hash".to_string(),
        project_path: None,
        conversation_hash: session.to_string(),
        local_hash: None,
        global_hash: format!("hash-{id}"),
        model: model.map(str::to_string),
        stats,
        role,
        is_tool_result: false,
        uuid: None,
        session_name: None,
    }
}

fn usage(input: u64, output: u64, cost: f64) -> Stats {
    Stats {
        input_tokens: input,
        output_tokens: output,
        cost,
        ..Stats::default()
    }
}

fn attr<'a>(span: &'a Span, key: &str) -> Option<&'a crate::otel::encode::AnyValue> {
    span.attributes
        .iter()
        .find(|attribute| attribute.key == key)
        .map(|attribute| &attribute.value)
}

fn attr_str_value<'a>(span: &'a Span, key: &str) -> Option<&'a str> {
    match attr(span, key) {
        Some(crate::otel::encode::AnyValue::StringValue(value)) => Some(value.as_str()),
        _ => None,
    }
}

fn attr_int_value(span: &Span, key: &str) -> Option<u64> {
    match attr(span, key) {
        Some(crate::otel::encode::AnyValue::IntValue(value)) => value.parse().ok(),
        _ => None,
    }
}

#[test]
fn one_prompt_and_two_replies_make_one_trace() {
    let messages = vec![
        message(0, MessageRole::User, "u1", "conv", None, Stats::default()),
        message(
            5,
            MessageRole::Assistant,
            "a1",
            "conv",
            Some("gpt-5.6-codex"),
            usage(100, 10, 0.01),
        ),
        message(
            20,
            MessageRole::Assistant,
            "a2",
            "conv",
            Some("gpt-5.6-codex"),
            usage(200, 20, 0.02),
        ),
    ];

    let turns = build_turns("Codex CLI", &messages, &[], false);
    assert_eq!(turns.len(), 1);

    let turn = &turns[0];
    assert_eq!(turn.spans.len(), 3);

    let agent = &turn.spans[0];
    assert_eq!(agent.name, "invoke_agent Codex CLI");
    assert_eq!(agent.parent_span_id, None);
    assert_eq!(agent.trace_id, turn.trace_id);
    assert_eq!(
        attr_str_value(agent, "gen_ai.operation.name"),
        Some("invoke_agent")
    );
    assert_eq!(
        attr_str_value(agent, "gen_ai.agent.name"),
        Some("Codex CLI")
    );
    assert_eq!(
        attr_int_value(agent, "gen_ai.usage.input_tokens"),
        Some(300)
    );

    for chat in &turn.spans[1..] {
        assert_eq!(chat.trace_id, turn.trace_id);
        assert_eq!(chat.parent_span_id.as_deref(), Some(agent.span_id.as_str()));
        assert_eq!(attr_str_value(chat, "gen_ai.operation.name"), Some("chat"));
        assert_eq!(attr_str_value(chat, "gen_ai.provider.name"), Some("openai"));
        assert_eq!(attr_str_value(chat, "splitrail.timing"), Some("derived"));
    }

    assert_eq!(turn.spans[1].name, "chat gpt-5.6-codex");
    assert_eq!(
        attr_int_value(&turn.spans[1], "gen_ai.usage.input_tokens"),
        Some(100)
    );
    assert_eq!(
        attr_int_value(&turn.spans[2], "gen_ai.usage.output_tokens"),
        Some(20)
    );
}

#[test]
fn chat_spans_are_children_of_the_turn_and_start_at_the_reply() {
    let messages = vec![
        message(0, MessageRole::User, "u1", "conv", None, Stats::default()),
        message(
            7,
            MessageRole::Assistant,
            "a1",
            "conv",
            Some("claude-sonnet-5"),
            usage(10, 1, 0.0),
        ),
    ];
    let turns = build_turns("Claude Code", &messages, &[], false);
    let chat = &turns[0].spans[1];

    assert_eq!(
        chat.start_time_unix_nano,
        crate::otel::encode::nanos(&at(7)).unwrap()
    );
    // The session ends with the reply, so the derived duration is the minimum.
    assert_eq!(
        chat.end_time_unix_nano,
        crate::otel::encode::nanos(&at(7).checked_add_signed(Duration::milliseconds(1)).unwrap())
            .unwrap()
    );
    assert_eq!(
        attr_str_value(chat, "gen_ai.provider.name"),
        Some("anthropic")
    );
}

#[test]
fn derived_duration_follows_the_next_message_and_is_clamped() {
    let messages = vec![
        message(0, MessageRole::User, "u1", "conv", None, Stats::default()),
        message(
            10,
            MessageRole::Assistant,
            "a1",
            "conv",
            Some("gpt-4o"),
            usage(10, 1, 0.0),
        ),
        // Ten minutes later: user stepped away, not a 10-minute request.
        message(
            610,
            MessageRole::Assistant,
            "a2",
            "conv",
            Some("gpt-4o"),
            usage(10, 1, 0.0),
        ),
    ];
    let turns = build_turns("Codex CLI", &messages, &[], false);
    let first = &turns[0].spans[1];
    let start = at(10);
    let end = start
        .checked_add_signed(Duration::milliseconds(MAX_DERIVED_CHAT_MS))
        .unwrap();
    assert_eq!(
        first.end_time_unix_nano,
        crate::otel::encode::nanos(&end).unwrap()
    );
}

#[test]
fn prompt_without_reply_produces_no_trace() {
    let messages = vec![message(
        0,
        MessageRole::User,
        "u1",
        "conv",
        None,
        Stats::default(),
    )];
    assert!(build_turns("Codex CLI", &messages, &[], false).is_empty());
}

#[test]
fn assistant_without_a_prompt_is_marked_as_transcript_sourced() {
    let messages = vec![message(
        0,
        MessageRole::Assistant,
        "a1",
        "conv",
        Some("gpt-4o"),
        usage(5, 5, 0.0),
    )];
    let turns = build_turns("Codex CLI", &messages, &[], false);
    assert_eq!(turns.len(), 1);
    assert_eq!(
        attr_str_value(&turns[0].spans[0], "splitrail.turn.origin"),
        Some("transcript")
    );
}

#[test]
fn zero_usage_synthetic_records_do_not_become_spans() {
    let messages = vec![
        message(0, MessageRole::User, "u1", "conv", None, Stats::default()),
        message(
            1,
            MessageRole::Assistant,
            "synthetic",
            "conv",
            None,
            Stats::default(),
        ),
        message(
            2,
            MessageRole::Assistant,
            "a1",
            "conv",
            Some("gpt-4o"),
            usage(1, 1, 0.0),
        ),
    ];
    let turns = build_turns("Codex CLI", &messages, &[], false);
    assert_eq!(turns[0].spans.len(), 2, "only one chat span is expected");
}

#[test]
fn conversations_do_not_mix_into_one_trace() {
    let messages = vec![
        message(0, MessageRole::User, "u1", "conv-a", None, Stats::default()),
        message(
            1,
            MessageRole::Assistant,
            "a1",
            "conv-a",
            Some("gpt-4o"),
            usage(1, 1, 0.0),
        ),
        message(2, MessageRole::User, "u2", "conv-b", None, Stats::default()),
        message(
            3,
            MessageRole::Assistant,
            "a2",
            "conv-b",
            Some("gpt-4o"),
            usage(1, 1, 0.0),
        ),
    ];
    let turns = build_turns("Codex CLI", &messages, &[], false);
    assert_eq!(turns.len(), 2);
    assert_ne!(turns[0].trace_id, turns[1].trace_id);
}

#[test]
fn ordering_is_stable_regardless_of_input_order() {
    let mut messages = vec![
        message(0, MessageRole::User, "u1", "conv", None, Stats::default()),
        message(
            5,
            MessageRole::Assistant,
            "a1",
            "conv",
            Some("gpt-4o"),
            usage(1, 1, 0.0),
        ),
        message(
            9,
            MessageRole::Assistant,
            "a2",
            "conv",
            Some("gpt-4o"),
            usage(1, 1, 0.0),
        ),
    ];
    let forward = build_turns("Codex CLI", &messages, &[], false);
    messages.reverse();
    let reversed = build_turns("Codex CLI", &messages, &[], false);

    let ids = |turns: &[TurnSpans]| -> Vec<String> {
        turns[0]
            .spans
            .iter()
            .map(|span| span.span_id.clone())
            .collect()
    };
    assert_eq!(ids(&forward), ids(&reversed));
}

#[test]
fn multiple_prompts_in_one_session_make_multiple_traces() {
    let messages = vec![
        message(0, MessageRole::User, "u1", "conv", None, Stats::default()),
        message(
            1,
            MessageRole::Assistant,
            "a1",
            "conv",
            Some("gpt-4o"),
            usage(1, 1, 0.0),
        ),
        message(2, MessageRole::User, "u2", "conv", None, Stats::default()),
        message(
            3,
            MessageRole::Assistant,
            "a2",
            "conv",
            Some("gpt-4o"),
            usage(1, 1, 0.0),
        ),
    ];
    let turns = build_turns("Codex CLI", &messages, &[], false);
    assert_eq!(turns.len(), 2);
    assert_ne!(turns[0].trace_id, turns[1].trace_id);
    assert_eq!(turns[0].spans.len(), 2);
    assert_eq!(turns[1].spans.len(), 2);
}

#[test]
fn provider_mapping_covers_the_registry_values() {
    assert_eq!(provider_for_model(Some("claude-opus-5")), Some("anthropic"));
    assert_eq!(provider_for_model(Some("gpt-5.6-codex")), Some("openai"));
    assert_eq!(
        provider_for_model(Some("gemini-4-argon")),
        Some("gcp.gen_ai")
    );
    assert_eq!(provider_for_model(Some("grok-4.7")), Some("xai"));
    assert_eq!(provider_for_model(Some("deepseek-v4")), Some("deepseek"));
    assert_eq!(provider_for_model(Some("qwen3.7-plus")), Some("alibaba"));
    assert_eq!(provider_for_model(Some("some-local-model")), None);
    assert_eq!(provider_for_model(None), None);
}

#[test]
fn cache_token_attributes_use_semconv_names() {
    let message =
        conversation_message_with(MessageStats::new().cached(1000).written(200).reasoning(50));
    let turns = build_turns("Claude Code", &[message], &[], false);
    let chat = &turns[0].spans[1];
    assert_eq!(
        attr_int_value(chat, "gen_ai.usage.cache_read.input_tokens"),
        Some(1000)
    );
    assert_eq!(
        attr_int_value(chat, "gen_ai.usage.cache_write.input_tokens"),
        Some(200)
    );
    assert_eq!(
        attr_int_value(chat, "splitrail.usage.reasoning_tokens"),
        Some(50)
    );
}

struct MessageStats {
    stats: Stats,
}

impl MessageStats {
    fn new() -> Self {
        Self {
            stats: Stats::default(),
        }
    }
    fn cached(mut self, tokens: u64) -> Self {
        self.stats.cache_read_tokens = tokens;
        self
    }
    fn written(mut self, tokens: u64) -> Self {
        self.stats.cache_creation_tokens = tokens;
        self
    }
    fn reasoning(mut self, tokens: u64) -> Self {
        self.stats.reasoning_tokens = tokens;
        self
    }
}

fn conversation_message_with(stats: MessageStats) -> ConversationMessage {
    message(
        0,
        MessageRole::Assistant,
        "a1",
        "conv",
        Some("claude-opus-5"),
        stats.stats,
    )
}

fn tool_call(offset_secs: i64, name: &str, call_id: &str, session: &str) -> ToolCall {
    ToolCall {
        conversation_hash: session.to_string(),
        timestamp: at(offset_secs),
        name: name.to_string(),
        call_id: Some(call_id.to_string()),
        duration_ms: None,
    }
}

#[test]
fn tool_spans_are_children_of_the_turn() {
    let messages = vec![
        message(0, MessageRole::User, "u1", "conv", None, Stats::default()),
        message(
            5,
            MessageRole::Assistant,
            "a1",
            "conv",
            Some("claude-opus-5"),
            usage(10, 1, 0.01),
        ),
    ];
    let calls = vec![tool_call(6, "Bash", "toolu_1", "conv")];

    let turns = build_turns("Claude Code", &messages, &calls, true);
    assert_eq!(turns.len(), 1);
    let spans = &turns[0].spans;
    assert_eq!(spans.len(), 3, "agent, chat, and one tool span");

    let tool = &spans[2];
    assert_eq!(tool.name, "execute_tool Bash");
    assert_eq!(
        tool.parent_span_id.as_deref(),
        Some(spans[0].span_id.as_str())
    );
    assert_eq!(tool.trace_id, turns[0].trace_id);
    assert_eq!(
        attr_str_value(tool, "gen_ai.operation.name"),
        Some("execute_tool")
    );
    assert_eq!(attr_str_value(tool, "gen_ai.tool.name"), Some("Bash"));
    assert_eq!(attr_str_value(tool, "gen_ai.tool.call.id"), Some("toolu_1"));
    assert_eq!(attr_str_value(tool, "splitrail.timing"), Some("derived"));
}

#[test]
fn tool_spans_are_absent_unless_requested() {
    let messages = vec![
        message(0, MessageRole::User, "u1", "conv", None, Stats::default()),
        message(
            5,
            MessageRole::Assistant,
            "a1",
            "conv",
            Some("claude-opus-5"),
            usage(10, 1, 0.01),
        ),
    ];
    let calls = vec![tool_call(6, "Bash", "toolu_1", "conv")];
    let turns = build_turns("Claude Code", &messages, &calls, false);
    assert_eq!(turns[0].spans.len(), 2);
    assert!(
        turns[0]
            .spans
            .iter()
            .all(|span| !span.name.starts_with("execute_tool"))
    );
}

#[test]
fn recorded_tool_duration_is_used_verbatim() {
    let messages = vec![
        message(0, MessageRole::User, "u1", "conv", None, Stats::default()),
        message(
            5,
            MessageRole::Assistant,
            "a1",
            "conv",
            Some("claude-opus-5"),
            usage(10, 1, 0.01),
        ),
    ];
    let mut call = tool_call(6, "Bash", "toolu_1", "conv");
    call.duration_ms = Some(2500);

    let turns = build_turns("Claude Code", &messages, &[call], true);
    let tool = &turns[0].spans[2];
    assert_eq!(attr_str_value(tool, "splitrail.timing"), Some("recorded"));
    assert_eq!(
        tool.start_time_unix_nano,
        crate::otel::encode::nanos(&at(6)).unwrap()
    );
    assert_eq!(
        tool.end_time_unix_nano,
        crate::otel::encode::nanos(
            &at(6)
                .checked_add_signed(Duration::milliseconds(2500))
                .unwrap()
        )
        .unwrap()
    );
}

#[test]
fn tool_calls_attach_to_the_turn_that_preceded_them() {
    let messages = vec![
        message(0, MessageRole::User, "u1", "conv", None, Stats::default()),
        message(
            1,
            MessageRole::Assistant,
            "a1",
            "conv",
            Some("claude-opus-5"),
            usage(1, 1, 0.0),
        ),
        message(10, MessageRole::User, "u2", "conv", None, Stats::default()),
        message(
            11,
            MessageRole::Assistant,
            "a2",
            "conv",
            Some("claude-opus-5"),
            usage(1, 1, 0.0),
        ),
    ];
    let calls = vec![
        tool_call(5, "Read", "toolu_1", "conv"),
        tool_call(15, "Edit", "toolu_2", "conv"),
    ];

    let turns = build_turns("Claude Code", &messages, &calls, true);
    assert_eq!(turns.len(), 2);

    let names = |index: usize| -> Vec<String> {
        turns[index]
            .spans
            .iter()
            .map(|span| span.name.clone())
            .collect()
    };
    assert!(names(0).contains(&"execute_tool Read".to_string()));
    assert!(!names(0).contains(&"execute_tool Edit".to_string()));
    assert!(names(1).contains(&"execute_tool Edit".to_string()));
}

#[test]
fn tool_only_messages_do_not_become_chat_spans() {
    // Codex represents each tool call as a zero-token message; a chat span for
    // it would be a request that never happened.
    let tool_only = Stats {
        tool_calls: 1,
        ..Stats::default()
    };
    let messages = vec![
        message(0, MessageRole::User, "u1", "conv", None, Stats::default()),
        message(
            1,
            MessageRole::Assistant,
            "tool",
            "conv",
            Some("gpt-5.6-codex"),
            tool_only,
        ),
        message(
            2,
            MessageRole::Assistant,
            "a1",
            "conv",
            Some("gpt-5.6-codex"),
            usage(5, 5, 0.01),
        ),
    ];
    let turns = build_turns("Codex CLI", &messages, &[], false);
    assert_eq!(turns[0].spans.len(), 2, "agent plus the real request");
}

#[test]
fn tool_only_turn_keeps_its_own_prompt() {
    let messages = vec![
        message(0, MessageRole::User, "u1", "conv", None, Stats::default()),
        message(
            1,
            MessageRole::Assistant,
            "a1",
            "conv",
            Some("gpt-5"),
            usage(5, 2, 0.0),
        ),
        message(10, MessageRole::User, "u2", "conv", None, Stats::default()),
        message(
            11,
            MessageRole::Assistant,
            "pending",
            "conv",
            Some("gpt-5"),
            Stats {
                tool_calls: 1,
                ..Stats::default()
            },
        ),
    ];
    let calls = vec![tool_call(11, "shell", "pending", "conv")];
    let turns = build_turns("Codex CLI", &messages, &calls, true);
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].spans.len(), 2);
    assert_eq!(
        turns[1].trace_id,
        ids::trace_id_for("Codex CLI", "conv", "hash-u2")
    );
    assert_eq!(turns[1].spans.len(), 2);
    assert_eq!(turns[1].spans[1].name, "execute_tool shell");
    let standalone = build_turns("Codex CLI", &messages[2..], &calls, true);
    assert_eq!(standalone[0].trace_id, turns[1].trace_id);
    assert_eq!(standalone[0].spans[1].span_id, turns[1].spans[1].span_id);
}

#[test]
fn aggregate_cache_usage_does_not_count_writes_as_reads() {
    let cached = message(
        0,
        MessageRole::Assistant,
        "cached",
        "conv",
        Some("gemini-2.5-pro"),
        Stats {
            cached_tokens: 40,
            ..Stats::default()
        },
    );
    let turns = build_turns("Gemini CLI", &[cached], &[], false);
    assert_eq!(
        attr_int_value(&turns[0].spans[1], "gen_ai.usage.cache_read.input_tokens"),
        Some(40)
    );

    let written = message(
        0,
        MessageRole::Assistant,
        "written",
        "conv",
        Some("claude-sonnet-4"),
        Stats {
            cached_tokens: 20,
            cache_creation_tokens: 20,
            ..Stats::default()
        },
    );
    let turns = build_turns("Claude Code", &[written], &[], false);
    assert_eq!(
        attr_int_value(&turns[0].spans[1], "gen_ai.usage.cache_read.input_tokens"),
        None
    );
    assert_eq!(
        attr_int_value(&turns[0].spans[1], "gen_ai.usage.cache_write.input_tokens"),
        Some(20)
    );
}

#[test]
fn equal_time_prompt_precedes_reply_even_when_hash_sorts_last() {
    let prompt = message(0, MessageRole::User, "z", "conv", None, Stats::default());
    let reply = message(
        0,
        MessageRole::Assistant,
        "a",
        "conv",
        Some("gpt-4o"),
        usage(3, 2, 0.0),
    );
    let turns = build_turns("Codex CLI", &[reply, prompt.clone()], &[], false);
    assert_eq!(turns.len(), 1);
    assert_eq!(
        turns[0].trace_id,
        ids::trace_id_for("Codex CLI", "conv", &prompt.global_hash)
    );
    assert_eq!(turns[0].spans.len(), 2);
    assert!(attr(&turns[0].spans[0], "splitrail.turn.origin").is_none());
}

#[test]
fn unbilled_prompt_keeps_its_tool_call_out_of_the_previous_turn() {
    let messages = [
        message(0, MessageRole::User, "u1", "conv", None, Stats::default()),
        message(
            1,
            MessageRole::Assistant,
            "a1",
            "conv",
            None,
            usage(1, 1, 0.0),
        ),
        message(2, MessageRole::User, "u2", "conv", None, Stats::default()),
    ];
    let turns = build_turns(
        "Codex CLI",
        &messages,
        &[tool_call(3, "shell", "call", "conv")],
        true,
    );
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].spans.len(), 2);
    assert_eq!(
        turns[1].trace_id,
        ids::trace_id_for("Codex CLI", "conv", &messages[2].global_hash)
    );
    assert_eq!(turns[1].spans[1].name, "execute_tool shell");
}

#[test]
fn session_name_cannot_leak_prompt_text_into_spans() {
    let mut reply = message(
        1,
        MessageRole::Assistant,
        "a",
        "conv",
        None,
        usage(1, 1, 0.0),
    );
    reply.session_name = Some("secret prompt text".to_string());
    let turns = build_turns("Codex CLI", &[reply], &[], false);
    let payload = simd_json::to_string(&turns[0].spans).unwrap();
    assert!(!payload.contains("secret prompt text"));
    assert!(
        turns[0]
            .spans
            .iter()
            .all(|span| attr(span, "splitrail.session.name").is_none())
    );
}

#[test]
fn invalid_message_and_tool_timestamps_are_skipped_without_epoch_fallback() {
    let mut reply = message(
        0,
        MessageRole::Assistant,
        "a",
        "conv",
        None,
        usage(1, 1, 0.0),
    );
    for date in [
        DateTime::UNIX_EPOCH - Duration::nanoseconds(1),
        DateTime::<Utc>::MAX_UTC,
    ] {
        reply.date = date;
        assert!(build_turns("Codex CLI", &[reply.clone()], &[], false).is_empty());
        let mut call = tool_call(1, "shell", "bad", "conv");
        call.timestamp = date;
        let prompt = message(0, MessageRole::User, "u", "conv", None, Stats::default());
        assert!(build_turns("Codex CLI", &[prompt], &[call], true).is_empty());
    }
    reply.date = DateTime::UNIX_EPOCH;
    let turns = build_turns("Codex CLI", &[reply], &[], false);
    assert_eq!(turns[0].spans[1].start_time_unix_nano, "0");
}

#[test]
fn tool_duration_outside_timestamp_range_is_skipped() {
    let prompt = message(0, MessageRole::User, "u", "conv", None, Stats::default());
    let mut call = tool_call(1, "shell", "bad-end", "conv");
    call.duration_ms = Some(i64::MAX);
    assert!(build_turns("Codex CLI", &[prompt], &[call], true).is_empty());
}
