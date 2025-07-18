use crate::types::{AgenticCodingToolStats, MultiAnalyzerStats};
use crate::utils::{format_date_for_display, format_number, NumberFormatOptions};
use anyhow::Result;
use colored::*;

// TODO: We really need to use a libary for this.
pub fn run_tui(stats: &AgenticCodingToolStats, format_options: &NumberFormatOptions) -> Result<()> {
    display_single_analyzer_stats(stats, format_options)
}

pub fn run_multi_tui(multi_stats: &MultiAnalyzerStats, format_options: &NumberFormatOptions) -> Result<()> {
    println!();
    println!("{}", "AGENTIC CODING TOOL ACTIVITY ANALYSIS".cyan().bold());
    println!("{}", "=====================================".cyan().bold());
    println!();
    
    // Display each analyzer separately
    for (i, stats) in multi_stats.analyzer_stats.iter().enumerate() {
        if i > 0 {
            println!();
            println!("{}", "─".repeat(80).dimmed());
            println!();
        }
        
        display_single_analyzer_stats(stats, format_options)?;
    }
    
    Ok(())
}

fn display_single_analyzer_stats(stats: &AgenticCodingToolStats, format_options: &NumberFormatOptions) -> Result<()> {
    println!("{} ({} chats)", stats.analyzer_name, stats.num_conversations);
    println!();
    println!("{}", "*Models:".dimmed());
    for (k, v) in &stats.model_abbrs.abbr_to_desc {
        // Only show the abbreviation if this model is used.
        for day_stats in stats.daily_stats.values() {
            if day_stats
                .models
                .contains_key(&stats.model_abbrs.abbr_to_model[k])
            {
                println!("{}", format!("  {}: {}", k, v).dimmed());
                break;
            }
        }
    }
    println!();

    // Print table header
    println!(
        "{:<15} {:>7} {:>12} {:>8} {:>9} {:>6} {:>6} {:>22}     {}{:<25}",
        "Date",
        "Cost",
        "Cached Tks",
        "Inp Tks",
        "Outp Tks",
        "Convs",
        // "Your Msgs",
        // "AI Msgs",
        "Tools",
        "Lines",
        "Models",
        " (see above)".dimmed()
    );
    println!("{}", "─".repeat(113).dimmed());

    let mut best_cost = 0.0;
    let mut best_cost_i = 0;
    let mut best_cached_tokens = 0;
    let mut best_cached_tokens_i = 0;
    let mut best_input_tokens = 0;
    let mut best_input_tokens_i = 0;
    let mut best_output_tokens = 0;
    let mut best_output_tokens_i = 0;
    let mut best_conversations = 0;
    let mut best_conversations_i = 0;
    let mut best_user_messages = 0;
    // let mut best_user_messages_i = 0;
    let mut best_ai_messages = 0;
    // let mut best_ai_messages_i = 0;
    let mut best_tool_calls = 0;
    let mut best_tool_calls_i = 0;
    for (i, day_stats) in stats.daily_stats.values().enumerate() {
        if day_stats.cost > best_cost {
            best_cost = day_stats.cost;
            best_cost_i = i;
        }
        if day_stats.cached_tokens > best_cached_tokens {
            best_cached_tokens = day_stats.cached_tokens;
            best_cached_tokens_i = i;
        }
        if day_stats.input_tokens > best_input_tokens {
            best_input_tokens = day_stats.input_tokens;
            best_input_tokens_i = i;
        }
        if day_stats.output_tokens > best_output_tokens {
            best_output_tokens = day_stats.output_tokens;
            best_output_tokens_i = i;
        }
        if day_stats.conversations > best_conversations {
            best_conversations = day_stats.conversations;
            best_conversations_i = i;
        }
        if day_stats.user_messages > best_user_messages {
            best_user_messages = day_stats.user_messages;
            // best_user_messages_i = i;
        }
        if day_stats.ai_messages > best_ai_messages {
            best_ai_messages = day_stats.ai_messages;
            // best_ai_messages_i = i;
        }
        if day_stats.tool_calls > best_tool_calls {
            best_tool_calls = day_stats.tool_calls;
            best_tool_calls_i = i;
        }
    }

    let mut total_cost = 0.0;
    let mut total_cached = 0;
    let mut total_input = 0;
    let mut total_output = 0;
    // let mut total_your_msgs = 0;
    // let mut total_ai_msgs = 0;
    let mut total_tool_calls = 0;

    // Print the row for each day.
    for (i, (date, day_stats)) in stats.daily_stats.iter().enumerate() {
        total_cost += day_stats.cost;
        total_cached += day_stats.cached_tokens;
        total_input += day_stats.input_tokens;
        total_output += day_stats.output_tokens;
        // total_your_msgs += day_stats.user_messages;
        // total_ai_msgs += day_stats.ai_messages;
        total_tool_calls += day_stats.tool_calls;

        // Collects model abbreviations from day_stats, falling back to model names if no
        // abbreviation is found
        let models = day_stats
            .models
            .keys()
            .map(|k| {
                stats
                    .model_abbrs
                    .model_to_abbr
                    .get(k)
                    .unwrap_or(&k.clone())
                    .clone()
            })
            .collect::<Vec<String>>()
            .join(", ");

        let cached_tokens = format_number(day_stats.cached_tokens, format_options);
        let input_tokens = format_number(day_stats.input_tokens, format_options);
        let output_tokens = format_number(day_stats.output_tokens, format_options);
        let conversations = format_number(day_stats.conversations as u64, format_options);
        // let user_messages = format_number(day_stats.user_messages as u64);
        // let ai_messages = format_number(day_stats.ai_messages as u64);
        let tool_calls = format_number(day_stats.tool_calls as u64, format_options);

        let lines_summary = format!(
            "{}/{}/{}",
            format_number(day_stats.file_operations.lines_read, format_options),
            format_number(day_stats.file_operations.lines_edited, format_options),
            format_number(day_stats.file_operations.lines_written, format_options)
        );

        // Check if this is an empty row (all zeros)
        let is_empty_row = day_stats.cost == 0.0
            && day_stats.cached_tokens == 0
            && day_stats.input_tokens == 0
            && day_stats.output_tokens == 0
            && day_stats.conversations == 0
            && day_stats.user_messages == 0
            && day_stats.ai_messages == 0
            && day_stats.tool_calls == 0;

        println!(
            "{:<15} {:>7} {:>12} {:>8} {:>9} {:>6} {:>6} {:>22}     {:<15}\x1b[0m",
            if is_empty_row {
                format_date_for_display(&date).dimmed()
            } else {
                format_date_for_display(&date).normal()
            },
            if is_empty_row {
                format!("${:.2}", day_stats.cost).dimmed()
            } else if i == best_cost_i {
                format!("${:.2}", day_stats.cost).red()
            } else {
                format!("${:.2}", day_stats.cost).yellow()
            },
            if is_empty_row {
                cached_tokens.dimmed()
            } else if i == best_cached_tokens_i {
                cached_tokens.red()
            } else {
                cached_tokens.dimmed()
            },
            if is_empty_row {
                input_tokens.dimmed()
            } else if i == best_input_tokens_i {
                input_tokens.red()
            } else {
                input_tokens.normal()
            },
            if is_empty_row {
                output_tokens.dimmed()
            } else if i == best_output_tokens_i {
                output_tokens.red()
            } else {
                output_tokens.normal()
            },
            if is_empty_row {
                conversations.dimmed()
            } else if i == best_conversations_i {
                conversations.red()
            } else {
                conversations.normal()
            },
            // if is_empty_row {
            //     user_messages.dimmed()
            // } else if i == best_user_messages_i {
            //     user_messages.red()
            // } else {
            //     user_messages.normal()
            // },
            // if is_empty_row {
            //     ai_messages.dimmed()
            // } else if i == best_ai_messages_i {
            //     ai_messages.red()
            // } else {
            //     ai_messages.normal()
            // },
            if is_empty_row {
                tool_calls.dimmed()
            } else if i == best_tool_calls_i {
                tool_calls.red()
            } else {
                tool_calls.green()
            },
            if is_empty_row {
                lines_summary.dimmed()
            } else {
                lines_summary.blue()
            },
            if is_empty_row {
                models.dimmed()
            } else {
                models.dimmed()
            },
        );
    }

    // Print totals row.
    println!("{}", "─".repeat(113).dimmed());

    // Calculate totals for columns
    let total_lines_r = stats
        .daily_stats
        .values()
        .map(|s| s.file_operations.lines_read)
        .sum::<u64>();
    let total_lines_e = stats
        .daily_stats
        .values()
        .map(|s| s.file_operations.lines_edited)
        .sum::<u64>();
    let total_lines_w = stats
        .daily_stats
        .values()
        .map(|s| s.file_operations.lines_written)
        .sum::<u64>();

    println!(
        "{:<15} {:>7} {:>12} {:>8} {:>9} {:>6} {:>6} {:>22}",
        format!("Total ({}d)", stats.daily_stats.len()),
        format!("${:.2}", total_cost).yellow().bold(),
        format_number(total_cached, format_options).dimmed().bold(),
        format_number(total_input, format_options).bold(),
        format_number(total_output, format_options).bold(),
        format_number(stats.num_conversations, format_options).bold(),
        // format_number(total_your_msgs as u64).bold(),
        // format_number(total_ai_msgs as u64).bold(),
        format_number(total_tool_calls as u64, format_options).green().bold(),
        format!(
            "{}/{}/{}",
            format_number(total_lines_r, format_options),
            format_number(total_lines_e, format_options),
            format_number(total_lines_w, format_options)
        )
        .blue()
        .bold()
    );

    println!();
    println!();

    // Calculate summary stats
    let total_tokens = total_cached + total_input + total_output;
    // let total_messages = total_your_msgs + total_ai_msgs;
    println!(
        "{:<19} {}",
        "Tokens:",
        format_number(total_tokens, format_options).bright_blue().bold()
    );
    // TODO: Message calculation is crazy, at least for CC.
    // println!(
    //     "{:<19} {}",
    //     "Messages exchanged:",
    //     format_number(total_messages as u64)
    // );
    println!(
        "{:<19} {}",
        "Tools Calls:",
        format_number(total_tool_calls as u64, format_options)
            .to_string()
            .bright_green()
            .bold()
    );
    println!(
        "{:<19} {}",
        "Cost:",
        format!("${:.2}", total_cost).bright_yellow().bold()
    );
    println!("{:<19} {}", "Days tracked:", stats.daily_stats.len());

    // Calculate current streak (consecutive days with activity from the most recent date)
    // let mut current_streak = 0;
    let sorted_dates: Vec<_> = stats.daily_stats.keys().collect();

    // Start from the most recent date and work backwards
    for date in sorted_dates.iter().rev() {
        let day_stats = &stats.daily_stats[*date];

        // Check if this day has any activity
        let has_activity = day_stats.cost > 0.0
            || day_stats.cached_tokens > 0
            || day_stats.input_tokens > 0
            || day_stats.output_tokens > 0
            || day_stats.conversations > 0
            || day_stats.user_messages > 0
            || day_stats.ai_messages > 0
            || day_stats.tool_calls > 0;

        if has_activity {
            // current_streak += 1;
        } else {
            break;
        }
    }

    // println!("{:<19} {} days", "Current streak:", current_streak);
    // println!();
    Ok(())
}
