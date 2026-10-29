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
- Editing of annotations. [#607]

### Changed

- `--disable-notifications` now directs notification worthy log messages to an in-app toast [#711]
- log output now has level and severity [#711]
- satty now shows a message dialog for fatal errors [#711]
- <kbd>Esc</kbd> no longer aborts tools. [#607], [#783]

### Deprecated

- `--profile-startup` is deprecated in favour of `--log-level debug` [#711]

### Removed

### Fixed

- Undo after you edit a text with a double click restores the previous text [#795]
- After you edit a text with a double click, Enter selects the text with the pointer tool [#795]

### Security

+<!-- link targets of the [#N] references above, not rendered -->
[#607]: https://github.com/Satty-org/Satty/pull/607
[#711]: https://github.com/Satty-org/Satty/pull/711
[#771]: https://github.com/Satty-org/Satty/pull/771
[#772]: https://github.com/Satty-org/Satty/pull/772
[#783]: https://github.com/Satty-org/Satty/pull/783
[#795]: https://github.com/Satty-org/Satty/pull/795

<!-- --8<-- [end:changelog] -->
