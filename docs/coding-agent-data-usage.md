# Why coding agents can use so much mobile data

This investigation began when Data Watch attributed almost 400 MB to Codex during a day of coding over a phone hotspot. There were no long video streams to explain it. The lesson was that an editor can look quiet while its coding agent is exchanging substantial amounts of context with a remote model.

These are observations from one Ubuntu machine on October 16, 2026, not a benchmark of every agent or an estimate of anyone else's daily usage. MB here means decimal megabytes; the dashboard uses MiB and GiB.

## What we observed

- The grouped Codex process total rose from about 390 MB to about 403 MB during the review. Both downloads and uploads contributed; it was not just downloading updates.
- Most of the recorded amount was collected after continuous TCP/UDP capture was enabled. Earlier incomplete socket estimates could not explain the whole total.
- A short independent Linux TCP-counter check also observed traffic on the active agent's encrypted model-service connections. TCP payload counters and Data Watch's IP-byte counters have different overhead and sampling boundaries, so they should not match exactly.
- Local session metadata showed roughly 1,400 model responses and a similar number of tool calls across multiple coding threads and subagents. That is a much heavier workload than a few human chat messages suggest.
- Large tool results, screenshots, long conversations, and frequent short command polls appeared in the work. These are plausible contributors, but encrypted traffic and grouped process history do not provide an exact byte breakdown per thread or feature.
- The recent startup logs did not establish a retry storm as the explanation. That limited check does not rule out retries earlier in the day.

The raw diagnostics stay private. This public summary omits session identifiers, conversation contents, credentials, and connection details. Data Watch itself stores program names and aggregate byte counts, not those session logs.

## Why the bytes add up

A coding agent repeatedly asks a model what to do, runs tools, sends results back, and receives another response. Reading a large file, inspecting a build log, or taking a screenshot can add material to subsequent requests. Multiple active threads and subagents create more exchanges. A command that runs locally does not itself need cloud traffic, but sending its output back to the model can.

How much context is transmitted depends on the client and transport. Some implementations resend conversation context, while others can reference server-side state and send incremental input. OpenAI documents this distinction in its [Responses WebSocket guide](https://developers.openai.com/api/docs/guides/websocket-mode). That API capability alone does not establish which transport a particular installed Codex version used for every request.

Token totals are not network-byte totals. Cached input tokens describe model processing and billing; they do not prove that those bytes avoided the mobile connection. Local log size is also not a measure of transmitted bytes. OpenAI's [production guidance](https://developers.openai.com/api/docs/guides/production-best-practices#compress-request-bodies) explicitly distinguishes request compression, which reduces network bytes, from token usage. Request compression was already enabled in the inspected Codex installation, so enabling it again was not a fix.

Other cloud agents can have the same broad pattern. For example, [Claude Code's usage guidance](https://code.claude.com/docs/en/costs#why-usage-climbs-in-a-long-session) describes repeated conversation context, tool results, and separate subagent requests. That explains a mechanism, not a comparable MB-per-task result. We did not run a controlled cross-agent bandwidth benchmark and cannot promise another hosted agent will use less data.

Several hundred MB is plausible for a long, busy agent workload. It would still deserve investigation during genuinely idle or light use. There is no universal normal daily allowance established by these observations.

## Practical changes to try

1. Give the agent bounded tasks and clear completion criteria. Stop work that is no longer needed.
2. Keep one active coding thread when parallel work is unnecessary. Additional agents should have a purpose worth their traffic.
3. Start a fresh thread at a natural milestone with a concise handoff summary rather than carrying unrelated history indefinitely.
4. Ask for focused file reads and concise command output. Batch independent checks and let long commands run before checking again, instead of repeatedly polling them after a second.
5. Use screenshots when visual evidence helps. Avoid repeatedly uploading large, unchanged images or logs.
6. Keep supported compression enabled. Do not assume cheaper cached tokens mean lower mobile-data use, or turn on obsolete transport flags.
7. Compare comparable workloads in Data Watch, recording both directions and the capture mode. Keep package updates and large initial downloads on unmetered Wi-Fi.

A fully local model can reduce cloud inference traffic once downloaded, but model downloads may be large and local speed and quality depend on the hardware. No local-model suitability assessment was part of this investigation.

## What Data Watch can and cannot prove

The connection total includes more than any one agent. Program attribution is best-effort and may group several instances under one name. IP headers, retransmissions, local-network transfers, VPNs, and dropped packets can affect comparisons with carrier billing. An unattributed gap is not evidence that Codex or another named program owns it.

Use the timeline and program history to locate the busy period, then investigate active processes and schedules. Data Watch cannot reconstruct a historical encrypted request, identify an editor extension sharing another process, or recover ownership that was never captured. Investigating with a cloud coding agent also generates traffic of its own, so keep the investigation focused.
