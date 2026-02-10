# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
