# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.20](https://github.com/kantord/enwiro/compare/enwiro-adapter-i3wm-v0.1.19...enwiro-adapter-i3wm-v0.1.20) - 2026-05-17

### Added

- add command runner feature ([#406](https://github.com/kantord/enwiro/pull/406))

## [0.1.19](https://github.com/kantord/enwiro/compare/enwiro-adapter-i3wm-v0.1.18...enwiro-adapter-i3wm-v0.1.19) - 2026-05-17

### Fixed

- *(deps)* update rust crate toml to v1 ([#422](https://github.com/kantord/enwiro/pull/422))

## [0.1.18](https://github.com/kantord/enwiro/compare/enwiro-adapter-i3wm-v0.1.17...enwiro-adapter-i3wm-v0.1.18) - 2026-05-17

### Fixed

- *(adapter-i3wm)* avoid empty-workspace race in activate rebalance ([#390](https://github.com/kantord/enwiro/pull/390))

## [0.1.17](https://github.com/kantord/enwiro/compare/enwiro-adapter-i3wm-v0.1.16...enwiro-adapter-i3wm-v0.1.17) - 2026-05-16

### Fixed

- *(adapter-i3wm)* skip unmanaged workspaces in rebalance plan emit
- *(adapter-i3wm)* collapse multi-hop rebalance into one rename

## [0.1.16](https://github.com/kantord/enwiro/compare/enwiro-adapter-i3wm-v0.1.15...enwiro-adapter-i3wm-v0.1.16) - 2026-05-15

### Fixed

- *(adapter-i3wm)* use non-reserved names for rebalance parking

## [0.1.15](https://github.com/kantord/enwiro/compare/enwiro-adapter-i3wm-v0.1.14...enwiro-adapter-i3wm-v0.1.15) - 2026-05-13

### Other

- updated the following local packages: enwiro-sdk

## [0.1.14](https://github.com/kantord/enwiro/compare/enwiro-adapter-i3wm-v0.1.13...enwiro-adapter-i3wm-v0.1.14) - 2026-05-11

### Other

- updated the following local packages: enwiro-sdk

## [0.1.13](https://github.com/kantord/enwiro/compare/enwiro-adapter-i3wm-v0.1.12...enwiro-adapter-i3wm-v0.1.13) - 2026-05-11

### Fixed

- *(adapter-i3wm)* fix swap cycle bug ([#350](https://github.com/kantord/enwiro/pull/350))

## [0.1.12](https://github.com/kantord/enwiro/compare/enwiro-adapter-i3wm-v0.1.11...enwiro-adapter-i3wm-v0.1.12) - 2026-05-10

### Added

- allow recipe gear to run linux gui apps ([#344](https://github.com/kantord/enwiro/pull/344))
- add basic gear feature (web only) ([#311](https://github.com/kantord/enwiro/pull/311))

### Other

- *(adapter-i3wm)* fire gear only on first GUI activation per env ([#343](https://github.com/kantord/enwiro/pull/343))

## [0.1.11](https://github.com/kantord/enwiro/compare/enwiro-adapter-i3wm-v0.1.10...enwiro-adapter-i3wm-v0.1.11) - 2026-05-09

### Added

- shorten binary name to enw

## [0.1.10](https://github.com/kantord/enwiro/compare/enwiro-adapter-i3wm-v0.1.9...enwiro-adapter-i3wm-v0.1.10) - 2026-04-16

### Fixed

- *(enwiro-adapter-i3wm)* always place newly activated workspace in shortcut zone

## [0.1.9](https://github.com/kantord/enwiro/compare/enwiro-adapter-i3wm-v0.1.8...enwiro-adapter-i3wm-v0.1.9) - 2026-04-13

### Added

- blend switch and activation signals into slot_score and launcher_score
- implement 3-element eviction cycle in i3wm adapter

### Other

- add i3 IPC listener for workspace switch events

## [0.1.8](https://github.com/kantord/enwiro/compare/enwiro-adapter-i3wm-v0.1.7...enwiro-adapter-i3wm-v0.1.8) - 2026-04-12

### Added

- replace least-score eviction with NetBenefit swap selection

### Other

- replace frecency with percentile slot_score in ManagedEnvInfo

## [0.1.7](https://github.com/kantord/enwiro/compare/enwiro-adapter-i3wm-v0.1.6...enwiro-adapter-i3wm-v0.1.7) - 2026-04-11

### Added

- always use short workspace id for newly activated enwiro

## [0.1.6](https://github.com/kantord/enwiro/compare/enwiro-adapter-i3wm-v0.1.5...enwiro-adapter-i3wm-v0.1.6) - 2026-02-13

### Added

- extend logging to other binaries

## [0.1.5](https://github.com/kantord/enwiro/compare/enwiro-adapter-i3wm-v0.1.4...enwiro-adapter-i3wm-v0.1.5) - 2026-02-12

### Added

- add rofi bridge

## [0.1.4](https://github.com/kantord/enwiro/compare/enwiro-adapter-i3wm-v0.1.3...enwiro-adapter-i3wm-v0.1.4) - 2026-02-11

### Other

- add note about PATH on i3

## [0.1.3](https://github.com/kantord/enwiro/compare/enwiro-adapter-i3wm-v0.1.2...enwiro-adapter-i3wm-v0.1.3) - 2026-02-11

### Other

- update readme
- use 2024 rust edition

## [0.1.2](https://github.com/kantord/enwiro/compare/enwiro-adapter-i3wm-v0.1.1...enwiro-adapter-i3wm-v0.1.2) - 2026-02-10

### Fixed

- *(deps)* update rust crate clap to 4.5.4
- *(deps)* update rust crate tokio to 1.37.0
- *(deps)* update rust crate clap to 4.5.3

### Other

- set up release-plz
- replace panics with anyhow error propagation
- Add more details to READMEs
- use char instead of string for single char split
