# Live tests for the native providers

The adapters for Anthropic (Messages API) and Google Gemini (Interactions API) are covered by
tests against fake servers that run in the normal suite and never touch the network. On top of
them there are optional **live tests** that call the real APIs with your own key. They are marked
`#[ignore]`, so `cargo test` never runs them by accident, and they spend a few cents.

## Before you start

- Keep each key in a file readable only by you (`chmod 600`). The tests read the file and nothing
  else: the key is never printed, logged or written anywhere.
- Run the tests through `scripts/test.sh`, as every other test.
- Every live test prints its usage and the estimated cost of each request, and fails as soon as the
  spend of a provider goes over the ceiling (see below).

## How to run

All live tests of both providers, one at a time:

```sh
NANTS_LIVE_ANTHROPIC_KEY_FILE=~/path/anthropic.key \
NANTS_LIVE_GEMINI_KEY_FILE=~/path/gemini.key \
scripts/test.sh cargo test --lib -- --ignored --nocapture --test-threads=1 \
  anthropic_live gemini_live native_models_live
```

Only one provider: keep its variable and its filter (`anthropic_live` or `gemini_live`). A single
test: pass its full name as the filter, for example `anthropic_live_cache`.

## Variables

| Variable | Required | What it does |
|---|---|---|
| `NANTS_LIVE_ANTHROPIC_KEY_FILE` | for the Anthropic tests | Path of the file with the Anthropic API key. |
| `NANTS_LIVE_GEMINI_KEY_FILE` | for the Gemini tests | Path of the file with the Gemini API key (AI Studio). |
| `NANTS_LIVE_MAX_USD` | no | Spend ceiling per provider for the whole run, in US$. Default `0.05`. |
| `NANTS_LIVE_MAX_SEARCHES` | no | Web searches allowed per provider for the whole run. Default `3`. |
| `NANTS_LIVE_ANTHROPIC_MODEL`, `NANTS_LIVE_GEMINI_MODEL` | no | Use this model instead of the one picked from the Models API. |
| `NANTS_LIVE_ANTHROPIC_PRICES`, `NANTS_LIVE_GEMINI_PRICES` | only without the price catalog | Known prices as `input,output[,cache read[,cache write]]`, in US$ per million tokens. |
| `NANTS_LIVE_ANTHROPIC_BASE`, `NANTS_LIVE_GEMINI_BASE` | no | Another API address (a company proxy). Default: the official one. |
| `NANTS_LIVE_GEMINI_REQUIRE_CACHE` | no | Turns the missing Gemini implicit cache read into a failure (see below). |

## Models and prices

- **Model:** picked at run time from the provider's Models API, never fixed in the code. Anthropic:
  the most recent Haiku (by `created_at`), with low effort. Gemini: the most recent
  `gemini-<version>-flash` with thinking.
- **Price:** the same public catalog Nants uses for the cost estimate, looked up with the connection
  type, before any model request. Without the catalog (no network to it), the `*_PRICES` variable is
  used; without either, the test fails before spending anything.
- **Cost:** each response's usage (input, cache read, cache write, output with the thinking) and its
  web searches go through the same calculation as the cost shown in Nants. Searches use the price
  per call of Nants (US$ 0.01 by default).

## Spend ceiling

The spend adds up per provider across all tests of the run. After each request the test prints the
request line and the running total; when the total goes over `NANTS_LIVE_MAX_USD`, or the searches
go over `NANTS_LIVE_MAX_SEARCHES`, the test fails and makes no further requests. A later test of the
same provider in the same run fails right away, before any paid request, when the spend has reached
the ceiling or the searches have gone over the limit. Each test ends with the provider total: requests,
input tokens (cache read and written), output tokens (thinking), searches and estimated cost.

## What each test proves

| Test | What it proves |
|---|---|
| `anthropic_live_text_and_tool` | Ping; the `anthropic-ratelimit-*` headers teach the pacer the limits; a tool call and the second turn resending the signed thinking, without and with streaming (same call, thinking in deltas); a request without tools over the tool history; JSON output. |
| `anthropic_live_cache` | A long `system` prefix, new in each run: the first request writes the cache and the second reads it; the cost uses the read and write prices. |
| `anthropic_live_web_search` | One web search (`max_uses: 1`) through the stream, with citations; if the dynamic search is discarded for a bad tool input, the retry notice arrives before the basic search; a second turn, without search, resending the search blocks. |
| `gemini_live_text_and_tool` | Ping; a tool call and the second turn resending the signed `thought` steps, without state; a request without tools over the history; a compacted turn with the bypass signature; JSON; a small image; the tool through the stream (same result, an empty tool result accepted); thinking summary in deltas. |
| `gemini_live_cache` | A long `system_instruction` prefix, the same across runs, in two requests: the implicit cache read shows in `total_cached_tokens` and is charged at the cache price. |
| `gemini_live_web_search` | One Google Search query through the stream: the count follows the `queries`, citations from `url_citation`, signed search steps; a second turn, without search, resending the search steps. |
| `native_models_live` | The model list of both providers, with the limits, without spending tokens. |

### About the Gemini implicit cache

The implicit cache is Google's best effort, with no guarantee: the documentation asks for at least
4,096 tokens on Gemini 3.8 Flash and requests with the same prefix close in time. Seen live on
2026-10-28: within one run, a new prefix was never read, even with pauses of up to 60 s; the stable
prefix was read in a later run already on its first request. That is why the prefix is the same
across runs, and a missing read is a warning in the report, not a failure. Set
`NANTS_LIVE_GEMINI_REQUIRE_CACHE=1` on a second run to make it a failure.

## Expected cost

Measured on 2026-10-28 (`claude-haiku-5-5` and `gemini-3.8-flash`, catalog prices):

| Provider | Requests | Tokens (input / output) | Searches | Estimated cost |
|---|---|---|---|---|
| Anthropic | 11 | 54,973 (13,684 from cache) / 662 | 1 | US$ 0.016 |
| Gemini | 13 | 6,494 (2,056 from cache) / 859 | 1 | US$ 0.017 |

Most of it is the web search of each provider (US$ 0.01 each at the Nants price). Without the
implicit cache read, the Gemini cache test makes two requests at full price, and the Gemini total
goes to about US$ 0.021. The default ceiling of US$ 0.05 per provider leaves room for a model that
thinks longer.

## What the live tests do not provoke

Some paths only show up on an error that costs money or a quota to provoke. The live tests do not
provoke them; the fake-server tests cover the handling:

- the Anthropic `pause_turn` (the server tool loop limit) and its continuation;
- a Gemini 400 that refuses the Google Search tool;
- the Gemini `429` quotas (per minute and per day) and the Anthropic spend limit.
