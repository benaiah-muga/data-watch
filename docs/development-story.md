# Why Data Watch exists

Data Watch began with a practical question: a phone's hotspot report said the connected PC used most of the mobile data, while the visible browser and editor activity did not seem to explain the total. The aim was a small local tool that could show when traffic happened and identify the programs responsible where Linux could provide reliable evidence.

## What made the investigation difficult

Linux exposes several views of network activity, and they do not measure the same thing. Interface counters include all bytes seen by the network interface. Per-process socket counters can miss UDP and QUIC, short connections that begin and end between samples, and traffic owned by protected system processes. Carrier accounting can add its own differences. As a result, a gap between the interface total and the sum attributed to programs is evidence to investigate, not proof that a particular application used those bytes.

The first collector used periodic socket snapshots. That was light on system overhead, but it could not reliably assign brief transfers or UDP traffic to a process. The project moved to a native Linux packet capture helper for continuous TCP and UDP observation. The helper reports aggregate byte counts and process/socket metadata to the ordinary user service. It does not save packet contents, URLs, remote addresses, or command arguments. The privileged part runs as a constrained systemd service, while the dashboard and history database remain unprivileged.

Continuous capture improves attribution, but it cannot reconstruct older gaps or guarantee that every packet maps to a process. Data Watch therefore keeps unattributed traffic visible instead of inventing an owner. It also labels the active collection mode so a user can tell when the more complete helper is running.

## Looking beyond the dashboard

The investigation also considered scheduled system activity. On one Ubuntu setup, package-list refreshes did not follow the same metered-network guard as unattended package upgrades. The local system configuration was adjusted so scheduled APT refreshes defer while a connection is metered or its status is unknown, then run when an unmetered connection is available. This was a machine-specific fix and is not installed or changed by Data Watch. Manual package commands remain available.

This finding identified a plausible source of background traffic, but it did not account for every byte in the historical unattributed total. The old readings cannot be retroactively assigned. The useful next step is to compare new readings with the continuous collector enabled and scheduled package refreshes respecting the metered connection.

## Keeping the tool practical

The dashboard and collector are packaged into one Linux executable, with the interface assets, font, favicon, and SQLite engine embedded. A user-level systemd service starts the monitor at login and keeps it running while the dashboard is closed. Optional continuous packet capture is installed separately with a privileged helper so routine monitoring does not require running the web app as root.

The dashboard is local-only: its HTTP server binds to loopback, its history stays in the user's data directory, and it makes no analytics or cloud requests. The interface grew from a basic usage view into a timeline, program history, directional percentages, and desktop alerts. The latest interface work adds a visible local clock, makes a selected timeline interval update the summary and app list together, provides a direct return to today's readings, and asks the desktop notification service to play its system alert sound. The chart now groups history into calendar days, weeks, months, and years. To keep those views useful without retaining every minute forever, records older than 30 days are compacted into daily traffic and program totals for up to five years. The dashboard also reports the current SQLite database size, including its write-ahead log.

The layout uses responsive breakpoints to stack the header and summary controls on narrow screens and keep the usage cards in two columns. Its main tradeoff is attribution accuracy versus system overhead: interface totals remain the broadest measure, while process attribution is best-effort and depends on the capture mode and Linux metadata available at the time.

Program browsing also needed to work without precise clicks or a long page of rows. Selecting any part of a program row now opens a native dialog, with keyboard focus kept inside the card and returned to the list when it closes. The card uses the selected time interval and reuses history already loaded by the dashboard. The list starts with ten programs and expands into a scrollable view that retains its position during polling. These changes make the details easier to reach without adding network requests for each interaction.


## Coding-agent traffic and disconnected history

A later review found substantial traffic attributed to active coding-agent processes, even after continuous capture was enabled. Session metadata showed many more model/tool rounds than the number of human prompts suggested. We documented [the observations, limits, and practical reductions](coding-agent-data-usage.md) without publishing private session logs or treating tokens as network bytes.

Disconnecting the hotspot also exposed a separate bug: Automatic connection selection cleared the active interface, and history queries used that empty value. The saved records were still present but the dashboard displayed empty totals. The collector now keeps a persistent history interface separately from the current live connection. Disconnection clears live-rate baselines while preserving saved metrics; a restart without a network restores the last history selection. The UI states when it is showing offline history, and exports keep the correct interface label.

Installation now opens the dashboard after checking that the local server is ready, while headless installations print the address. The README lists the Linux, architecture, library, desktop, and service requirements so the single-binary packaging is not mistaken for universal Linux compatibility.


## Watching memory when programs freeze

The next question came from a PC with 16 GB of installed RAM that still sometimes froze. A healthy reading after the event cannot explain an earlier slowdown, so the project gained a separate Memory tab and continuous local history. Collection is independent of the selected network interface and does not need the privileged packet helper.

The existing Rust service reads Linux memory totals, available headroom, swap counters, memory PSI, and readable process status files every six seconds. Available memory is more useful than free memory alone because Linux uses spare RAM for reclaimable cache. PSI adds evidence of tasks waiting for memory, while paging and kernel OOM counters help distinguish pressure from a merely large RAM percentage. None of these readings alone proves why an application froze.

To keep overhead modest, process memory uses RSS from status files rather than scanning memory mappings for PSS. The tradeoff is that shared pages can be counted more than once, so the UI explains why program totals need not match physical RAM use. All live groups are available; stored program history keeps the 20 largest groups per sample in five-minute peak windows. System snapshots are summarized by minute, and both memory tables expire after 30 days. Historical values are peaks and minimum headroom rather than cumulative memory allocations, and gaps remain distinguishable from zero use.

The interface reuses the existing colors, font, responsive cards, and program-dialog pattern. Network and Memory have separate history selections. Moving to Memory hides the network-only header controls and polls only the memory endpoint while both collectors continue in the background. No processes are stopped and no swap or cache settings are changed.
