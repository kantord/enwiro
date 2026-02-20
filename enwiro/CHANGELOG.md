# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
