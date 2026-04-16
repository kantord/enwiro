# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
