//! Turn segmentation and span construction.
//!
//! Splitrail's normalized messages are one row per user prompt or assistant
//! reply, with a timestamp and a token/cost breakdown. That is enough to build
//! the same shape of trace Claude Code emits natively:
//!
//! ```text
//! invoke_agent Codex CLI
//! ├── chat gpt-5.6-codex        (one assistant message = one billed request)
//! ├── execute_tool shell        (opt-in; needs per-analyzer tool records)
//! └── chat gpt-5.6-codex
//! ```
//!
//! A turn starts at a user message and covers the assistant messages and tool
//! calls that follow it. Duration is *derived* for requests, because transcripts
//! record when a message was written, not how long the request took: a `chat`
//! span ends at the next event in the session, clamped to a sanity window. Tool
//! spans use a recorded duration when the transcript has one. Either way the
//! span says which it is through `splitrail.timing`, so nobody builds an SLO on
//! an inferred number.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, Utc};

use crate::types::{ConversationMessage, MessageRole, Stats, ToolCall};

use super::encode::{self, KeyValue, SPAN_KIND_CLIENT, Span, attr_double, attr_int, attr_str};
use super::ids;

/// A chat span never looks longer than this, however long the gap until the
/// next message happens to be. A user stepping away for lunch is not a two-hour
/// request.
const MAX_DERIVED_CHAT_MS: i64 = 120_000;

/// Minimum span duration, so ordering survives two events in the same
/// millisecond.
const MIN_DURATION_MS: i64 = 1;

/// One trace: a turn plus the requests and tool calls inside it.
#[derive(Debug, Clone)]
pub struct TurnSpans {
    pub trace_id: String,
    pub spans: Vec<Span>,
}

/// Build traces for one analyzer's messages.
///
/// Messages and tool calls are grouped by conversation and ordered by
/// timestamp with a stable tie-break, so repeated exports produce identical
/// traces.
pub fn build_turns(
    analyzer_name: &str,
    messages: &[ConversationMessage],
    tool_calls: &[ToolCall],
    include_tool_spans: bool,
) -> Vec<TurnSpans> {
    let mut by_conversation: BTreeMap<&str, Vec<&ConversationMessage>> = BTreeMap::new();
    for message in messages {
        by_conversation
            .entry(message.conversation_hash.as_str())
            .or_default()
            .push(message);
    }

    let mut calls_by_conversation: HashMap<&str, Vec<&ToolCall>> = HashMap::new();
    if include_tool_spans {
        for call in tool_calls {
            if call
                .timestamp
                .timestamp_nanos_opt()
                .is_none_or(|nanos| nanos < 0)
            {
                continue;
            }
            calls_by_conversation
                .entry(call.conversation_hash.as_str())
                .or_default()
                .push(call);
        }
    }

    let mut turns = Vec::new();
    for (conversation, mut ordered) in by_conversation {
        ordered.retain(|message| {
            message
                .date
                .timestamp_nanos_opt()
                .is_some_and(|nanos| nanos >= 0)
        });
        ordered.sort_by(|a, b| {
            a.date
                .cmp(&b.date)
                .then_with(|| {
                    (a.role != MessageRole::User || a.is_tool_result)
                        .cmp(&(b.role != MessageRole::User || b.is_tool_result))
                })
                .then_with(|| a.global_hash.cmp(&b.global_hash))
        });

        let mut groups = segment_turns(&ordered);
        if include_tool_spans {
            let mut calls = calls_by_conversation
                .remove(conversation)
                .unwrap_or_default();
            calls.sort_by(|a, b| {
                a.timestamp
                    .cmp(&b.timestamp)
                    .then_with(|| a.key().cmp(&b.key()))
            });
            assign_tool_calls(&mut groups, &calls);
        }

        for group in &groups {
            if let Some(turn) = build_turn(analyzer_name, conversation, group, include_tool_spans) {
                turns.push(turn);
            }
        }
    }
    turns
}

/// A reply and the timestamp of whatever followed it in the session, which is
/// what the derived duration is measured against.
struct Reply<'a> {
    message: &'a ConversationMessage,
    next_event: Option<DateTime<Utc>>,
}

/// A turn: the prompt that started it and the work that answered it.
struct TurnGroup<'a> {
    user: Option<&'a ConversationMessage>,
    replies: Vec<Reply<'a>>,
    calls: Vec<&'a ToolCall>,
}

