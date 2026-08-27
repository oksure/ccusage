use std::{collections::HashSet, env, path::PathBuf};

use crate::Result;
use ccusage_adapter_common::collect_files_with_extension;

pub(crate) const DSH_HOME_ENV: &str = "DSH_HOME";

fn roots() -> Vec<PathBuf> {
    let env_candidates = env::var(DSH_HOME_ENV).ok().map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .collect()
    });
    let candidates = env_candidates
        .filter(|paths: &Vec<PathBuf>| !paths.is_empty())
        .unwrap_or_else(|| {
            crate::home::home_dir()
                .map(|home| vec![home.join(".dsh")])
                .unwrap_or_default()
        });
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|path| path.is_dir())
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

pub(super) fn discover_session_files() -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for root in roots() {
        let sessions = root.join("sessions");
        if !sessions.is_dir() {
            continue;
        }
        let mut compressed = Vec::new();
        collect_files_with_extension(&sessions, "zstd", &mut compressed);
        files.extend(compressed.into_iter().filter(|path| {
            path.file_name().and_then(|name| name.to_str()) == Some("session.jsonl.zstd")
        }));

        let mut raw = Vec::new();
        collect_files_with_extension(&sessions, "jsonl", &mut raw);
        files.extend(raw.into_iter().filter(|path| {
            path.file_name().and_then(|name| name.to_str()) == Some("session.jsonl")
        }));
    }
    files.sort();
    files.dedup();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use ccusage_test_support::{EnvVarsGuard, fs_fixture};

    use super::*;

    #[test]
    fn discovers_raw_and_compressed_session_logs_only() {
        let fixture = fs_fixture!({
            "sessions/project/raw/session.jsonl": "{}\n",
            "sessions/project/compressed/session.jsonl.zstd": "{}\n",
            "sessions/project/other/events.jsonl": "{}\n",
            "sessions/project/other/session.jsonl.bak": "{}\n",
        });
        let _guard = EnvVarsGuard::set_many([(
            DSH_HOME_ENV,
            Some(OsString::from(fixture.root().as_os_str())),
        )]);

        let files = discover_session_files().unwrap();

        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|path| path.ends_with("session.jsonl")));
        assert!(
            files
                .iter()
                .any(|path| path.ends_with("session.jsonl.zstd"))
        );
    }

    #[test]
    fn blank_home_falls_back_to_default_home() {
        let fixture = fs_fixture!({
            ".dsh/sessions/project/session/session.jsonl": "{}\n",
        });
        let home = OsString::from(fixture.root().as_os_str());
        let _guard = EnvVarsGuard::set_many([
            (DSH_HOME_ENV, Some(OsString::from("  "))),
            ("HOME", Some(home.clone())),
            ("USERPROFILE", Some(home)),
        ]);

        assert_eq!(discover_session_files().unwrap().len(), 1);
    }
}
