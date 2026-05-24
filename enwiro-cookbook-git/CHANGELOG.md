# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.18](https://github.com/kantord/enwiro/compare/enwiro-cookbook-git-v0.1.17...enwiro-cookbook-git-v0.1.18) - 2026-05-24

### Other

- updated the following local packages: enwiro-sdk

## [0.1.17](https://github.com/kantord/enwiro/compare/enwiro-cookbook-git-v0.1.16...enwiro-cookbook-git-v0.1.17) - 2026-05-23

### Added

- *(daemon)* unified listen-driven cookbook/adapter pool ([#498](https://github.com/kantord/enwiro/pull/498))

## [0.1.16](https://github.com/kantord/enwiro/compare/enwiro-cookbook-git-v0.1.15...enwiro-cookbook-git-v0.1.16) - 2026-05-20

### Added

- allow project-level config overrides ([#469](https://github.com/kantord/enwiro/pull/469))

## [0.1.15](https://github.com/kantord/enwiro/compare/enwiro-cookbook-git-v0.1.14...enwiro-cookbook-git-v0.1.15) - 2026-05-17

### Other

- updated the following local packages: enwiro-sdk

## [0.1.14](https://github.com/kantord/enwiro/compare/enwiro-cookbook-git-v0.1.13...enwiro-cookbook-git-v0.1.14) - 2026-05-17

### Other

- updated the following local packages: enwiro-sdk

## [0.1.13](https://github.com/kantord/enwiro/compare/enwiro-cookbook-git-v0.1.12...enwiro-cookbook-git-v0.1.13) - 2026-05-13

### Other

- updated the following local packages: enwiro-sdk

## [0.1.12](https://github.com/kantord/enwiro/compare/enwiro-cookbook-git-v0.1.11...enwiro-cookbook-git-v0.1.12) - 2026-05-11

### Other

- move shared types to sdk create ([#353](https://github.com/kantord/enwiro/pull/353))

## [0.1.11](https://github.com/kantord/enwiro/compare/enwiro-cookbook-git-v0.1.10...enwiro-cookbook-git-v0.1.11) - 2026-05-10

### Added

- add basic gear feature (web only) ([#311](https://github.com/kantord/enwiro/pull/311))

## [0.1.10](https://github.com/kantord/enwiro/compare/enwiro-cookbook-git-v0.1.9...enwiro-cookbook-git-v0.1.10) - 2026-04-13

### Fixed

- surface full error chain in notifications and handle branch already checked out

## [0.1.9](https://github.com/kantord/enwiro/compare/enwiro-cookbook-git-v0.1.8...enwiro-cookbook-git-v0.1.9) - 2026-04-03

### Added

- sort recipes globally by per-cookbook importance signal

### Other

- use JSONL format to print recipes

## [0.1.8](https://github.com/kantord/enwiro/compare/enwiro-cookbook-git-v0.1.7...enwiro-cookbook-git-v0.1.8) - 2026-02-20

### Added

- *(cookbook-git)* sort recipes
- add metadata

## [0.1.7](https://github.com/kantord/enwiro/compare/enwiro-cookbook-git-v0.1.6...enwiro-cookbook-git-v0.1.7) - 2026-02-18

### Fixed

- show HEAD branch as a recipe in git cookbook

### Other

- *(cookbook-git)* update documentation

## [0.1.6](https://github.com/kantord/enwiro/compare/enwiro-cookbook-git-v0.1.5...enwiro-cookbook-git-v0.1.6) - 2026-02-14

### Added

- *(cookbook-git)* worktrees on demand for branch-based recipes

### Fixed

- *(cookbook-git)* log worktree discovery errors

## [0.1.5](https://github.com/kantord/enwiro/compare/enwiro-cookbook-git-v0.1.4...enwiro-cookbook-git-v0.1.5) - 2026-02-13

### Added

- *(cookbook-git)* discover git worktrees as separate recipes

## [0.1.4](https://github.com/kantord/enwiro/compare/enwiro-cookbook-git-v0.1.3...enwiro-cookbook-git-v0.1.4) - 2026-02-13

### Added

- extend logging to other binaries

## [0.1.3](https://github.com/kantord/enwiro/compare/enwiro-cookbook-git-v0.1.2...enwiro-cookbook-git-v0.1.3) - 2026-02-11

### Other

- update readme
- add prek
- add missing readme files
- use 2024 rust edition

## [0.1.2](https://github.com/kantord/enwiro/compare/enwiro-cookbook-git-v0.1.1...enwiro-cookbook-git-v0.1.2) - 2026-02-10

### Fixed

- *(deps)* update rust crate git2 to 0.20.0
- *(deps)* update rust crate confy to v2
- *(deps)* update rust crate clap to 4.5.4

### Other

- set up release-plz
- replace panics with anyhow error propagation
- apply auto formatting
