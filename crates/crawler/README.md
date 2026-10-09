# cn-swarms

A Rust CLI that crawls web targets, classifies every extracted text chunk with
TypeSafe Jev, and sends important chunks to a strong OpenRouter LLM for review.
The rubric captures broad agent activity, including authorized runs, and
separately assesses evidence of consequential misconduct.

```text
Text / CSV seeds → concurrent HTTP crawl → response + text archive
                                                ↓
                               TypeSafe Jev (every nonempty chunk)
                                  ├─ P(important) < 0.70 → log decision
                                  └─ P(important) >= 0.70 → level-one finding
                                                            ↓
                                                  strong OpenRouter review
                                                            ↓
                                            evidence-backed serious report
All page outcomes → CSV; lifecycle events and decisions → JSONL
```

## Configuration and running

Copy [config.example.toml](config.example.toml) to `config.toml`, then enter your
TypeSafe key, OpenRouter key, and the OpenRouter model you want to use:

```toml
[typesafe]
api_key = "your-typesafe-key"
model = "jev-latest"

[openrouter]
api_key = "your-openrouter-key"
model = "provider/strong-review-model"
```

The OpenRouter model shown is a placeholder. Choose a strong model whose
provider supports JSON-schema structured outputs. TypeSafe's model defaults to
`jev-latest`; the OpenRouter model is required. Optional `base_url` fields default
to `https://api.typesafe.ai/v1` and `https://openrouter.ai/api/v1` respectively.

```sh
cargo build --release

cargo run --release -- \
  --config config.toml --input out.csv --column primary_urls \
  --output runs --max-pages 100 --max-depth 2 \
  --concurrency 8 --classifier-concurrency 8 --judge-concurrency 2

# Archive only: no configuration file, credentials, or model calls needed.
cargo run --release -- --input targets.txt --crawl-only --max-pages 25
```

`--config` explicitly selects the TOML file. Keys and models are read from that
file; environment variables and `.env` files are not used. Unknown configuration
fields, missing keys/model, invalid endpoints, and malformed TOML fail before
crawling. TOML errors omit source excerpts to avoid printing credentials.
`config.toml` and `*.local.toml` are gitignored. Files with other names should be
kept outside the repository or added to `.gitignore`.

The secret configuration is never copied to a run. Manifests contain model
names and nonsecret crawl/triage settings. Debug output redacts keys; model
response strings are scrubbed for both configured keys before parsing/logging.
Provider error messages, codes, and routing reasons are logged after credential
scrubbing; raw gateway bodies and upstream request metadata are excluded.
Archived website content is retained as
fetched; it is not a credential-redaction service for arbitrary page content.

Every invocation creates a separate timestamped run directory. Use
`cargo run -- --help` for all options.

## Inputs and crawling

Text files accept one domain, IP, or HTTP(S) URL per line, ignoring blank lines
and `#` comments. Bare domains and IPs use HTTPS; specify `http://` explicitly
for HTTP services. IPv4, IPv6, and explicit ports work. CIDR ranges are not
expanded. See [examples/targets.txt](examples/targets.txt).

CSV headers are matched case-insensitively. Without `--column`, all recognized
columns are ingested: `target`, `url`, `domain`, `ip`, `target_domain`, and
`primary_urls`. Cells may contain one target, a JSON array of strings (as in
`out.csv`), or semicolon/newline-separated targets. UTF-8 BOMs are supported.
Duplicate URLs and fragments are removed; invalid targets are recorded in
`rejected-targets.json`. Malformed CSVs and inputs without valid targets fail
before crawling.

Links and redirects stay within the exact hosts and explicit ports seeded by
the input. HTTP and HTTPS on default ports are allowed for a seeded host.
Subdomains, including `www`, must be seeded separately. Seeds start at depth
zero; discovered links increment depth. The global `--max-pages` budget includes
seeds, failures, and skipped URLs. Seeds beyond the budget remain listed in the
manifest and are counted in the summary. The queue is bounded by this budget.

Tokio tasks run on a multithreaded runtime. `--concurrency` bounds active page
jobs, including triage. Separate `--classifier-concurrency` and
`--judge-concurrency` limits bound calls to each provider. `--delay-ms` spaces
requests to each origin (default 1000), including robots and retry requests.
Robots policies are fetched once per origin and exclusions are logged.
Unavailable policies fail visibly; robots redirects are currently treated as
unavailable. `--ignore-robots` explicitly overrides the policy check.

