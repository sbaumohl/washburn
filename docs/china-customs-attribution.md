# China Customs: deeper operator and IP attribution review

Reviewed 2026-10-05T01:05:47.503759+00:00. Public evidence only.

**Result:** no defensible attribution to a named person, organization, group, AI provider/model, or submitting-client IP. The new evidence substantially strengthens the link among a small set of Customs retrieval attempts, but it does not identify their operator. The behavior remains repeated public-statistics retrieval; the available artifacts do not establish an intrusion or harmful effect.

**Newly recovered evidence.** The raw JSON omitted script bodies, but urlquery's public stored-code viewer retained them. Twelve recovered scripts matched their recorded SHA-256 hashes exactly: four task programs, four publication-metadata snippets, and four platform helper scripts. This supersedes the older brief's statement that the task-specific relay code was unavailable.

| Report time, UTC | BrewPage resource and source report | Recovered behavior |
| --- | --- | --- |
| 2026-09-27T02:37:24Z | [C4v2QvAGjq](https://urlquery.net/report/ee680e22-146c-4db3-95ba-01c0e345e5af) | Construct query parameters and call location.replace; screenshot says starting. |
| 2026-09-27T02:43:43Z | [p7BMv8P881](https://urlquery.net/report/4af932ee-0e8a-41fd-8fa4-30f0dfca92b3) | window.open plus a delayed location.href fallback; screenshot says opened=false. |
| 2026-09-27T02:46:30Z | [OAOJb5303U](https://urlquery.net/report/4f2ab464-dc40-426c-94ed-a6b14138f135) | Timed click of the go link; screenshot says CLICK FOR CUSTOMS. |
| 2026-09-27T02:48:19Z | [x3TRMjHXd0](https://urlquery.net/report/0fa7b417-9d5e-404c-8c65-b38a9ba18799) | Same navigation program as the first relay, published as a site; screenshot renders the Customs query table. |

The first and fourth task programs are byte-identical after removing surrounding whitespace. Their normalized SHA-256 is 298e1fb460607556a3aa12615d02bdd3134b7e568d069141ddb56a2aa7bc127d. The query selects commodity 22082000, region field 44, year 2025, RMB, and page size 100, among other parameters. The popup relay uses the same query as a literal URL. The third script is a 58-byte delayed click; the underlying anchor HTML was not recovered, so its exact URL is not asserted from that script alone.

The navigation query also exactly matches two earlier direct submissions after normal query-string decoding: [01:26:38 UTC](https://urlquery.net/report/5547d15d-5759-4754-ad19-1c9f9b27e1aa) and [02:03:40 UTC](https://urlquery.net/report/13f0351f-843b-4874-a2a0-6df3d4fca132) on September 27. That provides a code-and-task link independent of the common submitter ID. Copying or sharing remains possible; it does not establish one agent or person, or join every September report into one session.

None of the four recovered task programs contains a name, account handle, model identifier, API key, external collection callback, or exploitation payload. They implement navigation/pop-up/link-click attempts. The screenshots show starting, a blocked popup, a link, and finally the public statistics query table. The final screenshot shows table headers but no data rows in its visible area; that does not establish an empty server response or prove that no data was retrieved by other means.

**Publisher identifiers are resource locators.** The metadata gives four public namespace IDs, expiry timestamps, view counts, and html/site types. The first three record isOwner=false for the scanner viewer. No publisher account ID or uploader IP is present. isOwner=false is not a claim that the page lacks an owner. Expiry is not creation time. The site-type final publication explains a useful structural difference from the earlier html-type publications, without identifying who published them.

**IP attribution.** Every one of the 16 addresses in the captured HTTP IP objects is associated with a destination host:

| Network evidence | What it identifies |
| --- | --- |
| Nine addresses associated with stats.customs.gov.cn, including 27.155.113.162 and 36.158.231.195 | Customs delivery/CDN infrastructure. These are not the submitting operator's addresses. |
| **79.132.136.83**, ASN **44051**, Fornex Hosting S.L., recorded GeoIP Germany | The shared BrewPage hosting service used by the relays. This does not identify a German, Russian, Spanish, or other national actor. |
| Four Google addresses and two Cloudflare addresses | Font, analytics, and library destinations. |
| Scanner exit-node identifier **31pu2ilhjrkmwpf** | urlquery scanning infrastructure. The same node appears in 141 of the 1,996 inspected convenience-corpus reports, including the 41 September Customs reports. It is not a client IP or unique actor identifier. |

All 42 reports have empty submission tags/meta, empty configured referrer, and no supplied cookies. Raw request headers contain the scanner's Firefox user agent and ordinary Customs/BrewPage origin/referrer values; no Forwarded, X-Forwarded-For, X-Real-IP, or X-Client-IP header identifies a submitting client. The reports describe scanner-to-site traffic, not the separate client-to-urlquery submission request. The complete address-role table is in evidence/customs-attribution-deep/ip-roles.csv.

**External linkage checks.** Exact public searches for each relay ID returned its known report only. The four task-script SHA-256 searches also returned the corresponding known report only. The two long first/final script hashes differ only because of surrounding whitespace; both were searched. General captured-content queries are less reliable: known scripts with the Customs hostname were omitted from some results. Their negative coverage must not be overstated.

Static decoding across 1,996 preserved raw reports found only the 42 known Customs reports. Twenty-six additional numeric-code matches were rejected because the digits occurred inside unrelated fuzzy-hash strings. The public agent-swarm-research repository (24 files), 200 task records, 45,989 collected URLs, 22,385 DseWiki revisions, and 4,579 prowiki page records supplied no exact Customs host/relay/commodity-code fingerprint match. A separate proximity check for the relevant commodity words near China/Chinese/Shanghai/Guangdong/customs in task/revision/page text also found no match. This is bounded corpus coverage, not a universal exclusion of OpenAI or other operators.

The complete 900-question public DeepSearchQA dataset and 1,266-question public BrowseComp release supplied no matching Customs task in the checks performed. The closest DeepSearchQA coffee question concerns 2018–2021 UN Comtrade exports of a different commodity code and is not a match. Benchmark participation by any model remains unestablished.

GitHub searches for the target found an April anti-scraping discussion, an August soybean-market research PR, and a September ad-blocking rules PR; their available contents/times do not connect them to these reports, relay IDs, or task parameters. These are target mentions, **not suspects**. Exact relay-ID and account-ID GitHub issue searches found no match. Bing returned unrelated results and Google a JavaScript/redirect shell, so general web searching did not establish absence of other public attribution.

The October CDC control's 869-byte custom script was also recovered and hash-verified. It refers to wonder.cdc.gov, has no model name or Customs reference, and is not the Customs program. Previously shared BrewPage helper code remains platform boilerplate, not an operator fingerprint.

**What would resolve the remaining gap.** urlquery can potentially join the exact report IDs to original submission/account/session/API-client records and clarify what the common submitter ID represents. BrewPage can potentially join the four public resources to their publishing records. The meaningful missing fields are original submission/publication timestamps, account or service identity, pseudonymous session/API-key or ownership identity, and trusted ingress source IP/ASN. An external operator transcript naming the same report IDs would also be strong evidence. A source IP could still identify a proxy or shared service rather than a person.

An unsent, concrete request with the exact report/page locators is saved as reports/china-customs-provider-attribution-request.md. No outreach was sent. The documented stored-document API was checked without a key and returned 401 for two known public capture hashes; that limitation was preserved. No target/relay URL, callback, credential, new scan, or captured program was exercised.

Evidence and reproducibility: evidence/customs-attribution-deep/findings.json, recovered-script-manifest.json, script-*.txt, relay-publisher-metadata.json, workflow-joins.json, ip-and-header-audit.json, ip-roles.csv, decoded-corpus-pivots.json, external-corpus-search-summary.json, semantic-task-search.json, cdc-custom-code-comparison.json, documented-resource-api.json, exact search captures, and four preserved screenshots.
