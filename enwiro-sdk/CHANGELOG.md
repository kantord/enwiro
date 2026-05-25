# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0](https://github.com/kantord/enwiro/compare/enwiro-sdk-v0.4.2...enwiro-sdk-v0.5.0) - 2026-05-25

### Added

- add `enw mark` command for manual status tracking ([#515](https://github.com/kantord/enwiro/pull/515))

## [0.4.2](https://github.com/kantord/enwiro/compare/enwiro-sdk-v0.4.1...enwiro-sdk-v0.4.2) - 2026-05-24

### Added

- add `enw info --json` ([#509](https://github.com/kantord/enwiro/pull/509))
- pilot IPC for daemon

## [0.4.1](https://github.com/kantord/enwiro/compare/enwiro-sdk-v0.4.0...enwiro-sdk-v0.4.1) - 2026-05-23

### Added

- *(daemon)* unified listen-driven cookbook/adapter pool ([#498](https://github.com/kantord/enwiro/pull/498))

## [0.4.0](https://github.com/kantord/enwiro/compare/enwiro-sdk-v0.3.1...enwiro-sdk-v0.4.0) - 2026-05-20

### Added

- allow project-level config overrides ([#469](https://github.com/kantord/enwiro/pull/469))

### Other

- *(sdk)* replace shell-script fixtures with prebuilt fake-plugin  ([#479](https://github.com/kantord/enwiro/pull/479))

## [0.3.1](https://github.com/kantord/enwiro/compare/enwiro-sdk-v0.3.0...enwiro-sdk-v0.3.1) - 2026-05-17

### Added

- add command runner feature ([#406](https://github.com/kantord/enwiro/pull/406))

## [0.3.0](https://github.com/kantord/enwiro/compare/enwiro-sdk-v0.2.3...enwiro-sdk-v0.3.0) - 2026-05-17

### Added

- gate gear entries behind explicit -y confirmation ([#400](https://github.com/kantord/enwiro/pull/400))

### Fixed

- *(deps)* update strum monorepo to 0.28.0 ([#421](https://github.com/kantord/enwiro/pull/421))

## [0.2.2](https://github.com/kantord/enwiro/compare/enwiro-sdk-v0.2.1...enwiro-sdk-v0.2.2) - 2026-05-13

### Other

- move client and plugin modules into enwiro-sdk
- consolidate atomic_write helper in enwiro-sdk

## [0.2.1](https://github.com/kantord/enwiro/compare/enwiro-sdk-v0.2.0...enwiro-sdk-v0.2.1) - 2026-05-11

### Other

- move shared types to sdk create ([#353](https://github.com/kantord/enwiro/pull/353))

## [0.2.0](https://github.com/kantord/enwiro/compare/enwiro-sdk-v0.1.0...enwiro-sdk-v0.2.0) - 2026-05-10

### Added

- allow recipe gear to run linux gui apps ([#344](https://github.com/kantord/enwiro/pull/344))
