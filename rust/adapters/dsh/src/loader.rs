use std::collections::HashSet;

use crate::{LoadedEntry, PricingMap, Result, cli::SharedArgs};

use super::parser;

pub fn load_entries(shared: &SharedArgs, pricing: &PricingMap) -> Result<Vec<LoadedEntry>> {
    crate::progress::track_usage_load(
        crate::progress::UsageLoadAgent("DeepSeek Harness"),
        shared.json,
        || {
            let files = super::paths::discover_session_files()?;
            let loaded =
                ccusage_adapter_common::read_files_parallel(&files, shared.single_thread, |file| {
                    parser::read_session_file(file, shared, pricing).unwrap_or_else(|error| {
                        crate::debug_log(
                            shared,
                            format!(
                                "Failed to read DeepSeek Harness session file {}: {error}",
                                file.display()
                            ),
                        );
                        Vec::new()
                    })
                });
            let mut entries = Vec::new();
            let mut seen = HashSet::new();
            for file_entries in loaded {
                for entry in file_entries {
                    let id = entry.data.message.id.clone();
                    if id.as_ref().is_none_or(|id| seen.insert(id.clone())) {
                        entries.push(entry);
                    }
                }
            }
            entries.sort_by_key(|entry| {
                (
                    entry.timestamp,
                    entry.session_id.to_string(),
                    entry.data.message.id.clone().unwrap_or_default(),
                )
            });
            Ok(entries)
        },
    )
}