fn segment_turns<'a>(ordered: &[&'a ConversationMessage]) -> Vec<TurnGroup<'a>> {
    let mut turns: Vec<TurnGroup<'a>> = Vec::new();
    let mut current: Option<TurnGroup<'a>> = None;

    for (index, message) in ordered.iter().enumerate() {
        let next_event = ordered.get(index + 1).map(|message| message.date);
        match message.role {
            MessageRole::User if !message.is_tool_result => {
                if let Some(turn) = current.take() {
                    turns.push(turn);
                }
                current = Some(TurnGroup {
                    user: Some(message),
                    replies: Vec::new(),
                    calls: Vec::new(),
                });
            }
            MessageRole::User | MessageRole::Assistant => match current.as_mut() {
                Some(turn) => turn.replies.push(Reply {
                    message,
                    next_event,
                }),
                None => {
                    // A tool-result row or assistant output may be the first
                    // surviving event in an incremental/resumed transcript.
                    current = Some(TurnGroup {
                        user: None,
                        replies: vec![Reply {
                            message,
                            next_event,
                        }],
                        calls: Vec::new(),
                    });
                }
            },
        }
    }

    if let Some(turn) = current.take() {
        turns.push(turn);
    }

    turns
}

/// Attach each tool call to the turn it happened in: the last turn that started
/// at or before the call. Tool calls that precede every prompt (a resumed
/// session) belong to the first turn.
fn assign_tool_calls<'a>(turns: &mut [TurnGroup<'a>], calls: &[&'a ToolCall]) {
    if turns.is_empty() {
        return;
    }
    let starts: Vec<DateTime<Utc>> = turns
        .iter()
        .map(|turn| {
            turn.user
                .or_else(|| turn.replies.first().map(|reply| reply.message))
                .map(|message| message.date)
                .unwrap_or_else(Utc::now)
        })
        .collect();

    for call in calls {
        let mut owner = 0;
        for (index, start) in starts.iter().enumerate() {
            if *start <= call.timestamp {
                owner = index;
            }
        }
        turns[owner].calls.push(call);
    }
}

fn build_turn(
    analyzer_name: &str,
    conversation: &str,
    group: &TurnGroup<'_>,
    include_tool_spans: bool,
) -> Option<TurnSpans> {
    let first = group
        .user
        .or_else(|| group.replies.first().map(|reply| reply.message))?;

    let replies: Vec<&Reply<'_>> = group
        .replies
        .iter()
        .filter(|reply| is_reportable(reply.message))
        .collect();
    if replies.is_empty() && group.calls.is_empty() {
        return None;
    }

    let trace_id = ids::trace_id_for(analyzer_name, conversation, &first.global_hash);
    let agent_span_id = ids::turn_span_id(analyzer_name, conversation, &first.global_hash);
    let conversation_id = conversation_id(analyzer_name, conversation, &first.global_hash);

    let mut chat_spans = Vec::with_capacity(replies.len());
    let mut turn_start = group.user.map(|user| user.date);
    let mut turn_end: Option<DateTime<Utc>> = None;
    let mut turn_input = 0u64;
    let mut turn_output = 0u64;
    let mut turn_cost = 0.0f64;

    for reply in &replies {
        let start = reply.message.date;
        let end = derived_end(start, reply.next_event);
        let Some(span) = chat_span(
            analyzer_name,
            conversation,
            &conversation_id,
            &trace_id,
            &agent_span_id,
            reply.message,
            start,
            end,
        ) else {
            continue;
        };
        chat_spans.push(span);
        turn_input = turn_input.saturating_add(reply.message.stats.input_tokens);
        turn_output = turn_output.saturating_add(reply.message.stats.output_tokens);
        turn_cost += reply.message.stats.cost;
        turn_start = Some(turn_start.map_or(start, |current| current.min(start)));
        turn_end = Some(turn_end.map_or(end, |current| current.max(end)));
    }

    let mut tool_spans = Vec::new();
    if include_tool_spans {
        for call in &group.calls {
            let duration = call.duration_ms.filter(|duration| *duration > 0);
            let start = call.timestamp;
            let Some(end) = start
                .checked_add_signed(Duration::milliseconds(duration.unwrap_or(MIN_DURATION_MS)))
            else {
                continue;
            };
            let timing = if duration.is_some() {
                "recorded"
            } else {
                "derived"
            };
            let Some(span) = tool_span(
                analyzer_name,
                conversation,
                &conversation_id,
                &trace_id,
                &agent_span_id,
                call,
                start,
                end,
                timing,
            ) else {
                continue;
            };
            tool_spans.push(span);
            turn_start = Some(turn_start.map_or(start, |current| current.min(start)));
            turn_end = Some(turn_end.map_or(end, |current| current.max(end)));
        }
    }

    if chat_spans.is_empty() && tool_spans.is_empty() {
        return None;
    }
    let turn_start = turn_start.unwrap_or(first.date);
    let turn_end = turn_end.unwrap_or(turn_start);

    let mut spans = Vec::with_capacity(chat_spans.len() + tool_spans.len() + 1);
    spans.push(agent_span(
        analyzer_name,
        &conversation_id,
        &trace_id,
        &agent_span_id,
        turn_start,
        turn_end,
        turn_input,
        turn_output,
        turn_cost,
        group.user.is_some(),
    )?);
    spans.append(&mut chat_spans);
    spans.append(&mut tool_spans);

    Some(TurnSpans { trace_id, spans })
}

