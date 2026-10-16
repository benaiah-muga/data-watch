# Changelog

## 1.3.0 - 2026-10-16

### Added

- Whole-row program details in an accessible popup card with interval usage, live rate, connection share, last observation, and history.
- A ten-program default view with an expandable scrollable list that preserves its position during refreshes.
- A dashboard screenshot and reusable SVG project logo in the README and repository assets.

## 1.2.0 - 2026-10-16

### Added

- Calendar-based chart ranges for seven daily bars, twelve weeks, twelve months, and five years.
- Compact daily traffic and program archives retained for up to five years after detailed minute records expire.
- A live history database size readout that includes SQLite write-ahead log data.

## 1.1.1 - 2026-10-16

### Fixed

- Prevent the mobile live-throughput and connection cards from inheriting desktop flex basis as oversized fixed heights.

## 1.1.0 - 2026-10-16

### Added

- A public development story documenting the hotspot investigation, attribution limits, and design choices.
- A prominent live local-time card in the dashboard header, including the system timezone.
- A selected timeline interval updates connection totals and direction percentages, with a Return to today control for the current readings.
- Daily data alerts and test notifications request the desktop theme's alarm sound.

## 1.0.0 - 2026-10-16

### Fixed

- Count bytes already transferred on newly observed TCP connections while keeping startup and interface-change counters baselined.
- Preserve unnamed socket baselines so discovering an owner later does not add traffic from before monitoring began.
- Replace the optional NetHogs adapter with native continuous TCP and UDP packet collection, including QUIC and transfers shorter than the dashboard polling interval.
- Use cumulative capture frames with sequence numbers and restart baselines to tolerate dropped IPC messages without duplicate usage.

### Added

- A root-owned systemd capture helper that starts at boot, stays running after logout, and keeps the dashboard and database unprivileged.
- Explicit packet tracking versus limited TCP mode, capture-loss reporting, and the date packet tracking began. Earlier incomplete history is preserved.
