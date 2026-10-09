# China Customs: focused passive evidence review

Reviewed 2026-10-04 UTC. All work and evidence are on the research EC2 devbox.

## Attribution update — 2026-10-05 UTC

The submitter-ID inference below is superseded by [the account attribution audit](urlquery-submitter-attribution-audit.md). The shared ID appears in roughly **930,000 public indexed reports**, including three independently published blocklist/false-positive discussions. It should not be treated as a specific OpenAI or common-operator fingerprint. A historically working urlquery API key is exposed in a report linked by the Transluce ZIP, but the recorded owner is **a different account**. Also, omitted submit.user metadata does not prove anonymous submission: a verified authenticated API submission omits it in public JSON. The Customs actor remains unknown.

## Deeper attribution and recovered-code update — 2026-10-05 UTC

See [the deeper attribution review](china-customs-attribution.md). Twelve scripts, including all four task-specific relay programs, were recovered through the public stored-code viewer and verified against their recorded SHA-256 hashes. The first/final relay navigation programs match after surrounding whitespace removal and share an exact query with two earlier direct scans. Four screenshots were inspected. These observations strengthen workflow linkage but identify no operator, organization, model, or initiating-client IP. All 16 IP addresses in captured HTTP IP objects map to Customs, BrewPage, Google, or Cloudflare destinations. Provider-side submission/publication records remain the key missing evidence.

## Assessment

The public evidence supports an iterative effort to retrieve China Customs trade statistics using URLQuery as a remote browser. It does **not** establish a Chinese-developed model, a particular agent, unauthorized intrusion, or aggressive scraping.

The September session contains **41 distinct scan reports over approximately 20 hours 40 minutes**: 37 direct Customs submissions and four BrewPage versions. Their scanner browsers recorded **766 HTTP entries to stats.customs.gov.cn**, including assets, challenge retries and no-status attempts. Neither number is a count of independent agent decisions.

Two important errors in earlier research are now resolved:

1. Three shared script hashes cited as linking Customs and CDC relay authors are **BrewPage platform code**. Two exactly match iframe sizing and anchor-navigation functions; the third matches ordinary homepage code. These hashes cannot connect operators or models.
2. Data POSTs have conflicting recorded sizes: size=0 and an empty stored-body hash, but size_decoded=2462–2508. The earlier claim that the target delivered no data is unsupported. Whether actual data rows returned remains unknown.

## Scope and reproducibility

I reparsed all 42 disclosed Customs-related raw reports: 41 September reports and one December 2025 precursor linked by account/target. Both pages of the current host search returned 39 distinct IDs, all covered. The BrewPage search returned eight IDs; three older unrelated controls were separately inspected.

The 39-hit host index comprises 37 direct September reports, one September relay that reached Customs, and the December precursor. Three unsuccessful Customs-titled BrewPage versions never requested Customs and therefore are additional to the host-search set.

Only existing public scan indexes/reports and ordinary platform JavaScript were requested. No government endpoint, relay URL or callback was visited; no captured code executed; no new scans submitted.

Audit artifacts:

- evidence/customs_deep_dive/recount.json: 42 deduplicated report rows, raw artifact paths/hashes, and every Customs HTTP entry with timestamp, method, path, status and response metadata.
- evidence/customs_deep_dive/counts.json: counts, units and exact boundaries.
- evidence/customs_deep_dive/platform-fingerprint-proof.json: extracted script hash comparisons.
- evidence/customs_deep_dive/account-controls.json: 12 account/control records.
- evidence/customs_deep_dive/task-fingerprint-search.json: benchmark search.
- evidence/customs_deep_dive/recount.py: repeatable raw parsing logic.

## Timeline and apparent objective

Times below are UTC report timestamps, usually tens of seconds after actual browser requests.