/// Derived end of a request: the next event in the session, clamped.
fn derived_end(start: DateTime<Utc>, next: Option<DateTime<Utc>>) -> DateTime<Utc> {
    let Some(next) = next else {
        return start + Duration::milliseconds(MIN_DURATION_MS);
    };
    let gap = next - start;
    if gap.num_milliseconds() < MIN_DURATION_MS {
        return start + Duration::milliseconds(MIN_DURATION_MS);
    }
    if gap.num_milliseconds() > MAX_DERIVED_CHAT_MS {
        return start + Duration::milliseconds(MAX_DERIVED_CHAT_MS);
    }
    start + gap
}

fn stats_are_empty(stats: &Stats) -> bool {
    stats.input_tokens == 0
        && stats.output_tokens == 0
        && stats.reasoning_tokens == 0
        && stats.cache_creation_tokens == 0
        && stats.cache_read_tokens == 0
        && stats.cached_tokens == 0
        && stats.cost.abs() < f64::EPSILON
}

/// Is this message a billed request worth a span? Synthetic records, tool-only
/// records, and prompts are not: they have no usage of their own.
fn is_reportable(message: &ConversationMessage) -> bool {
    message.role == MessageRole::Assistant && !stats_are_empty(&message.stats)
}

fn conversation_id(analyzer: &str, conversation_hash: &str, first_hash: &str) -> String {
    if conversation_hash.trim().is_empty() {
        format!("{analyzer}:{first_hash}")
    } else {
        conversation_hash.to_string()
    }
}

