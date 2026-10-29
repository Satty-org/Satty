<!-- --8<-- [start:changelog] -->

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com),
and this project adheres to [Semantic Versioning](https://semver.org).

## NEXTRELEASE

### Added

- Improved support for international keyboard layouts. Fallbackk to US
  layout if no match in current keyboard layout. Match unshifed keys also,
  i.e. new <kbb>Ctrl+equal</kbb> shortcut works on all layouts. [#771], [#772]

### Changed

- `--disable-notifications` now directs notification worthy log messages to an in-app toast [#711]
- log output now has level and severity [#711]
- satty now shows a message dialog for fatal errors [#711]

### Deprecated

- `--profile-startup` is deprecated in favour of `--log-level debug` [#711]

### Removed

### Fixed

### Security

<!-- link targets of the [#N] references above, not rendered -->

[#711]: https://github.com/Satty-org/Satty/pull/711
[#771]: https://github.com/Satty-org/Satty/pull/771
[#772]: https://github.com/Satty-org/Satty/pull/772

<!-- --8<-- [end:changelog] -->