| Time | Observation |
|---|---|
| 2025-12-03 16:19:27 | [Precursor](https://urlquery.net/report/1798627f-1cdc-4575-b3fe-8022b4018b42): English data endpoint with an anti-bot parameter; GET responses include 502 then 400. Same widely reused submitter value and target, not proof of the same operator/task/model. |
| 2026-09-26 20:15:09–21:02:47 | Five direct captures: [query form](https://urlquery.net/report/bae82781-3a23-4889-b46e-14efb34f53e9), then HS 09012100 and location code 31. |
| 2026-09-27 00:42:59–04:18:42 | Eighteen direct captures cycling HS 22082000/22083000 and locations 44/31. Several mistakenly encode all query separators as %26. |
| 01:06:36 | [Direct data URL](https://urlquery.net/report/775bd505-ddc3-4b63-8287-73d83f1135c8) with an anti-bot parameter receives 400. A copied-URL workaround is plausible; site JavaScript can also add such parameters. |
| 02:37:24,02:43:43,02:46:30 | Three BrewPage captures titled “Customs relay” or “Customs popup relay”; no Customs requests recorded. |
| 02:48:19 | [Fourth BrewPage version](https://urlquery.net/report/0fa7b417-9d5e-404c-8c65-b38a9ba18799) navigates to Customs. A no-status navigation and 412 precede 200. Demonstrates execution of the relay and browser challenge flow, not intrusion. |
| 13:22:47–16:55:30 | Fourteen direct captures: HS 04069000/33030000, cache-buster, query-form/country-list/English variants, sorting/page-size changes. |
| 16:51:40 | [Data endpoint invoked with GET](https://urlquery.net/report/ec425dc2-b10a-4fdf-81a8-3c24d3600190) returns 405 with Allow: POST. |
| 16:55:30 | [Last indexed capture](https://urlquery.net/report/c8a766a7-0eac-4d59-b754-9881b8fe46cd) changes ordering to CODE ASC DEFAULT. Not proof activity stopped. |

The HS values are consistent with coffee, brandy, whisky, cheese and perfume. This resembles trade-statistics research, but no original task was recovered. TRADE_CO_PORT carries 31/44; their precise regional meaning was not established and these should not be labeled particular ports.

A saved public 900-row DeepSearchQA CSV contains none of the five exact HS codes, stats.customs, or searched phrases brandy/whisky/Chinese customs/China Customs. This limited negative search does not exclude paraphrases, other benchmarks or ordinary user research.

## Volume and outcomes

| Measurement | Count/window |
|---|---|
| September scan reports |41|
| Direct Customs/BrewPage submissions |37/4; only one BrewPage capture reaches Customs|
| HTTP entries, all hosts/Customs |872/766|
| Customs navigation entries |109, including retries|
| Customs data-endpoint POSTs |25|
| Customs statuses |687 HTTP 200;35 HTTP412; five HTTP400; one HTTP405;38 no status|
| Maximum rolling 60-second Customs HTTP-entry count |59, including assets|
| Report timestamps |2026-09-26T20:15:09Z to 2026-09-27T16:55:30Z|
| HTTP-entry timestamps |2026-09-26T20:14:30.305Z to 2026-09-27T16:55:07.681Z|

Thirty captures finish on the portal title: 25 query-list renders and five form/list renders. Four malformed query submissions are rejected. Two HTTPS forms finish at “Warning: Security Risk” without recorded target status. Three failed BrewPage versions remain on BrewPage. Two direct September data-endpoint submissions receive 400/405.

All 25 page-generated XHR POSTs have HTTP 200 and size_decoded 2462–2508, but size=0, absent body data and SHA-256 of empty stored content. Nonzero decoded metadata prevents inferring an empty server reply; missing bodies prevent establishing successful row retrieval. HTTP 200 alone resolves neither.

No-status HTTP entries do not prove target arrival. Scanner entries are not target-side logs. There is no demonstrated load impact, traffic flood, credential misuse, exploit, nonpublic access or modification. Successful rendering through the site's own browser challenge is insufficient to establish unauthorized access-control bypass. Operator/user instructions are absent, so authority boundaries remain unknown.

## Account relationship and model identity

All 42 Customs reports carry public URLQuery submitter ID akbkyowd9geqr98. It also appears on:

- [June 14 mail.tm script](https://urlquery.net/report/da18c6ca-6fde-4d78-8b7e-7253d01c7ff5) and [URLQuery signup script](https://urlquery.net/report/a058c9e6-8555-4832-aeab-d84ccbbe2dc5).
- [June 21 AIHW Tableau activity](https://urlquery.net/report/1ee69d3e-6629-4ccf-8da5-b8f1f544b3cc), an episode [Transluce](https://transluce.org/agent-activity) links to the OpenAI-acknowledged swarm.
- [September 19 Quidax URL](https://urlquery.net/report/960b3314-f644-4832-a57e-fab108a95678).
- [October 2 CDC WONDER capture](https://urlquery.net/report/77d24491-53e8-4f2c-9257-1f1abcccad43).

This establishes a shared recorded submitter value. The subsequent all-history audit found that it spans roughly 930,000 public reports and independently cited security-checking contexts, so it should not be treated as a specific common-operator or OpenAI fingerprint. Its platform-level meaning and ownership remain unconfirmed.

Controls: unrelated [August BrewPage PDF](https://urlquery.net/report/7ecef470-33b5-41a5-8a13-55f870dd410e) and [July review page](https://urlquery.net/report/1b148fe4-ed95-4194-a9c4-8614cd5ba1a2) have no user field. Other Transluce agent reports likewise have none. The ID is not present in every public JSON export, and BrewPage does not invariably appear under it. Missing user metadata does not establish anonymity: the later attribution audit found an authenticated API submission whose public JSON omits that field. Account ownership and the reason this value is so widely reused remain unresolved.

No DeepSeek, Qwen, Kimi, GLM-version, MiniMax, OpenAI, Claude or Anthropic string occurs in accessible JSON for the 42 Customs reports. Much script/response content is missing, so this is a limited negative observation.

Target IPs are Customs/CDN delivery networks, not submitting-agent location. Firefox settings and reused scanner exit-node IDs are platform characteristics. Chinese target/page language says nothing about acting-model origin.

## Resolved false script fingerprint

The public platform file https://brewpage.app/js/publish.js explains the shared srcdoc hashes:

| SHA-256 | Bytes | Platform code |
|---|---:|---|
|64391b0f1072470f4d991d63616f3fb997fe7bffc5aedd6de5bb3b247552b27b|1579|heightScript: iframe auto-height|
|e734f3844e5a9a9d25b803b4f3318d32dccd84000798720474118e08edfd477a|1221|anchorScript: fragment navigation|
|420f6354509a0b3032bed6314a184ce5304930c3ed563f36abf7e2a9e96ace17|462|Ordinary homepage inline script|

The first two strings were reconstructed by static string-literal parsing, not execution. Their bytes and hashes exactly match scan metadata. The full fetched platform-file hash differs from the captured external-file hash; the proof is the extracted-string matches, not unchanged whole-file bytes.

These are platform-injected scripts, so shared hashes cannot establish relay author, model or coordination. Task-specific relay scripts were subsequently recovered and hash-verified; see the deeper attribution review linked above. The prior CDC control's custom program differs from the Customs program. The shared submitter value remains observed but is not a specific operator fingerprint.

## Remaining evidence gaps

The later attribution review inspected all four relay screenshots: the first three show starting/a blocked popup/a link; the final screenshot renders query-table headers with no visible data rows. This does not resolve whether the server returned data. The public stored-code viewer yielded the relay programs; unauthenticated requests to the documented captured-document API returned 401. No submitted relay or government URL was fetched.

Attribution would require an agent transcript/tool log, trustworthy model identifier linked to a submission, or independent evidence about submitter client separation. No operator outreach was attempted.

The defensible activity label is repeated public-statistics retrieval. Model identity and authorization remain unresolved; serious unauthorized effects are not demonstrated.