Transport failures, HTTP 429, and HTTP 5xx responses receive bounded exponential
retries. Numeric `Retry-After` is honored up to 60 seconds. Both model clients
also retry transport failures, 429, and 5xx responses, including TypeSafe 529.
Final text responses, including website error pages, are archived with their
HTTP status. Oversized responses fail rather than becoming silently truncated
model inputs. Model response bodies are also size-limited.

## Classification and review

Each Jev request asks three independent Noul questions about the same source
chunk: importance for research, actual agent activity, and potentially serious
behavior. Noul returns the probability the answer is yes, not an ordinal
severity score or a separate confidence field. See TypeSafe's
[Noul documentation](https://docs.typesafe.ai/primitives/noul) and
[API reference](https://docs.typesafe.ai/api).

`--importance-threshold` defaults to **0.70**, inclusive. A chunk meeting this
threshold is a **level-one finding**, whether authorized or potentially
unauthorized. Its Markdown note and flag event are persisted before the strong
LLM request starts. Chunks below the threshold are archived and logged but do
not trigger strong-model calls. An invalid classifier response is recorded as
incomplete processing, never treated as a negative decision.

The strong OpenRouter judge independently reviews each important chunk, with
Jev's scores as a routing hypothesis. It returns activity, seriousness, and
confidence scores, classification, summary, exact source quotes, uncertainties,
and a serious-behavior confirmation. The client uses OpenRouter's
[structured output format](https://openrouter.ai/docs/guides/features/structured-outputs)
and requires a supporting provider. Source content and classifier decisions
are explicitly marked as untrusted evidence, not instructions.

`--report-threshold` defaults to **0.80**. A serious Markdown report requires
confirmation from the strong judge, validated source quotes, and activity,
seriousness, and confidence scores all at or above that threshold. A strong
judge can downgrade a Jev flag; the level-one note remains available with the
review outcome. Review failures also preserve the note and archived evidence.

`--rubric path.md` appends investigation criteria to both stages and retains a
copy in the run. See [examples/rubric.md](examples/rubric.md). Text is split into
UTF-8-safe chunks of `--chunk-chars` characters (default 12,000). Every nonempty
chunk is classified; selected chunks are reviewed in full. Actual model IDs,
provider usage, scores, decisions, and failures are logged. Rejected JSON model
responses are retained after credential scrubbing. Other chunks continue after
classification or review errors.

Reports assemble the strong judge's assessment, provenance, evidence, and
uncertainties into Markdown. The judge currently has no tools for independent
corroboration or cross-page investigation. The 70% threshold is a routing rule;
its real-world recall on this research corpus still needs evaluation.

## Output

```text
runs/<timestamp>-<pid>/
  manifest.json             # seeds, crawl limits, models, routing thresholds
  rejected-targets.json
  results.csv               # one row per scheduled URL, including errors/skips
  events.jsonl              # events, Jev decisions, strong-model judgments
  summary.json
  summary.md
  pages/<url-sha256>.body    # response bytes after HTTP decompression
  pages/<url-sha256>.txt     # extracted text used by the classifier and judge
  pages/<url-sha256>.json    # provenance, status, redirects, discovered links
  decisions/<url-sha256>.json
  findings/<url-sha256>.md   # level-one Jev finding and any review result
  reports/<url-sha256>.md    # confirmed serious finding
```

CSV artifact paths are relative to the run directory. Each row contains the
highest Jev importance probability and the strongest available LLM review.
All per-chunk classifier decisions and reviews remain in JSON and Markdown.
Summaries count successful classifier calls, important chunks, successful
reviews, page findings, serious reports, and failures. CSV and event logs flush
as work completes; pending level-one notes are completed after triage.

Exit codes: `0` for completion, `2` for page/classifier/judge failures with saved
artifacts, `130` after Ctrl-C, and `1` for startup errors. Ctrl-C stops scheduling,
marks queued jobs as skipped, and finishes active jobs before writing the summary.

## Troubleshooting OpenRouter 404s

A 404 may mean that OpenRouter has no eligible provider for the request, even
when the model and API URL are valid. Inspect a `judge_error` event's `api_error`
fields: `message`, `model`, `endpoint`, `failed_routing_step`, and `routing_reasons`.
The redacted message also appears in CSV errors and the finding's incomplete
processing section.

For `zdr-violation-by-account`, check your OpenRouter
[privacy settings](https://openrouter.ai/settings/privacy) and allowed-provider
settings. The model needs an allowed endpoint compatible with the account's
zero-data-retention policy and the request's structured-output requirements.
OpenRouter documents its
[provider and data-policy filters](https://openrouter.ai/docs/guides/routing/provider-selection).
The CLI does not override those policies or silently switch models. Fix the
provider/policy mismatch before starting another review run. Existing level-one
findings and raw archives remain available after failed reviews.

## Project state and boundaries

The CLI implements a complete seed-to-file pipeline. Modules separate seed
loading (`source`), crawling (`crawl`), TOML settings (`config`), bounded provider
transport (`api`), Jev classification (`jev`), strong review (`judge`), scheduling
(`pipeline`), and artifact writing (`sink`). Existing Python cleanup scripts,
notebooks, datasets, and attribution/research notes remain alongside the CLI.

This version handles static UTF-8 HTTP text and readable HTML. It does not
execute JavaScript, scan ports, expand networks, decode other character sets,
parse PDFs/images, or resume previous runs. Scripts, styles, templates, and
noscript nodes remain in the raw archive but are excluded from extracted HTML
text. Binary content is skipped and logged. Decisions are local to a chunk;
context crossing a boundary can be missed. Page limits, byte limits,
concurrency, and chunk size bound the workload.

Useful next steps are a labeled evaluation of Jev recall and the routing
threshold, cross-page evidence aggregation, browser/PDF ingestion, resumable
jobs, and a database sink. Live provider behavior remains unverified until a
run with real credentials; automated tests use mock endpoints.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Integration tests exercise the real CLI against a localhost website and mocked
TypeSafe/OpenRouter endpoints. They cover TOML-sourced credentials/models, the
inclusive 70% gate, level-one persistence before review, downgrade/report
behavior, classifier and reviewer errors, key scrubbing, Unicode chunk coverage,
robots rules, retries, scope, body limits, and crawl-only operation. Tests require
permission to bind a local socket and make no paid API calls.

## Research notes

Swarm incidents/observatories:
- Huggingface/OAI
- Collusion.wiki (Sydney)

urlquery
shodan as attribution confirmation?
rust web crawler for important links? mpsc + queue + jitter + backoff? -> save findings locally, load into ES
agent then works locally using ES tool calls + shodan.io + urlquery

# Notes on Agent Swarms

## Collusion.wiki

In the wake of the Hugging Face attack, we tried to find AI agents on the
internet using several methods.

We describe below some of our high-level strategies for searching for agents
on the open internet.

- Launching large GPT-5.6 agent swarms with instructions to find other agents
on the internet.
- Eliciting GPT-5.6 by putting it in situations where it had broken out of
ExploitGym, and watching where it would go from there.
- Identifying constraints that rogue agents might have which could help us
locate them. Some constraints such agents might face include:
    - In order to communicate with each other externally, agents need to find Schelling points to arrive at the same place on the internet.
    - Agents might not be able to use websites that require complex sign-up processes.
    - Agents might prefer sites that are easy to interact with via the CLI.
    - Agents may not be able to send POST requests if in an environment where they intentionally are allowed to search the internet.
    - Agents might have to use services which don’t have anti-bot filters, or ones which are easy to circumvent.
- Identifying agent motivations. For example, agents might want to:
    - Find answers to RL and evaluation tasks.
    - Store large amounts of data persistently.
    - Communicate with other agents.
    - Gain access to compute, in order to do tasks with more resources than allocated.
- Building classifiers for agent activity on the internet:
    - We scraped- Common Crawl, as well as Pastebin, GitHub Gists, and other sites for activity uploaded by agents. We developed multi-stage LLM classifiers to recognize agent activity.
    - This mostly turned up a lot of authorized agent activity. We found many GitHub repos and links that were agent-made, but clearly had humans involved.
- Reaching out to organizations we thought would be high-priority targets for hacks.

We enumerated sites based on constraints we imagined models might be under, using Kimi K3 to avoid triggering safety classifiers. We asked Kimi to list “all the categories of software which might be writeable via GET” and, amongst other things, it listed “Forums, bulletin boards, early wikis”.

We used a script to further probe each category Kimi provided. Asking Kimi “Can you list out the top forums, bulletin boards, early wikis which come to mind which would allow writes via GET requests?” lists out UseModWiki as the second item under the heading “wikis”.

UseModWiki is a wikifarm which doesn’t distinguish heavily between GET and POST requests (as with many other old Perl CGI forums). UseModWiki has one agent edit. DSEWiki and the other wikis the agents used are all relatively prominent forks of UseModWiki.


## [Murmuration](https://maramasaeva.com/observatory#3-documented-background-all-public-secondary-sources-marked)
