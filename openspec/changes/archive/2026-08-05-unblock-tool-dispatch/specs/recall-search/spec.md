# recall-search Delta

## ADDED Requirements

### Requirement: Eager index build is observable in logs
The system SHALL emit an INFO-level log line when the eager recall index build
starts and an INFO-level log line when it completes. The completion line SHALL
carry the effective backend, the number of scope indexes built, and the elapsed
build duration, so operators can monitor build time against their startup-probe
budget as the vault grows.

#### Scenario: Build start and completion are logged with duration
- **WHEN** the server starts with recall enabled and the eager index build runs
  over the vault
- **THEN** the log contains an INFO line marking the build start, followed by an
  INFO line marking readiness that includes the backend name, the scope count,
  and the elapsed duration of the build

#### Scenario: No build logs when recall is off
- **WHEN** the server starts with `MUNINN_RECALL_BACKEND=off`
- **THEN** no index-build start or completion lines are emitted
