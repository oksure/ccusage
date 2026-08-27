# ccusage-adapter-dsh

The DeepSeek Harness (`dsh`) adapter: it turns DSH's append-only session logs
into the usage entries the reports render.

## Owns

- `loader.rs` - reading source files and cross-file deduplication.
- `parser.rs` - zstd/raw decoding, event parsing, token mapping, and pricing.
- `paths.rs` - `DSH_HOME` discovery and session-log selection.
- `report.rs` - the JSON and table row shaping shared by focused reports.

Anything that is not specific to this source belongs in `ccusage-core` or
`ccusage-adapter-common` instead.

## Data source

- `${DSH_HOME:-~/.dsh}/sessions/**/session.jsonl.zstd`
- `${DSH_HOME:-~/.dsh}/sessions/**/session.jsonl` when DSH compression is disabled

The compressed format is a concatenation of independent Zstandard frames. Packed
delta-chunk rows are ignored because usage is recorded separately as
`assistant/chunk` usage events and finalized `assistant/message` events.

## Accounting

DSH's token meter replaces an earlier usage sample when a finalized
`assistant/message` for the same turn and step arrives. The adapter follows the
same rule, so streaming samples are not double-counted. DSH's `reasoningTokens`
is a subset of `outputTokens` for token-meter totals and is not added again.

## Public surface

- `load_entries`
- `report_from_rows`
- `summarize_entries`
- `run`
- `has_data`

## Depends on

- `ccusage-adapter-common`
- `ccusage-core`
- `jiff`
- `serde`
- `serde_json`
- `zstd`
