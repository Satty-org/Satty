<!-- --8<-- [start:changelog] -->

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com),
and this project adheres to [Semantic Versioning](https://semver.org).

Please add link(s) to PR(s) as [#N] to the end of each entry, and add a link target at the end of the changelog of the next release.

## NEXTRELEASE

### Added

- Improved support for international keyboard layouts. Fallback to US layout if no match in current keyboard layout. Match unshifted keys also, i.e. new <kbd>Ctrl+equal</kbd> shortcut works on all layouts. [#771], [#772]
- Editing of annotations. [#607]
- Brush <kbd>b</kbd> now paints with a soft edge and slightly transparent, the former brush is the new pen tool. [799]
- Line, arrow, pen, brush and highlighter draw a horizontal line while <kbd>Ctrl</kbd> is held. [#801]

### Changed

- `--disable-notifications` now directs notification worthy log messages to an in-app toast [#711]
- log output now has level and severity [#711]
- satty now shows a message dialog for fatal errors [#711]
- <kbd>Esc</kbd> no longer aborts tools. [#607], [#783]
- Pen tool <kbd>p</kbd>, it draws like the brush did before. [#799]
- Pen, freehand highlighter and brush are grouped under one toolbar button, the freehand highlighter is no longer grouped with the block highlighter. [#799]
- Pointer tool is on <kbd>v</kbd> instead of <kbd>p</kbd>, which selects the pen now. [#799]
- Freehand highlighter is on <kbd>h</kbd> instead of <kbd>Shift+G</kbd>. [#799]
- Freehand highlighter tool name for the config and `--initial-tool` is `highlighter` instead of `highlight-freehand`. [#799]
- Caps style for lines (several tools) is butt now, instead of round. This avoids drawing issues with lines shorter than linewidth and let's the anchors sit on the edges of the line. [#799]

### Deprecated

- `--profile-startup` is deprecated in favour of `--log-level debug` [#711]
- Config `primary-highlighter` and option `--primary-highlighter` are deprecated and ignored, block *highlight* and freehand *highlighter* are separate tools now. [#799]

### Removed

- Holding <kbd>Ctrl</kbd> at the start of a highlight no longer switches between block and freehand highlighter. [#799]

### Fixed

- Undo after you edit a text with a double click restores the previous text [#795]
- After you edit a text with a double click, Enter selects the text with the pointer tool [#795]

### Security

<!-- link targets of the [#N] references above, not rendered -->

[#607]: https://github.com/Satty-org/Satty/pull/607
[#711]: https://github.com/Satty-org/Satty/pull/711
[#771]: https://github.com/Satty-org/Satty/pull/771
[#772]: https://github.com/Satty-org/Satty/pull/772
[#783]: https://github.com/Satty-org/Satty/pull/783
[#795]: https://github.com/Satty-org/Satty/pull/795
[#799]: https://github.com/Satty-org/Satty/pull/799
[#801]: https://github.com/Satty-org/Satty/pull/801

<!-- --8<-- [end:changelog] -->
