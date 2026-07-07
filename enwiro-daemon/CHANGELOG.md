# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.16](https://github.com/kantord/enwiro/compare/enwiro-daemon-v0.0.15...enwiro-daemon-v0.0.16) - 2026-07-07

### Added

- add experimental web gui ([#692](https://github.com/kantord/enwiro/pull/692))
- add a global OCI-runtime override for microVM isolation via krun ([#694](https://github.com/kantord/enwiro/pull/694))
- let envs declare a main_folder ([#688](https://github.com/kantord/enwiro/pull/688))

### Fixed

- mount a git worktree's real path alongside its env symlink ([#690](https://github.com/kantord/enwiro/pull/690))
- *(deps)* update rust crate rand to 0.10 ([#684](https://github.com/kantord/enwiro/pull/684))

### Other

- *(deps)* pin dependencies ([#698](https://github.com/kantord/enwiro/pull/698))

## [0.0.15](https://github.com/kantord/enwiro/compare/enwiro-daemon-v0.0.14...enwiro-daemon-v0.0.15) - 2026-07-05

### Added

- harden claude-in-container auth and go Podman-only ([#683](https://github.com/kantord/enwiro/pull/683))
- run isolated containers as the host user ([#682](https://github.com/kantord/enwiro/pull/682))
- allow running claude from an isolated shell
- bind claude auth proxy to teh container bridge ([#678](https://github.com/kantord/enwiro/pull/678))

### Fixed

- bind-mount a git worktree's main repo into the container ([#685](https://github.com/kantord/enwiro/pull/685))
- sanitize environment names before using them as OCI image tags ([#686](https://github.com/kantord/enwiro/pull/686))

## [0.0.14](https://github.com/kantord/enwiro/compare/enwiro-daemon-v0.0.13...enwiro-daemon-v0.0.14) - 2026-07-02

### Added

- authenticate claude in experimental isolated containers ([#673](https://github.com/kantord/enwiro/pull/673))

## [0.0.13](https://github.com/kantord/enwiro/compare/enwiro-daemon-v0.0.12...enwiro-daemon-v0.0.13) - 2026-06-28

### Added

- add experimental container-based isolation (behind feature flag) ([#662](https://github.com/kantord/enwiro/pull/662))

## [0.0.12](https://github.com/kantord/enwiro/compare/enwiro-daemon-v0.0.11...enwiro-daemon-v0.0.12) - 2026-06-28

### Fixed

- *(deps)* update rust crate optative-process-pool to 0.0.4 ([#654](https://github.com/kantord/enwiro/pull/654))

## [0.0.11](https://github.com/kantord/enwiro/compare/enwiro-daemon-v0.0.10...enwiro-daemon-v0.0.11) - 2026-06-06

### Added

- deduplicate equivalent recipes across cookbooks ([#608](https://github.com/kantord/enwiro/pull/608))

## [0.0.10](https://github.com/kantord/enwiro/compare/enwiro-daemon-v0.0.9...enwiro-daemon-v0.0.10) - 2026-06-03

### Added

- automatically mark environments as done ([#589](https://github.com/kantord/enwiro/pull/589))

## [0.0.9](https://github.com/kantord/enwiro/compare/enwiro-daemon-v0.0.8...enwiro-daemon-v0.0.9) - 2026-05-25

### Fixed

- *(deps)* update rust crate optative-process-pool to 0.0.3 ([#532](https://github.com/kantord/enwiro/pull/532))
- validate plugin names ([#529](https://github.com/kantord/enwiro/pull/529))

## [0.0.8](https://github.com/kantord/enwiro/compare/enwiro-daemon-v0.0.7...enwiro-daemon-v0.0.8) - 2026-05-25

### Added

- add `enw mark` command for manual status tracking ([#515](https://github.com/kantord/enwiro/pull/515))

## [0.0.7](https://github.com/kantord/enwiro/compare/enwiro-daemon-v0.0.6...enwiro-daemon-v0.0.7) - 2026-05-24

### Added

- add `enw info --json` ([#509](https://github.com/kantord/enwiro/pull/509))
- pilot IPC for daemon

## [0.0.6](https://github.com/kantord/enwiro/compare/enwiro-daemon-v0.0.5...enwiro-daemon-v0.0.6) - 2026-05-23

### Added

- *(daemon)* unified listen-driven cookbook/adapter pool ([#498](https://github.com/kantord/enwiro/pull/498))

## [0.0.5](https://github.com/kantord/enwiro/compare/enwiro-daemon-v0.0.4...enwiro-daemon-v0.0.5) - 2026-05-20

### Added

- replace show-path with prep ([#478](https://github.com/kantord/enwiro/pull/478))
- allow project-level config overrides ([#469](https://github.com/kantord/enwiro/pull/469))

## [0.0.4](https://github.com/kantord/enwiro/compare/enwiro-daemon-v0.0.3...enwiro-daemon-v0.0.4) - 2026-05-17

### Added

- retain recipe id in EnvStats ([#460](https://github.com/kantord/enwiro/pull/460))

## [0.0.3](https://github.com/kantord/enwiro/compare/enwiro-daemon-v0.0.2...enwiro-daemon-v0.0.3) - 2026-05-17

### Fixed

- *(deps)* update rust crate signal-hook to 0.4 ([#420](https://github.com/kantord/enwiro/pull/420))

## [0.0.2](https://github.com/kantord/enwiro/compare/enwiro-daemon-v0.0.1...enwiro-daemon-v0.0.2) - 2026-05-15

### Added

- move daemon to new binary
