#![allow(clippy::collapsible_if)]

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::analyzer::{Analyzer, DataSource};
use crate::contribution_cache::ContributionStrategy;
use crate::types::{Application, ConversationMessage, MessageRole, Stats};
use crate::utils::hash_text;

pub struct AntigravityCliAnalyzer;

impl AntigravityCliAnalyzer {
    pub fn new() -> Self {
        Self
    }

    fn data_dir() -> Option<PathBuf> {
        dirs::home_dir().map(|h| {
            h.join(".gemini")
                .join("antigravity-cli")
                .join("conversations")
        })
    }
}

// Protobuf wire-format parser helper types and functions

#[derive(Clone, Debug)]
pub(crate) struct AgProtoField {
    pub(crate) number: u32,
    pub(crate) wire: u8,
    pub(crate) varint: u64,
    pub(crate) fixed: Vec<u8>,
    pub(crate) bytes: Vec<u8>,
    pub(crate) nested: Option<Vec<AgProtoField>>,
}

const PB_WIRE_VARINT: u8 = 0;
const PB_WIRE_FIXED64: u8 = 1;
const PB_WIRE_BYTES: u8 = 2;
const PB_WIRE_FIXED32: u8 = 5;
const MAX_DEPTH: usize = 32;

fn read_varint(data: &[u8]) -> Option<(u64, &[u8])> {
    let mut val = 0u64;
    let mut shift = 0;
    for (i, &b) in data.iter().enumerate() {
        val |= ((b & 0x7F) as u64) << shift;
        if (b & 0x80) == 0 {
            return Some((val, &data[i + 1..]));
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
    None
}

pub(crate) fn ag_proto_parse(data: &[u8]) -> Option<Vec<AgProtoField>> {
    ag_proto_parse_depth(data, 0)
}

fn ag_proto_parse_depth(mut data: &[u8], depth: usize) -> Option<Vec<AgProtoField>> {
    if depth > MAX_DEPTH {
        return None;
    }
    let mut out = Vec::new();
    while !data.is_empty() {
        let (tag, rest) = read_varint(data)?;
        data = rest;
        let number = (tag >> 3) as u32;
        let wire = (tag & 0x7) as u8;
        if number == 0 {
            return None;
        }
        let mut f = AgProtoField {
            number,
            wire,
            varint: 0,
            fixed: Vec::new(),
            bytes: Vec::new(),
            nested: None,
        };
        match wire {
            PB_WIRE_VARINT => {
                let (v, rest) = read_varint(data)?;
                f.varint = v;
                data = rest;
            }
            PB_WIRE_FIXED64 => {
                if data.len() < 8 {
                    return None;
                }
                f.fixed = data[..8].to_vec();
                data = &data[8..];
            }
            PB_WIRE_FIXED32 => {
                if data.len() < 4 {
                    return None;
                }
                f.fixed = data[..4].to_vec();
                data = &data[4..];
            }
            PB_WIRE_BYTES => {
                let (ln, rest) = read_varint(data)?;
                let ln = ln as usize;
                if rest.len() < ln {
                    return None;
                }
                f.bytes = rest[..ln].to_vec();
                data = &rest[ln..];
                if let Some(nested) = ag_proto_parse_depth(&f.bytes, depth + 1) {
                    if ag_proto_looks_like_message(&nested) {
                        f.nested = Some(nested);
                    }
                }
            }
            3 | 4 => {
                // start/end group, deprecated, skip
            }
            _ => return None, // unknown wire type
        }
        out.push(f);
    }
    Some(out)
}

fn ag_proto_looks_like_message(fields: &[AgProtoField]) -> bool {
    if fields.is_empty() {
        return false;
    }
    for f in fields {
        if f.number < 1 || f.number > 100_000 {
            return false;
        }
    }
    true
}

fn ag_proto_string(f: &AgProtoField) -> Option<String> {
    if f.wire != PB_WIRE_BYTES {
        return None;
    }
    let s = std::str::from_utf8(&f.bytes).ok()?;
    Some(s.to_string())
}

fn ag_proto_collect_strings(fields: &[AgProtoField], min_len: usize) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(fs: &[AgProtoField], min_len: usize, out: &mut Vec<String>) {
        for f in fs {
            if f.wire == PB_WIRE_BYTES && f.nested.is_none() {
                if let Some(s) = ag_proto_string(f) {
                    if s.chars().count() >= min_len {
                        out.push(s);
                    }
                }
            }
            if let Some(ref nested) = f.nested {
                walk(nested, min_len, out);
            }
        }
    }
    walk(fields, min_len, &mut out);
    out
}

fn earliest_antigravity_timestamp(fields: &[AgProtoField]) -> Option<DateTime<Utc>> {
    let mut best: Option<DateTime<Utc>> = None;
    fn walk(fs: &[AgProtoField], best: &mut Option<DateTime<Utc>>) {
        for f in fs {
            if let Some(ref nested) = f.nested {
                if let Some((sec, nanos)) = ag_proto_timestamp(nested) {
                    if sec > 946_684_800 && sec < 4_102_444_800 {
                        if let Some(t) = Utc.timestamp_opt(sec, nanos).single() {
                            match best {
                                Some(b) if t < *b => *best = Some(t),
                                None => *best = Some(t),
                                _ => {}
                            }
                        }
                    }
                }
                walk(nested, best);
            }
        }
    }
    walk(fields, &mut best);
    best
}

fn ag_proto_timestamp(fields: &[AgProtoField]) -> Option<(i64, u32)> {
    let mut sec = 0i64;
    let mut nanos = 0u32;
    let mut saw_sec = false;
    for f in fields {
        if f.wire != PB_WIRE_VARINT {
            return None;
        }
        match f.number {
            1 => {
                sec = f.varint as i64;
                saw_sec = true;
            }
            2 => {
                nanos = f.varint as u32;
            }
            _ => return None,
        }
    }
    if saw_sec { Some((sec, nanos)) } else { None }
}

fn clean_antigravity_step_strings(strs: Vec<String>, step_type: i32) -> Vec<String> {
    let mut cleaned = Vec::new();
    for s in strs {
        let trimmed = s.trim().to_string();
        if is_noisy_antigravity_step_string(&trimmed) {
            continue;
        }
        cleaned.push(trimmed);
    }
    let mut cleaned = dedupe_strings(cleaned);
    if step_type == 14 {
        if let Some(prompt) = best_antigravity_user_prompt(&cleaned) {
            cleaned = vec![prompt];
        }
    }
    cleaned
}

fn dedupe_strings(in_vec: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for s in in_vec {
        if seen.insert(s.clone()) {
            out.push(s);
        }
    }
    out
}

fn is_uuid_like(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 {
        return false;
    }
    if parts[0].len() != 8
        || parts[1].len() != 4
        || parts[2].len() != 4
        || parts[3].len() != 4
        || parts[4].len() != 12
    {
        return false;
    }
    parts
        .iter()
        .all(|part| part.chars().all(|c| c.is_ascii_hexdigit()))
}

fn is_noisy_antigravity_step_string(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    if is_uuid_like(s) {
        return true;
    }
    if s.starts_with("MODEL_PLACEHOLDER_") {
        return true;
    }
    if s.starts_with('{')
        && (s.contains("\"toolAction\"")
            || s.contains("\"toolSummary\"")
            || s.contains("\"DirectoryPath\""))
    {
        return true;
    }
    if looks_like_antigravity_opaque_id(s) {
        return true;
    }
    if s.starts_with("file:///home/") {
        return true;
    }
    if s.starts_with("/home/") && s.contains("/.gemini/") {
        return true;
    }
    if s.starts_with("/Users/") && s.contains("/.gemini/") {
        return true;
    }
    if s.starts_with(r"C:\Users\") && s.contains(r"\.gemini\") {
        return true;
    }
    if s.starts_with("command(")
        || s.starts_with("execute_url(")
        || s.starts_with("read_url(")
        || s.starts_with("mcp(")
    {
        return true;
    }
    false
}

fn looks_like_antigravity_opaque_id(s: &str) -> bool {
    if s.contains(|c: char| c.is_whitespace()) {
        return false;
    }
    if s.len() < 16 || s.len() > 128 {
        return false;
    }
    let mut alpha = 0;
    let mut digit = 0;
    let mut symbol = 0;
    for c in s.chars() {
        if c.is_ascii_alphabetic() {
            alpha += 1;
        } else if c.is_ascii_digit() {
            digit += 1;
        } else if c == '_' || c == '-' || c == '.' {
            symbol += 1;
        } else {
            return false;
        }
    }
    if alpha + digit + symbol != s.len() {
        return false;
    }
    if digit == s.len() || digit + symbol == s.len() {
        return true;
    }
    alpha > 0 && digit > 0
}

fn best_antigravity_user_prompt(strs: &[String]) -> Option<String> {
    let mut best: Option<String> = None;
    let mut best_score = -1i32;
    for s in strs {
        let score = antigravity_prompt_score(s);
        if score > best_score {
            best = Some(s.clone());
            best_score = score;
        }
    }
    if best_score <= 0 { None } else { best }
}

fn antigravity_prompt_score(s: &str) -> i32 {
    let trimmed = s.trim();
    if trimmed.is_empty() || is_noisy_antigravity_step_string(trimmed) {
        return -1;
    }
    let mut score = trimmed.len() as i32;
    if trimmed.contains(|c: char| c.is_whitespace()) {
        score += 50;
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        score -= 100;
    }
    if trimmed.starts_with('/') || trimmed.starts_with("file://") {
        score -= 100;
    }
    if !trimmed.contains(|c: char| c.is_ascii_alphabetic()) {
        score -= 100;
    }
    score
}

// GenMetaInfo represents LLM generation metadata decoded from SQLite's `gen_metadata` table.
//
// The protobuf schema of this metadata corresponds to the Gemini/Google GenAI API response structure,
// detailing the model ID, display name, and API usage stats (input/output/reasoning tokens).
struct GenMetaInfo {
    // Canonical model ID (e.g. "gemini-3-flash-a")
    model_id: Option<String>,
    // Human-readable model name (e.g. "Gemini 3.5 Flash (Medium)")
    #[allow(dead_code)]
    model_name: Option<String>,
    // Number of tokens in the prompt (input)
    input_tokens: u64,
    // Number of tokens generated in the response (output, candidates)
    output_tokens: u64,
    // Sub-segment of output tokens spent on model reasoning/thinking
    reasoning_tokens: u64,
}

// Parses raw protobuf wire-format fields from the `gen_metadata` table.
//
// Schema Mapping (from Google GenAI response structure):
// - Field 1 (nested): The primary response metadata envelope
//   - Field 19 (string): Model ID identifier
//   - Field 21 (string): User-facing model name string
//   - Field 4 (nested): Token usage statistics
//     - Field 5 (varint): Total prompt input tokens
//     - Field 2 (varint): Total output/candidates tokens
//     - Field 3 (varint): Output tokens spent on reasoning
fn parse_gen_metadata(fields: &[AgProtoField]) -> Option<GenMetaInfo> {
    let f1 = fields.iter().find(|f| f.number == 1)?;
    let nested = f1.nested.as_ref()?;

    let mut model_id = None;
    let mut model_name = None;
    let mut input_tokens = 0;
    let mut output_tokens = 0;
    let mut reasoning_tokens = 0;

    for f in nested {
        match f.number {
            19 => {
                model_id = ag_proto_string(f);
            }
            21 => {
                model_name = ag_proto_string(f);
            }
            4 => {
                if let Some(token_fields) = &f.nested {
                    for tf in token_fields {
                        match tf.number {
                            5 => input_tokens = tf.varint,
                            2 => output_tokens = tf.varint,
                            3 => reasoning_tokens = tf.varint,
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Some(GenMetaInfo {
        model_id,
        model_name,
        input_tokens,
        output_tokens,
        reasoning_tokens,
    })
}

// Parses tool call events from step payload fields to extract action statistics.
//
// Schema Mapping (from Antigravity step action trajectory):
// - Field 20 (nested): Execution metadata envelope
//   - Field 7 (nested): Tool invocation call details
//     - Field 2 (string): Name of the tool executed (e.g., "run_command", "view_file")
fn extract_tool_stats_from_fields(fields: &[AgProtoField], stats: &mut Stats) {
    let tool_calls = fields
        .iter()
        .filter(|f| f.number == 20)
        .filter_map(|f| f.nested.as_deref())
        .flatten()
        .filter(|f20| f20.number == 7)
        .filter_map(|f20| f20.nested.as_deref())
        .filter_map(|nested7| {
            nested7
                .iter()
                .find(|f7| f7.number == 2)
                .and_then(ag_proto_string)
        });

    for tool_name in tool_calls {
        stats.tool_calls += 1;
        match tool_name.as_str() {
            "run_command" => stats.terminal_commands += 1,
            "view_file" => stats.files_read += 1,
            "write_to_file" | "replace_file_content" | "multi_replace_file_content" => {
                stats.files_edited += 1;
            }
            "grep_search" => stats.file_content_searches += 1,
            _ => {}
        }
    }
}

#[async_trait]
impl Analyzer for AntigravityCliAnalyzer {
    fn display_name(&self) -> &'static str {
        "Antigravity CLI"
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();
        if let Some(home_dir) = dirs::home_dir() {
            let home_str = home_dir.to_string_lossy();
            patterns.push(format!(
                "{home_str}/.gemini/antigravity-cli/conversations/**/*.db"
            ));
        }
        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let sources = Self::data_dir()
            .filter(|d| d.is_dir())
            .into_iter()
            .flat_map(|dir| WalkDir::new(dir).into_iter())
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "db")
            })
            .map(|e| DataSource {
                path: e.into_path(),
            })
            .collect();
        Ok(sources)
    }

    fn parse_source(&self, source: &DataSource) -> Result<Vec<ConversationMessage>> {
        let conn = Connection::open_with_flags(
            &source.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;

        let mut gen_meta_map = std::collections::HashMap::new();
        if let Ok(mut gen_stmt) = conn.prepare("SELECT idx, data FROM gen_metadata") {
            if let Ok(mut gen_rows) = gen_stmt.query([]) {
                while let Ok(Some(row)) = gen_rows.next() {
                    if let (Ok(idx), Ok(data)) = (row.get::<_, i32>(0), row.get::<_, Vec<u8>>(1)) {
                        if !data.is_empty() {
                            if let Some(fields) = ag_proto_parse(&data) {
                                if let Some(info) = parse_gen_metadata(&fields) {
                                    gen_meta_map.insert(idx, info);
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut stmt =
            conn.prepare("SELECT idx, step_type, step_payload FROM steps ORDER BY idx")?;

        let mut rows = stmt.query([])?;
        let mut messages = Vec::new();

        let file_path_str = source.path.to_string_lossy().into_owned();
        let conversation_hash = hash_text(&file_path_str);

        let session_name = source
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned());

        let mut last_seen_model = None;

        while let Some(row) = rows.next()? {
            let idx: i32 = row.get(0)?;
            let step_type: i32 = row.get(1)?;
            let payload: Vec<u8> = row.get(2)?;

            if payload.is_empty() {
                continue;
            }

            let Some(fields) = ag_proto_parse(&payload) else {
                continue;
            };

            let strs = clean_antigravity_step_strings(
                dedupe_strings(ag_proto_collect_strings(&fields, 20)),
                step_type,
            );

            let ts = earliest_antigravity_timestamp(&fields).unwrap_or_else(Utc::now);

            if strs.is_empty() {
                continue;
            }

            let role = if step_type == 14 {
                MessageRole::User
            } else {
                MessageRole::Assistant
            };

            let content = strs.join("\n\n");

            let (model, stats) = if role == MessageRole::Assistant {
                let mut stats = Stats::default();
                extract_tool_stats_from_fields(&fields, &mut stats);

                if let Some(gen_meta) = gen_meta_map.get(&idx) {
                    let model_name_str = gen_meta
                        .model_id
                        .clone()
                        .unwrap_or_else(|| "gemini-3-flash-preview".to_string());
                    let cost = crate::models::calculate_total_cost(
                        &model_name_str,
                        gen_meta.input_tokens,
                        gen_meta.output_tokens,
                        0,
                        0,
                    );
                    stats.input_tokens = gen_meta.input_tokens;
                    stats.output_tokens = gen_meta.output_tokens;
                    stats.reasoning_tokens = gen_meta.reasoning_tokens;
                    stats.cost = cost;
                    last_seen_model = Some(model_name_str.clone());
                    (Some(model_name_str), stats)
                } else if gen_meta_map.is_empty() {
                    // Fallback to estimation only if there's no gen_metadata at all in the db (e.g. unit tests)
                    let model_name_str = last_seen_model
                        .clone()
                        .unwrap_or_else(|| "gemini-2.5-flash".to_string());
                    let estimated_output_tokens = crate::analyzers::copilot::count_tokens(&content);
                    let cost = crate::models::calculate_total_cost(
                        &model_name_str,
                        0,
                        estimated_output_tokens,
                        0,
                        0,
                    );
                    stats.output_tokens = estimated_output_tokens;
                    stats.cost = cost;
                    (Some(model_name_str), stats)
                } else {
                    // Real database, but this step has no gen_metadata (e.g. tool execution step)
                    (last_seen_model.clone(), stats)
                }
            } else {
                (None, Stats::default())
            };

            let global_hash = hash_text(&format!(
                "{}_{}_{}_{}",
                file_path_str,
                ts.to_rfc3339(),
                idx,
                role == MessageRole::User
            ));

            messages.push(ConversationMessage {
                application: Application::AntigravityCli,
                date: ts,
                project_hash: "".to_string(),
                conversation_hash: conversation_hash.clone(),
                local_hash: None,
                global_hash,
                model,
                stats,
                role,
                uuid: None,
                session_name: session_name.clone(),
            });
        }

        Ok(messages)
    }

    fn get_watch_directories(&self) -> Vec<PathBuf> {
        Self::data_dir()
            .filter(|d| d.is_dir())
            .into_iter()
            .collect()
    }

    fn is_valid_data_path(&self, path: &Path) -> bool {
        path.is_file() && path.extension().is_some_and(|ext| ext == "db")
    }

    fn contribution_strategy(&self) -> ContributionStrategy {
        ContributionStrategy::SingleSession
    }

    fn is_available(&self) -> bool {
        Self::data_dir()
            .filter(|d| d.is_dir())
            .into_iter()
            .flat_map(|dir| WalkDir::new(dir).into_iter())
            .filter_map(|e| e.ok())
            .any(|e| e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "db"))
    }
}
