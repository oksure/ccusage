use ccusage_core::*;

mod loader;
mod parser;
mod paths;
mod report;

use crate::{
    PricingMap, Result, cli::AgentCommandArgs, print_json_or_jq, print_usage_table, sort_summaries,
    wants_json,
};

pub use loader::load_entries;
pub(crate) use report::report_from_rows;
pub use report::summarize_entries;

pub fn run(args: AgentCommandArgs) -> Result<()> {
    let shared = args.shared;
    let pricing = PricingMap::load_with_overrides(
        shared.offline,
        crate::log_level() != Some(0),
        shared.pricing_overrides.iter(),
    );
    let mut entries = load_entries(&shared, &pricing)?;
    ccusage_adapter_common::filter_loaded_entries_by_date(&mut entries, &shared);
    let mut rows = summarize_entries(&entries, args.kind)?;
    sort_summaries(&mut rows, &shared.order, ccusage_core::summary_period);
    if wants_json(&shared) {
        return print_json_or_jq(
            report_from_rows(&rows, args.kind),
            shared.jq.as_deref(),
            shared.no_cost,
        );
    }
    print_usage_table(
        "DeepSeek Harness Token Usage Report",
        ccusage_core::first_column(args.kind),
        &rows,
        &shared,
        false,
        None,
    )?;
    Ok(())
}

pub fn has_data() -> bool {
    paths::discover_session_files().is_ok_and(|files| !files.is_empty())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ccusage_test_support::{EnvVarGuard, fs_fixture};
    use serde_json::json;

    use super::*;
    use crate::cli::{AgentReportKind, CostMode, SharedArgs};

    fn session_log() -> String {
        [
            r#"{"type":"session","version":0,"id":"session-1","createdAt":1780000000000,"cwd":"/workspace/project","delegationDepth":0}"#,
            r#"{"type":"request/header","seq":1,"time":1780000000001,"data":{"header":{"config":{"provider":"together","model":"deepseek-ai/DeepSeek-V4-Flash-0731"}}}}"#,
            r#"{"type":"request/context","seq":2,"time":1780000000002,"data":{"provider":"together","model":"deepseek-ai/DeepSeek-V4-Flash-0731"}}"#,
            r#"{"type":"assistant/chunk","seq":3,"time":1780000000010,"data":{"turn":1,"step":1,"chunk":{"type":"usage","usage":{"inputTokens":100,"outputTokens":20,"cacheReadTokens":30,"cacheWriteTokens":4,"reasoningTokens":12}}}}"#,
            r#"{"type":"assistant/message","seq":4,"time":1780000000020,"data":{"turn":1,"step":1,"message":{"role":"assistant","source":{"kind":"model","provider":"together","model":"deepseek-ai/DeepSeek-V4-Flash-0731"},"content":[]},"usage":{"inputTokens":120,"outputTokens":30,"cacheReadTokens":40,"cacheWriteTokens":6,"reasoningTokens":18}}}"#,
            r#"{"type":"request/context","seq":5,"time":1780000000030,"data":{"provider":"openai-codex","model":"gpt-5.6-luna"}}"#,
            r#"{"type":"assistant/chunk","seq":6,"time":1780000000040,"data":{"turn":1,"step":2,"chunk":{"type":"usage","usage":{"inputTokens":50,"outputTokens":5}}}}"#,
            r#"{"type":"assistant/message","seq":7,"time":1780000000050,"data":{"turn":1,"step":2,"message":{"role":"assistant","source":{"kind":"model","provider":"openai-codex","model":"gpt-5.6-luna"},"content":[]}}}"#,
        ]
        .join("\n")
    }

    fn shared() -> SharedArgs {
        SharedArgs {
            mode: CostMode::Display,
            timezone: Some("UTC".to_string()),
            ..SharedArgs::default()
        }
    }

    #[test]
    fn loads_raw_session_usage_and_replaces_streaming_samples() {
        let fixture = fs_fixture!({
            "sessions/--workspace-project--/session-1/session.jsonl": session_log(),
        });
        let _guard = EnvVarGuard::set("DSH_HOME", fixture.root());

        let entries = load_entries(&shared(), &PricingMap::default()).unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].session_id.as_ref(), "session-1");
        assert_eq!(entries[0].project_path.as_ref(), "/workspace/project");
        assert_eq!(
            entries[0].model.as_deref(),
            Some("deepseek-ai/DeepSeek-V4-Flash-0731")
        );
        assert_eq!(entries[0].data.message.usage.input_tokens, 120);
        assert_eq!(entries[0].data.message.usage.output_tokens, 30);
        assert_eq!(entries[0].data.message.usage.cache_read_input_tokens, 40);
        assert_eq!(entries[0].data.message.usage.cache_creation_input_tokens, 6);
        assert_eq!(entries[0].extra_total_tokens, 0);
        assert_eq!(entries[1].data.message.usage.input_tokens, 50);
        assert_eq!(entries[1].model.as_deref(), Some("gpt-5.6-luna"));
    }

    #[test]
    fn reads_concatenated_zstd_frames() {
        let fixture = fs_fixture!({});
        let path = fixture.path("sessions/project/session-1/session.jsonl.zstd");
        let log = session_log();
        let (header, events) = log.split_once('\n').unwrap();
        let mut encoded = zstd::stream::encode_all(format!("{header}\n").as_bytes(), 0).unwrap();
        encoded.extend(zstd::stream::encode_all(events.as_bytes(), 0).unwrap());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, encoded).unwrap();
        let _guard = EnvVarGuard::set("DSH_HOME", fixture.root());

        let entries = load_entries(&shared(), &PricingMap::default()).unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].date, "2026-05-28");
    }

    #[test]
    fn deduplicates_the_same_step_across_session_files() {
        let fixture = fs_fixture!({
            "sessions/project-a/session-a/session.jsonl": session_log(),
            "sessions/project-b/session-b/session.jsonl": session_log(),
            "sessions/project-c/session-c/session.jsonl": "not a DSH session\n",
        });
        let _guard = EnvVarGuard::set("DSH_HOME", fixture.root());

        let entries = load_entries(&shared(), &PricingMap::default()).unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.data.message.id.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("dsh:session-1:1:1"), Some("dsh:session-1:1:2")]
        );
    }

    #[test]
    fn daily_report_keeps_cache_write_tokens_and_does_not_add_reasoning_twice() {
        let fixture = fs_fixture!({
            "sessions/project/session-1/session.jsonl": session_log(),
        });
        let _guard = EnvVarGuard::set("DSH_HOME", fixture.root());
        let entries = load_entries(&shared(), &PricingMap::default()).unwrap();
        let rows = summarize_entries(&entries, AgentReportKind::Daily).unwrap();
        let report = report_from_rows(&rows, AgentReportKind::Daily);

        assert_eq!(report["daily"][0]["inputTokens"], 170);
        assert_eq!(report["daily"][0]["outputTokens"], 35);
        assert_eq!(report["daily"][0]["cacheCreationTokens"], 6);
        assert_eq!(report["daily"][0]["cacheReadTokens"], 40);
        assert_eq!(report["daily"][0]["totalTokens"], 251);
        assert_eq!(
            report["daily"][0]["modelsUsed"],
            json!(["deepseek-ai/DeepSeek-V4-Flash-0731", "gpt-5.6-luna"])
        );
    }
}
