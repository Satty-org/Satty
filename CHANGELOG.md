<!-- --8<-- [start:changelog] -->

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com),
and this project adheres to [Semantic Versioning](https://semver.org).

## NEXTRELEASE

### Added

- Editing of annotations. [#607]

### Changed

- `--disable-notifications` now directs notification worthy log messages to an in-app toast [#711]
- log output now has level and severity [#711]
- satty now shows a message dialog for fatal errors [#711]
- <kbd>Esc</kbd> no longer aborts tools. [#607], [#783]
- <kbd>Esc</kbd> or <kbd>Ctrl+Enter</kbd> exits text tool instead of aborting it. [#783]

### Deprecated

- `--profile-startup` is deprecated in favour of `--log-level debug` [#711]

### Removed

### Fixed

### Security

+<!-- link targets of the [#N] references above, not rendered -->
[#607]: https://github.com/Satty-org/Satty/pull/607
[#711]: https://github.com/Satty-org/Satty/pull/711
[#783]: https://github.com/Satty-org/Satty/pull/783

<!-- --8<-- [end:changelog] -->
