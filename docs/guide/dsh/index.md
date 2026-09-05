# DeepSeek Harness Data Source

ccusage can read DeepSeek Harness (`dsh`) session logs as a supported local data source. It uses the same daily, monthly, and session report shapes as the other focused sources and includes DSH rows in unified reports.

## Focused Views

```bash
# Daily usage
ccusage dsh daily

# Monthly usage
ccusage dsh monthly

# Usage grouped by DSH session
ccusage dsh session
```

When a report period contains more than one model, the table automatically
adds one row per model with its own token and cost totals. Use
`--breakdown` when you also want a model row for periods that used only one
model. JSON reports always include the same data under `modelBreakdowns`.

## Data Source

The adapter reads session logs below `${DSH_HOME:-~/.dsh}/sessions/`. DSH normally writes checksummed Zstandard frames to `session.jsonl.zstd`; it can also write plain `session.jsonl` when compression is disabled.

```text
${DSH_HOME:-~/.dsh}/
└── sessions/
    └── --<normalized-cwd>--/
        └── <encoded-session-id>/
            ├── session.jsonl.zstd
            └── session.jsonl
```

`DSH_HOME` may contain a comma-separated list of roots when usage from more than one DSH home needs to be combined. Missing roots are skipped.

## Accounting

- `assistant/chunk` records whose chunk type is `usage` provide an early usage sample.
- `assistant/message.usage` is the final sample for the same turn and step when present.
- The adapter keeps one latest sample per `(turn, step)` and lets the finalized message replace the streaming sample, matching DSH's token meter behavior.
- `inputTokens`, `cacheReadTokens`, `cacheWriteTokens`, and `outputTokens` map to ccusage's uncached input, cache read, cache creation, and output buckets.
- DSH's `reasoningTokens` is metadata within the output accounting and is not added a second time.
- Provider-qualified model names are tried before raw model names for pricing lookup. `--mode display` reports usage without requiring pricing data.

The adapter counts usage events, not compaction summaries, tool events, or packed text/reasoning delta rows. Failed requests that have no usage event are not counted.

## Environment Variables

| Variable | Description |
| --- | --- |
| `DSH_HOME` | Override the DSH home, or provide comma-separated DSH homes |
| `LOG_LEVEL` | Adjust verbosity (`0` silent through `5` trace) |

## Configuration

The `dsh` namespace supports the shared report options:

```json
{
	"dsh": {
		"defaults": {
			"offline": true
		},
		"commands": {
			"session": {
				"json": true
			}
		}
	}
}
```

The data root itself is selected by `DSH_HOME`, not by the ccusage configuration file.

## Related Guides

- [All Sources](/guide/all-reports) - Include DSH with every detected source
- [Session Usage](/guide/session-reports) - Compare conversation-level usage
- [Environment Variables](/guide/environment-variables) - Configure data roots
