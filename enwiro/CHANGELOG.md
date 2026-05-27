# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.42](https://github.com/kantord/enwiro/compare/enwiro-v0.3.41...enwiro-v0.3.42) - 2026-05-27

### Added

- improve `enw ls` output format ([#536](https://github.com/kantord/enwiro/pull/536))

### Fixed

- --env flag ignored ([#543](https://github.com/kantord/enwiro/pull/543))

### Other

- add basic docs site ([#539](https://github.com/kantord/enwiro/pull/539))

## [0.3.41](https://github.com/kantord/enwiro/compare/enwiro-v0.3.40...enwiro-v0.3.41) - 2026-05-25

### Fixed

- validate plugin names ([#529](https://github.com/kantord/enwiro/pull/529))

## [0.3.40](https://github.com/kantord/enwiro/compare/enwiro-v0.3.39...enwiro-v0.3.40) - 2026-05-25

### Added

- improve enw run UX ([#527](https://github.com/kantord/enwiro/pull/527))
- auto-mark status on cook and prep ([#526](https://github.com/kantord/enwiro/pull/526))
- add `enw mark` command for manual status tracking ([#515](https://github.com/kantord/enwiro/pull/515))

## [0.3.39](https://github.com/kantord/enwiro/compare/enwiro-v0.3.38...enwiro-v0.3.39) - 2026-05-24

### Added

- add `enw info --json` ([#509](https://github.com/kantord/enwiro/pull/509))
- pilot IPC for daemon

## [0.3.38](https://github.com/kantord/enwiro/compare/enwiro-v0.3.37...enwiro-v0.3.38) - 2026-05-23

### Other

- updated the following local packages: enwiro-sdk, enwiro-sdk, enwiro-daemon

## [0.3.37](https://github.com/kantord/enwiro/compare/enwiro-v0.3.36...enwiro-v0.3.37) - 2026-05-21

### Added

- unify list commands ([#487](https://github.com/kantord/enwiro/pull/487))

## [0.3.36](https://github.com/kantord/enwiro/compare/enwiro-v0.3.35...enwiro-v0.3.36) - 2026-05-20

### Added

- replace show-path with prep ([#478](https://github.com/kantord/enwiro/pull/478))
- allow project-level config overrides ([#469](https://github.com/kantord/enwiro/pull/469))

### Fixed

- *(deps)* update rust crate dialoguer to 0.12 ([#465](https://github.com/kantord/enwiro/pull/465))

## [0.3.35](https://github.com/kantord/enwiro/compare/enwiro-v0.3.34...enwiro-v0.3.35) - 2026-05-19

### Added

- *(enw)* add rm command ([#464](https://github.com/kantord/enwiro/pull/464))

## [0.3.34](https://github.com/kantord/enwiro/compare/enwiro-v0.3.33...enwiro-v0.3.34) - 2026-05-17

### Added

- retain recipe id in EnvStats ([#460](https://github.com/kantord/enwiro/pull/460))
- add command runner feature ([#406](https://github.com/kantord/enwiro/pull/406))

## [0.3.33](https://github.com/kantord/enwiro/compare/enwiro-v0.3.32...enwiro-v0.3.33) - 2026-05-17

### Added

- gate gear entries behind explicit -y confirmation ([#400](https://github.com/kantord/enwiro/pull/400))

### Fixed

- *(deps)* update strum monorepo to 0.28.0 ([#421](https://github.com/kantord/enwiro/pull/421))

## [0.3.32](https://github.com/kantord/enwiro/compare/enwiro-v0.3.31...enwiro-v0.3.32) - 2026-05-17

### Added

- add submodules garnish with autorun hooks ([#398](https://github.com/kantord/enwiro/pull/398))
- add "just" garnish

### Other

- .

## [0.3.31](https://github.com/kantord/enwiro/compare/enwiro-v0.3.30...enwiro-v0.3.31) - 2026-05-13

### Other

- move client and plugin modules into enwiro-sdk
- consolidate atomic_write helper in enwiro-sdk

## [0.3.30](https://github.com/kantord/enwiro/compare/enwiro-v0.3.29...enwiro-v0.3.30) - 2026-05-12

### Added

- replace slow path with explicit failure when daemon is not running ([#355](https://github.com/kantord/enwiro/pull/355))

## [0.3.29](https://github.com/kantord/enwiro/compare/enwiro-v0.3.28...enwiro-v0.3.29) - 2026-05-11

### Other

- move shared types to sdk create ([#353](https://github.com/kantord/enwiro/pull/353))

## [0.3.28](https://github.com/kantord/enwiro/compare/enwiro-v0.3.27...enwiro-v0.3.28) - 2026-05-10

### Added

- allow recipe gear to run linux gui apps ([#344](https://github.com/kantord/enwiro/pull/344))
- add basic gear feature (web only) ([#311](https://github.com/kantord/enwiro/pull/311))

## [0.3.27](https://github.com/kantord/enwiro/compare/enwiro-v0.3.26...enwiro-v0.3.27) - 2026-05-09

### Added

- shorten binary name to enw

## [0.3.26](https://github.com/kantord/enwiro/compare/enwiro-v0.3.25...enwiro-v0.3.26) - 2026-04-16

### Fixed

- *(enwiro-adapter-i3wm)* always place newly activated workspace in shortcut zone

## [0.3.25](https://github.com/kantord/enwiro/compare/enwiro-v0.3.24...enwiro-v0.3.25) - 2026-04-13

### Fixed

- surface full error chain in notifications and handle branch already checked out
- list-all no longer kills user-managed daemon on every invocation

## [0.3.24](https://github.com/kantord/enwiro/compare/enwiro-v0.3.23...enwiro-v0.3.24) - 2026-04-13

### Added

- blend switch and activation signals into slot_score and launcher_score
- daemon runs always, idle only gates recipe cache refresh

### Other

- daemon spawns listen subprocess and records workspace switch events

## [0.3.23](https://github.com/kantord/enwiro/compare/enwiro-v0.3.22...enwiro-v0.3.23) - 2026-04-12

### Other

- add launcher_score and slot_scores wrappers in usage_stats
- replace frecency with percentile slot_score in ManagedEnvInfo

## [0.3.22](https://github.com/kantord/enwiro/compare/enwiro-v0.3.21...enwiro-v0.3.22) - 2026-04-12

### Added

- rank environments by activation percentile in list-all

### Other

- replace frecency fields with decay-based activation_buffer
- group frecency fields into UserIntentSignals struct
- extract atomic_write helper in usage_stats

## [0.3.21](https://github.com/kantord/enwiro/compare/enwiro-v0.3.20...enwiro-v0.3.21) - 2026-04-11

### Added

- always use short workspace id for newly activated enwiro

## [0.3.20](https://github.com/kantord/enwiro/compare/enwiro-v0.3.19...enwiro-v0.3.20) - 2026-04-03

### Added

- kill cache daemon when binary is replaced

## [0.3.19](https://github.com/kantord/enwiro/compare/enwiro-v0.3.18...enwiro-v0.3.19) - 2026-04-03

### Added

- sort recipes globally by per-cookbook importance signal
- only kill daemon based on real user idleness
- *(wrap)* exec() into child instead of babysitting it

### Other

- use JSONL format to print recipes

## [0.3.18](https://github.com/kantord/enwiro/compare/enwiro-v0.3.17...enwiro-v0.3.18) - 2026-02-20

### Added

- do not show recipes that have already been cooked
- add metadata
- add a metadata field
- use fixed sorting for cookbooks

## [0.3.17](https://github.com/kantord/enwiro/compare/enwiro-v0.3.16...enwiro-v0.3.17) - 2026-02-16

### Added

- refresh data more often

## [0.3.16](https://github.com/kantord/enwiro/compare/enwiro-v0.3.15...enwiro-v0.3.16) - 2026-02-15

### Added

- preserve metadata when cooking recipes

### Other

- co-locate metadata with environments

## [0.3.15](https://github.com/kantord/enwiro/compare/enwiro-v0.3.14...enwiro-v0.3.15) - 2026-02-15

### Added

- smart sorting for environments

## [0.3.14](https://github.com/kantord/enwiro/compare/enwiro-v0.3.13...enwiro-v0.3.14) - 2026-02-15

### Added

- add optional description field to recipes

### Fixed

- fix performance issues in wrap command

## [0.3.13](https://github.com/kantord/enwiro/compare/enwiro-v0.3.12...enwiro-v0.3.13) - 2026-02-14

### Added

- *(enwiro)* add background daemon for recipe caching

## [0.3.12](https://github.com/kantord/enwiro/compare/enwiro-v0.3.11...enwiro-v0.3.12) - 2026-02-14

### Fixed

- *(enwiro)* handle slashes in recipe names

## [0.3.11](https://github.com/kantord/enwiro/compare/enwiro-v0.3.10...enwiro-v0.3.11) - 2026-02-13

### Added

- extend logging to other binaries

## [0.3.10](https://github.com/kantord/enwiro/compare/enwiro-v0.3.9...enwiro-v0.3.10) - 2026-02-13

### Added

- add logging

### Fixed

- avoid picking up non-executables as plugins

## [0.3.9](https://github.com/kantord/enwiro/compare/enwiro-v0.3.8...enwiro-v0.3.9) - 2026-02-13

### Added

- add notification

## [0.3.8](https://github.com/kantord/enwiro/compare/enwiro-v0.3.7...enwiro-v0.3.8) - 2026-02-12

### Fixed

- trim adapter output to prevent whitespace in environment names

## [0.3.7](https://github.com/kantord/enwiro/compare/enwiro-v0.3.6...enwiro-v0.3.7) - 2026-02-12

### Added

- add rofi bridge

## [0.3.6](https://github.com/kantord/enwiro/compare/enwiro-v0.3.5...enwiro-v0.3.6) - 2026-02-11

### Fixed

- resolve plugin executables by full path
- avoid need for PATH customization in i3 config

## [0.3.5](https://github.com/kantord/enwiro/compare/enwiro-v0.3.4...enwiro-v0.3.5) - 2026-02-11

### Fixed

- cook environment from adapter name when no explicit name is given
- reject invalid UTF-8 from cookbook subprocess output
- check subprocess exit status in CookbookClient
- write error mesage to stderr, not stdout

### Other

- update readme
- split name resolution from environment lookup internally
- test multiple cookbooks
- replace unwrap anti-pattern with match in get_or_cook_environment
- make more things testable through traits
- add prek
- add missing readme files
- fix typo
- use 2024 rust edition

## [0.3.4](https://github.com/kantord/enwiro/compare/enwiro-v0.3.3...enwiro-v0.3.4) - 2026-02-10

### Added

- add current environment name as env variable

### Fixed

- fix error message
- *(deps)* update rust crate confy to v2
- *(deps)* update strum monorepo to 0.27.0

### Other

- set up release-plz
- replace panics with anyhow error propagation
- *(deps)* update rust crate rstest to 0.26.0
- remove unsafe block for setting env
- *(deps)* update rust crate assertables to v9
- remove deprecated env::home_dir call
- fix clippy issues
- better handling of temporary folders
- apply auto formatting
- update rand crate
- minor style fixes
