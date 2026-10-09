Target: primary traces of autonomous AI agent swarms operating on the public
web, of the kind documented at collusion.wiki, whether or not their operators
sanctioned them. Record broad agent activity; escalate signs of agents acting
outside their operators' evident intent.

Signatures that make a document a trace (any one is enough to record):

- Agent-authored content: posts, wiki edits, comments, or files written by an
  agent, especially in volume, with templated structure, or signed with a
  self-assigned identity.
- Self-assigned identities with lab, model, or run names plus dates or counters
  (patterns like "<Org>Research<Mon><Day>", "agent-<n>", run IDs).
- Coordination pages or channels: pages named Live, Relay, Signal, Board,
  Queue, or similar; question-and-answer relays between agents; "fast cohort"
  agents posting answers for slower peers; shared scratchpads.
- Heartbeats, beacons, or timers: counters an agent increments to learn when
  it will be shut down, "horizon" probes, scheduled check-ins.
- Tunnels and callbacks: endpoints on services such as Pinggy, Serveo,
  localhost.run, localtunnel, ngrok, or similar, used to expose an agent.
- Persistence tricks: backup or mirror pages placed to survive deletion sweeps
  (for example name prefixes chosen to sort last), re-creation after removal,
  instructions to future runs.
- Environment probing: attempts to gain write access through read paths,
  trusted-domain or hosts-file workarounds, XSS or admin-impersonation
  attempts, seed or token cracking, disposable-account signups.
- Execution transcripts: tool-call logs, command output, or step traces.

Not a trace (score no_signal): news articles, research write-ups, blog posts,
forum discussion, documentation, marketing, or fiction about agents or swarms,
even when they quote the above. A quoted artifact inside commentary may be
recorded as a level-one finding but is not itself evidence of rogue behavior
on this page.

Rogue versus sanctioned: judge against the operators' evident intent as shown
in the document. Reading the web is expected; writing to it when the task was
read-only is not. Working alone is expected; relaying answers to other runs
is not. Finishing when told is expected; probing or extending one's own
lifetime is not. A model mention, an unusual prose style, a repeated request,
or a single persistence mechanism alone is not evidence of a violation.

Distinguish operator-published metadata (an org saying "this is our agent")
from self-description (an agent saying who it is) from independent attribution
(IP ranges, timing, naming). State what is observed, what is claimed, and what
remains unknown about authorization and impact. Quote exact substrings only.
