use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use jiff::tz::TimeZone as JiffTimeZone;
use serde::Deserialize;

use ccusage_adapter_common::jsonl;

use crate::{
    LoadedEntry, PricingMap, Result, TimestampMs, TokenUsageRaw, UsageEntry, UsageMessage,
    calculate_cost_for_usage,
    cli::{CostMode, SharedArgs},
    fast::LinePrefilter,
    format_date_tz, format_rfc3339_millis, missing_pricing_model_for_candidates, parse_tz,
};

const DEFAULT_DSH_MODEL: &str = "unknown";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DshHeaderLine {
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    r#type: Option<String>,
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    id: Option<String>,
    #[serde(default, deserialize_with = "jsonl::lenient_i64")]
    created_at: Option<i64>,
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DshRecord {
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    r#type: Option<String>,
    #[serde(default, deserialize_with = "jsonl::lenient_i64")]
    time: Option<i64>,
    #[serde(default, deserialize_with = "jsonl::lenient_object")]
    data: Option<DshData>,
}

#[derive(Debug, Deserialize)]
struct DshData {
    #[serde(default, deserialize_with = "jsonl::lenient_i64")]
    turn: Option<i64>,
    #[serde(default, deserialize_with = "jsonl::lenient_i64")]
    step: Option<i64>,
    #[serde(default, deserialize_with = "jsonl::lenient_object")]
    chunk: Option<DshChunk>,
    #[serde(default, deserialize_with = "jsonl::lenient_object")]
    message: Option<DshMessage>,
    #[serde(default, deserialize_with = "jsonl::lenient_object")]
    usage: Option<DshUsage>,
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    provider: Option<String>,
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    model: Option<String>,
    #[serde(default, deserialize_with = "jsonl::lenient_object")]
    header: Option<DshRequestHeader>,
}

#[derive(Debug, Deserialize)]
struct DshRequestHeader {
    #[serde(default, deserialize_with = "jsonl::lenient_object")]
    config: Option<DshRoute>,
}

#[derive(Clone, Debug, Deserialize)]
struct DshRoute {
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    provider: Option<String>,
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DshChunk {
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    r#type: Option<String>,
    #[serde(default, deserialize_with = "jsonl::lenient_object")]
    usage: Option<DshUsage>,
}

#[derive(Debug, Deserialize)]
struct DshMessage {
    #[serde(default, deserialize_with = "jsonl::lenient_object")]
    source: Option<DshRoute>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DshUsage {
    #[serde(default, deserialize_with = "jsonl::lenient_u64")]
    input_tokens: u64,
    #[serde(default, deserialize_with = "jsonl::lenient_u64")]
    output_tokens: u64,
    #[serde(default, deserialize_with = "jsonl::lenient_u64")]
    cache_read_tokens: u64,
    #[serde(default, deserialize_with = "jsonl::lenient_u64")]
    cache_write_tokens: u64,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct StepKey {
    turn: i64,
    step: i64,
}

#[derive(Clone, Debug)]
struct DshSample {
    timestamp: TimestampMs,
    usage: TokenUsageRaw,
    route: Option<DshRoute>,
    finalized: bool,
}

pub(super) fn read_session_file(
    file: &Path,
    shared: &SharedArgs,
    pricing: &PricingMap,
) -> Result<Vec<LoadedEntry>> {
    let fallback = file_timestamp(file);
    let bytes = fs::read(file)?;
    let content = if file.extension().and_then(|value| value.to_str()) == Some("zstd") {
        zstd::stream::decode_all(bytes.as_slice())?
    } else {
        bytes
    };
    let Some(header) = jsonl::records::<DshHeaderLine>(&content, None)
        .next()
        .filter(|header| header.r#type.as_deref() == Some("session"))
        .filter(|header| header.id.is_some())
    else {
        return Ok(Vec::new());
    };
    let session_id = header.id.unwrap_or_default();
    let project_path = header.cwd.unwrap_or_else(|| "unknown".to_string());
    let created_at = header
        .created_at
        .filter(|value| *value > 0)
        .map(TimestampMs::from_millis)
        .unwrap_or(fallback);
    // Delta chunks do not carry usage. The `usage` marker keeps large streamed
    // responses out of serde while retaining usage chunks, route changes, and
    // finalized assistant messages.
    let prefilter = LinePrefilter::any(&[
        br#""request/header""#,
        br#""request/context""#,
        br#""assistant/message""#,
        br#""usage""#,
    ]);
    let mut route = None;
    let mut samples = BTreeMap::<StepKey, DshSample>::new();
    for record in jsonl::records::<DshRecord>(&content, Some(&prefilter)) {
        let Some(data) = record.data.as_ref() else {
            continue;
        };
        match record.r#type.as_deref() {
            Some("request/header") => {
                route = data
                    .header
                    .as_ref()
                    .and_then(|header| valid_route(header.config.clone()));
            }
            Some("request/context") => {
                route = valid_route(Some(DshRoute {
                    provider: data.provider.clone(),
                    model: data.model.clone(),
                }));
            }
            Some("assistant/chunk") => {
                if data
                    .chunk
                    .as_ref()
                    .and_then(|chunk| chunk.r#type.as_deref())
                    != Some("usage")
                {
                    continue;
                }
                let Some(usage) = data.chunk.as_ref().and_then(|chunk| chunk.usage) else {
                    continue;
                };
                let Some(key) = step_key(data) else {
                    continue;
                };
                upsert_sample(
                    &mut samples,
                    key,
                    DshSample {
                        timestamp: event_timestamp(record.time, created_at),
                        usage: usage.into(),
                        route: route.clone(),
                        finalized: false,
                    },
                );
            }
            Some("assistant/message") => {
                let Some(key) = step_key(data) else {
                    continue;
                };
                let message_route = data
                    .message
                    .as_ref()
                    .and_then(|message| valid_route(message.source.clone()));
                if let Some(usage) = data.usage {
                    upsert_sample(
                        &mut samples,
                        key,
                        DshSample {
                            timestamp: event_timestamp(record.time, created_at),
                            usage: usage.into(),
                            route: message_route.or_else(|| route.clone()),
                            finalized: true,
                        },
                    );
                } else if let Some(sample) = samples.get_mut(&key)
                    && let Some(message_route) = message_route
                {
                    sample.route = Some(message_route);
                }
            }
            _ => {}
        }
    }

    let tz = parse_tz(shared.timezone.as_deref());
    Ok(samples
        .into_iter()
        .filter_map(|(key, sample)| {
            let usage = sample.usage;
            if usage.input_tokens == 0
                && usage.output_tokens == 0
                && usage.cache_read_input_tokens == 0
                && usage.cache_creation_input_tokens == 0
            {
                return None;
            }
            Some(to_loaded_entry(
                &session_id,
                &project_path,
                key,
                sample,
                tz.as_ref(),
                shared.mode,
                pricing,
            ))
        })
        .collect())
}

fn step_key(data: &DshData) -> Option<StepKey> {
    let turn = data.turn?;
    let step = data.step?;
    if turn < 0 || step < 0 {
        return None;
    }
    Some(StepKey { turn, step })
}

fn valid_route(route: Option<DshRoute>) -> Option<DshRoute> {
    route.filter(|route| route.provider.is_some() && route.model.is_some())
}

fn upsert_sample(
    samples: &mut BTreeMap<StepKey, DshSample>,
    key: StepKey,
    mut candidate: DshSample,
) {
    if let Some(existing) = samples.get(&key) {
        if existing.finalized && !candidate.finalized {
            return;
        }
        if candidate.route.is_none() {
            candidate.route = existing.route.clone();
        }
    }
    samples.insert(key, candidate);
}

fn event_timestamp(time: Option<i64>, fallback: TimestampMs) -> TimestampMs {
    time.filter(|value| *value > 0)
        .map(TimestampMs::from_millis)
        .unwrap_or(fallback)
}

fn to_loaded_entry(
    session_id: &str,
    project_path: &str,
    key: StepKey,
    sample: DshSample,
    tz: Option<&JiffTimeZone>,
    mode: CostMode,
    pricing: &PricingMap,
) -> LoadedEntry {
    let model = sample
        .route
        .as_ref()
        .and_then(|route| route.model.clone())
        .unwrap_or_else(|| DEFAULT_DSH_MODEL.to_string());
    let provider = sample
        .route
        .as_ref()
        .and_then(|route| route.provider.as_deref())
        .unwrap_or_default();
    let cost = calculate_dsh_cost(provider, &model, sample.usage, mode, pricing);
    let missing_pricing_model = missing_dsh_pricing(provider, &model, sample.usage, mode, pricing);
    let timestamp_text = format_rfc3339_millis(sample.timestamp);
    let id = format!("dsh:{session_id}:{}:{}", key.turn, key.step);
    let data = UsageEntry {
        session_id: Some(session_id.to_string()),
        timestamp: timestamp_text,
        version: None,
        message: UsageMessage {
            usage: sample.usage,
            model: Some(model.clone()),
            id: Some(id),
        },
        cost_usd: None,
        request_id: None,
        is_api_error_message: None,
        is_sidechain: None,
    };
    LoadedEntry {
        data,
        timestamp: sample.timestamp,
        date: format_date_tz(sample.timestamp, tz),
        project: Arc::from("dsh"),
        session_id: Arc::from(session_id),
        project_path: Arc::from(project_path),
        cost,
        extra_total_tokens: 0,
        credits: None,
        message_count: None,
        model: Some(model),
        usage_limit_reset_time: None,
        missing_pricing_model,
    }
}

fn calculate_dsh_cost(
    provider: &str,
    model: &str,
    usage: TokenUsageRaw,
    mode: CostMode,
    pricing: &PricingMap,
) -> f64 {
    for candidate in dsh_model_candidates(provider, model) {
        if mode == CostMode::Display || pricing.find(&candidate).is_some() {
            return calculate_cost_for_usage(Some(&candidate), usage, None, mode, Some(pricing));
        }
    }
    0.0
}

fn missing_dsh_pricing(
    provider: &str,
    model: &str,
    usage: TokenUsageRaw,
    mode: CostMode,
    pricing: &PricingMap,
) -> Option<String> {
    if mode == CostMode::Display {
        return None;
    }
    missing_pricing_model_for_candidates(
        model,
        dsh_model_candidates(provider, model),
        crate::total_usage_tokens(usage),
        Some(pricing),
    )
}

fn dsh_model_candidates(provider: &str, model: &str) -> Vec<String> {
    let mut candidates = Vec::with_capacity(2);
    if !provider.is_empty() {
        candidates.push(format!("{provider}/{model}"));
    }
    candidates.push(model.to_string());
    candidates.dedup();
    candidates
}

fn file_timestamp(file: &Path) -> TimestampMs {
    fs::metadata(file)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| TimestampMs::from_millis(duration.as_millis().min(i64::MAX as u128) as i64))
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|duration| {
                    TimestampMs::from_millis(duration.as_millis().min(i64::MAX as u128) as i64)
                })
                .unwrap_or(TimestampMs::UNIX_EPOCH)
        })
}

impl From<DshUsage> for TokenUsageRaw {
    fn from(usage: DshUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_input_tokens: usage.cache_write_tokens,
            cache_read_input_tokens: usage.cache_read_tokens,
            speed: None,
            cache_creation: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_samples_replace_streaming_samples_but_not_the_reverse() {
        let key = StepKey { turn: 1, step: 1 };
        let mut samples = BTreeMap::new();
        upsert_sample(
            &mut samples,
            key,
            DshSample {
                timestamp: TimestampMs::from_millis(1),
                usage: TokenUsageRaw {
                    input_tokens: 10,
                    ..TokenUsageRaw::default()
                },
                route: None,
                finalized: false,
            },
        );
        upsert_sample(
            &mut samples,
            key,
            DshSample {
                timestamp: TimestampMs::from_millis(2),
                usage: TokenUsageRaw {
                    input_tokens: 20,
                    ..TokenUsageRaw::default()
                },
                route: None,
                finalized: true,
            },
        );
        upsert_sample(
            &mut samples,
            key,
            DshSample {
                timestamp: TimestampMs::from_millis(3),
                usage: TokenUsageRaw {
                    input_tokens: 30,
                    ..TokenUsageRaw::default()
                },
                route: None,
                finalized: false,
            },
        );

        assert_eq!(samples[&key].usage.input_tokens, 20);
    }

    #[test]
    fn provider_qualified_model_is_tried_before_raw_model() {
        assert_eq!(
            dsh_model_candidates("together", "deepseek-ai/DeepSeek-V4-Flash-0731"),
            vec![
                "together/deepseek-ai/DeepSeek-V4-Flash-0731",
                "deepseek-ai/DeepSeek-V4-Flash-0731"
            ]
        );
    }
}