#[allow(clippy::too_many_arguments)]
fn chat_span(
    analyzer_name: &str,
    conversation: &str,
    conversation_id: &str,
    trace_id: &str,
    parent_span_id: &str,
    message: &ConversationMessage,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Option<Span> {
    let name = match &message.model {
        Some(model) => format!("chat {model}"),
        None => "chat".to_string(),
    };

    let mut attributes = vec![
        attr_str("gen_ai.operation.name", "chat"),
        attr_str("splitrail.analyzer", analyzer_name),
        attr_str("splitrail.timing", "derived"),
        attr_str("gen_ai.conversation.id", conversation_id),
    ];
    if let Some(provider) = provider_for_model(message.model.as_deref()) {
        attributes.push(attr_str("gen_ai.provider.name", provider));
    }
    if let Some(model) = &message.model {
        attributes.push(attr_str("gen_ai.request.model", model));
        attributes.push(attr_str("gen_ai.response.model", model));
    }
    attributes.extend(usage_attributes(&message.stats));

    Some(Span {
        trace_id: trace_id.to_string(),
        span_id: ids::chat_span_id(analyzer_name, conversation, &message.global_hash),
        parent_span_id: Some(parent_span_id.to_string()),
        name,
        kind: SPAN_KIND_CLIENT,
        start_time_unix_nano: encode::nanos(&start)?,
        end_time_unix_nano: encode::nanos(&end)?,
        attributes,
        status: None,
        events: Vec::new(),
    })
}

#[allow(clippy::too_many_arguments)]
fn tool_span(
    analyzer_name: &str,
    conversation: &str,
    conversation_id: &str,
    trace_id: &str,
    parent_span_id: &str,
    call: &ToolCall,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    timing: &str,
) -> Option<Span> {
    let mut attributes = vec![
        attr_str("gen_ai.operation.name", "execute_tool"),
        attr_str("gen_ai.tool.name", call.name.clone()),
        attr_str("splitrail.analyzer", analyzer_name),
        attr_str("splitrail.timing", timing),
        attr_str("gen_ai.conversation.id", conversation_id),
    ];
    if let Some(call_id) = &call.call_id {
        attributes.push(attr_str("gen_ai.tool.call.id", call_id.clone()));
    }

    Some(Span {
        trace_id: trace_id.to_string(),
        span_id: ids::tool_span_id(analyzer_name, conversation, &call.key()),
        parent_span_id: Some(parent_span_id.to_string()),
        name: format!("execute_tool {}", call.name),
        kind: SPAN_KIND_CLIENT,
        start_time_unix_nano: encode::nanos(&start)?,
        end_time_unix_nano: encode::nanos(&end)?,
        attributes,
        status: None,
        events: Vec::new(),
    })
}

#[allow(clippy::too_many_arguments)]
fn agent_span(
    analyzer_name: &str,
    conversation_id: &str,
    trace_id: &str,
    span_id: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    input_tokens: u64,
    output_tokens: u64,
    cost: f64,
    has_prompt: bool,
) -> Option<Span> {
    let mut attributes = vec![
        attr_str("gen_ai.operation.name", "invoke_agent"),
        attr_str("gen_ai.agent.name", analyzer_name),
        attr_str("splitrail.analyzer", analyzer_name),
        attr_str("splitrail.timing", "derived"),
        attr_str("gen_ai.conversation.id", conversation_id),
        attr_int("gen_ai.usage.input_tokens", input_tokens),
        attr_int("gen_ai.usage.output_tokens", output_tokens),
    ];
    if cost != 0.0 {
        attributes.push(attr_double("splitrail.cost.usd", cost));
    }
    if !has_prompt {
        attributes.push(attr_str("splitrail.turn.origin", "transcript"));
    }

    Some(Span {
        trace_id: trace_id.to_string(),
        span_id: span_id.to_string(),
        parent_span_id: None,
        name: format!("invoke_agent {analyzer_name}"),
        kind: SPAN_KIND_CLIENT,
        start_time_unix_nano: encode::nanos(&start)?,
        end_time_unix_nano: encode::nanos(&end)?,
        attributes,
        status: None,
        events: Vec::new(),
    })
}

fn usage_attributes(stats: &Stats) -> Vec<KeyValue> {
    let mut attributes = Vec::new();
    if stats.input_tokens > 0 {
        attributes.push(attr_int("gen_ai.usage.input_tokens", stats.input_tokens));
    }
    if stats.output_tokens > 0 {
        attributes.push(attr_int("gen_ai.usage.output_tokens", stats.output_tokens));
    }
    let cache_read_tokens = if stats.cache_read_tokens > 0 {
        stats.cache_read_tokens
    } else {
        stats
            .cached_tokens
            .saturating_sub(stats.cache_creation_tokens)
    };
    if cache_read_tokens > 0 {
        attributes.push(attr_int(
            "gen_ai.usage.cache_read.input_tokens",
            cache_read_tokens,
        ));
    }
    if stats.cache_creation_tokens > 0 {
        attributes.push(attr_int(
            "gen_ai.usage.cache_write.input_tokens",
            stats.cache_creation_tokens,
        ));
    }
    if stats.reasoning_tokens > 0 {
        attributes.push(attr_int(
            "splitrail.usage.reasoning_tokens",
            stats.reasoning_tokens,
        ));
    }
    if stats.tool_calls > 0 {
        attributes.push(attr_int(
            "splitrail.tool_calls",
            u64::from(stats.tool_calls),
        ));
    }
    if stats.cost != 0.0 {
        attributes.push(attr_double("splitrail.cost.usd", stats.cost));
    }
    attributes
}

/// Map a model name onto the `gen_ai.provider.name` registry values.
fn provider_for_model(model: Option<&str>) -> Option<&'static str> {
    let model = model?.trim().to_ascii_lowercase();
    let model = model.as_str();
    if model.starts_with("claude") {
        Some("anthropic")
    } else if model.starts_with("gpt")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
        || model.contains("codex")
    {
        Some("openai")
    } else if model.starts_with("gemini") {
        Some("gcp.gen_ai")
    } else if model.starts_with("grok") {
        Some("xai")
    } else if model.starts_with("deepseek") {
        Some("deepseek")
    } else if model.starts_with("qwen") {
        Some("alibaba")
    } else if model.starts_with("kimi") || model.starts_with("moonshot") {
        Some("moonshot")
    } else if model.starts_with("glm") || model.starts_with("z-ai") {
        Some("zhipu")
    } else if model.starts_with("minimax") {
        Some("minimax")
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
