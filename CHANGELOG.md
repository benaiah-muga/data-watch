# Changelog

## 1.0.0 - 2026-10-16

### Fixed

- Count bytes already transferred on newly observed TCP connections while keeping startup and interface-change counters baselined.
- Preserve unnamed socket baselines so discovering an owner later does not add traffic from before monitoring began.
- Replace the optional NetHogs adapter with native continuous TCP and UDP packet collection, including QUIC and transfers shorter than the dashboard polling interval.
- Use cumulative capture frames with sequence numbers and restart baselines to tolerate dropped IPC messages without duplicate usage.

### Added

- A root-owned systemd capture helper that starts at boot, stays running after logout, and keeps the dashboard and database unprivileged.
- Explicit packet tracking versus limited TCP mode, capture-loss reporting, and the date packet tracking began. Earlier incomplete history is preserved.
