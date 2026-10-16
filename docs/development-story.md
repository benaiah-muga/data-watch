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
