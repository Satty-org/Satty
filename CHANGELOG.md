<!-- --8<-- [start:changelog] -->

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com),
and this project adheres to [Semantic Versioning](https://semver.org).

Please add link(s) to PR(s) as [#N] to the end of each entry, and add a link target at the end of the changelog of the next release.

## NEXTRELEASE

### Breaking changes

Please check these when upgrading, especially if you use your own `config.toml` or key bindings.

- The `[keybinds]` section uses a new syntax `"BINDING" = "TOOL-OR-COMMAND"`, e.g. `"<Control>l" = "line"`. The old syntax `line = "l"` still works but is deprecated and logs a warning. [#555]
- Changed default shortcuts:
  - <kbd>v</kbd> selects the pointer tool, <kbd>p</kbd> selects the new pen tool. [#799]
  - <kbd>b</kbd> selects the brush, which now paints with a soft edge and is slightly transparent. The former brush is the pen tool. [#799]
  - <kbd>h</kbd> selects the freehand highlighter, <kbd>g</kbd> the block highlighter. [#761], [#799]
  - <kbd>Delete</kbd> deletes the selected annotation, <kbd>Shift+Delete</kbd> clears all. [#607]
  - <kbd>w</kbd> focuses the size input field, <kbd>s</kbd> selects the spotlight tool. [#804]
- <kbd>Shift</kbd> while drawing a box shaped tool (crop, rectangle, ellipse, blur, highlight, ...) snaps to the aspect ratios configured in `aspect-ratios` (1:1 is one of them) instead of only drawing squares and circles. <kbd>Ctrl</kbd> keeps the current aspect ratio. [#643], [#647], [#648], [#697], [#702]
- <kbd>Esc</kbd> and <kbd>Enter</kbd> are no longer used by the tools (e.g. to abort or finish a crop) and always run the configured actions, except in the text tool. Tools finish at the end of a drag; use undo to revert. The dismiss and commit buttons were removed from the toolbar. [#607], [#783]
- In the text tool <kbd>Enter</kbd> inserts a line break, <kbd>Esc</kbd> or <kbd>Ctrl+Enter</kbd> finish the text. <kbd>Esc</kbd> no longer discards the text, use undo instead. [#823]
- Without a crop, the exported image includes annotations outside of the screenshot and has a transparent background there. Crop to the screenshot to get the old behavior. [#662]
- Freehand and block highlighter are separate tools. Holding <kbd>Ctrl</kbd> at the start of a highlight no longer switches between them, and `primary-highlighter` / `--primary-highlighter` are deprecated and ignored. The freehand highlighter is called `highlighter` in the config and `--initial-tool`. [#761], [#799]
- Styles moved to the new `[style]` section and can be set per tool. `corner-roundness`, `default-fill-shapes` and `default-round-caps` were removed from `[general]`, use `corner-radius`, `fill` and `round-caps` in `[style]` instead. The command line option `--corner-roundness` was renamed to `--corner-radius`, the old name is still accepted. [#814]
- The size in the bottom toolbar is no longer a factor but an absolute value in px, pt or percent depending on the tool. `annotation-size-factor` scales the sizes once at startup. [#814]
- Lines, arrows and other stroked shapes have flat (butt) caps instead of round ones, so short lines are drawn correctly and handles sit on the ends of the line. [#799]
- `--disable-notifications` shows notifications as an in-app toast instead of dropping them. [#711]
- `--profile-startup` is deprecated in favour of `--log-level debug`. [#711]

### Added

- Edit annotations with the pointer tool <kbd>v</kbd>: [#607], [#631], [#642], [#673], [#700], [#714], [#723], [#744], [#748], [#769], [#781], [#787], [#795], [#810]
  - Click to select, <kbd>Alt+Click</kbd> to select between overlapping annotations. Thick strokes can be grabbed anywhere on the stroke.
  - Move, resize by handles, nudge with the cursor keys, delete with <kbd>Delete</kbd>.
  - Change the style of the selected annotation with the toolbar.
  - Change the layer with <kbd>Alt+Scroll</kbd> or <kbd>Alt+PageUp</kbd>/<kbd>Alt+PageDown</kbd>.
  - Double-click a text to edit it.
  - Resizing a marker changes its number or extra ring.
  - All of this can be undone and redone.
  - Clicking an annotation with another tool temporarily switches to the pointer tool.
  - New config `auto-select-new` selects newly created annotations.
- Image tool <kbd>o</kbd>: click to insert an image from a file chooser, or drag a box to fit it in. <kbd>Ctrl+V</kbd> pastes an image from the clipboard and images can be dropped onto the window. [#563], [#766], [#773], [#817]
- Pixelate tools: `fringe-pixelate` <kbd>x</kbd> replaces the area with its surroundings before pixelating it, so the original content cannot be recovered. `pixelate` and `fringe` are available as well. [#469], [#646]
- Spotlight tool <kbd>s</kbd> dims everything but the marked area. [#804]
- Lensblur tool <kbd>Shift+U</kbd> blurs everything but the marked area. [#804]
- Blackout tool <kbd>Shift+X</kbd> blacks out the marked area. [#806]
- Pen tool <kbd>p</kbd>, which draws like the brush did before. [#799]
- Grouped tools: related tools share one toolbar button. Open the menu with a long click or a click on the indicator; it also shows the shortcuts. [#600], [#717], [#799], [#813]
- All toolbar buttons and commands can be bound to any key, including zoom, scale, size, fill and round caps. New defaults are e.g. <kbd>Ctrl+1</kbd> fit to window, <kbd>Alt+1</kbd> 100 %, <kbd>Ctrl+plus</kbd>/<kbd>Ctrl+minus</kbd> zoom, <kbd>f</kbd> toggle fill, <kbd>k</kbd> toggle round caps, <kbd>-</kbd> cycle size. Use `"none"` to unbind a key. [#555], [#606], [#618], [#624]
- Shortcuts work with all keyboard layouts: they follow the active layout, also match keys typed with <kbd>Shift</kbd>, and fall back to a Latin layout or US QWERTY for non-Latin layouts like Cyrillic or Greek. [#608], [#771], [#772]
- All styles can be configured per tool in `[style.TOOL]`, i.e. color, size, small, medium, large, fill, round caps, corner radius and the new opacity. Tools without own values use the general `[style]`. [#814]
- Text style (effect and background opacity) can be configured via fill, round caps and opacity. [#814]
- Open a screenshot from the clipboard with `--from-clipboard [COMMAND]`, default command `wl-paste -t image`. [#679], [#737]
- System wide config in `$XDG_CONFIG_DIRS/satty/config.toml` (usually `/etc/xdg/satty/config.toml`), read before the user config. [#667]
- The crop selection snaps to the image bounds. [#659], [#694], [#702]
- Round caps toggle rounded corners for rectangle, blur, pixelate and highlight. [#661]
- Pen, brush and highlighter draw a straight 15° aligned line while <kbd>Shift</kbd> is held. [#728]
- Log viewer, open it with <kbd>Ctrl+Shift+L</kbd>. [#785]
- Line, arrow, pen, brush and highlighter draw a horizontal line while <kbd>Ctrl</kbd> is held. [#801]
- The bottom toolbar shows the aspect ratio of the image or shape, and length and angle of lines and arrows. [#647], [#781]
- Selection, crop border and center marker are drawn with two colors, so they are visible on dark and light backgrounds, and keep their size when zooming. [#659]
- When zoomed in, the screenshot is drawn without smoothing, so single pixels are visible. [#652], [#750]
- Missing folders are created when saving. [#743]
- WebP images can be opened even if gdk-pixbuf has no WebP loader. [#563]
- New config `notification-grace-period` to wait for the notification daemon to read the thumbnail before exiting. [#635]
- New `--log-level` option, log output has level and severity, and a dialog is shown for fatal errors. [#711]
- Documentation website at <https://satty-org.github.io/Satty/>. [#668], [#763]

### Changed

- Unknown config fields log a warning instead of preventing Satty from starting. [#698]
- Blur is drawn below all other annotations, crop above them. [#607]
- Blur strength no longer depends on the zoom of the canvas. [#804]
- The size sets the opacity of the block highlighter, the darkness of the spotlight and the strength of blur and lensblur. [#804], [#814]
- Highlight, spotlight, blur, lensblur and marker draw a rectangle with fill or an ellipse without. [#804], [#808]
- Switching tools updates the toolbar to the style of the tool. [#814]

### Fixed

- Satty can be installed with `cargo install` again. [#625]
- Notification thumbnails no longer go missing when Satty exits right after saving. [#635]
- `input-scale` no longer has the inverse effect on the window size. [#690]
- Lines, arrows and the strokes of pen, brush and highlighter are visible right at the click, not only after the mouse moved. [#803]
- The transparency checkerboard is no longer drawn into exported images. [#754]
- `--initial-tool crop` shows the crop dimensions. [#641]
- A simple click no longer creates tiny, invisible annotations. [#660], [#720]
- The text effect toggle only reacts to <kbd>Alt</kbd> alone, so e.g. <kbd>Ctrl+Shift+Alt+U</kbd> for unicode input in fcitx5 works. [#696]
- The center marker appears and disappears right when <kbd>Alt</kbd> is pressed or released. [#734]
- Short unfilled arrows are drawn correctly and do not change direction, and toggling round caps no longer changes the arrow length. [#778]
- Custom colors are no longer duplicated in the color dialog. [#669]
- `--help` no longer refers to the nonexistent option `action_on_copy`. [#693]
- Fonts that are named instances of a variable font are loaded; variable font styles are not supported and a warning is logged. [#618]
- The canvas has the focus at start, not the size field. [#824]
- Add CSS variables for compatibility with newer GTK and Adawait. [#826]
- The text cursor and the box of an empty text are visible again while editing. [#827]

### Contributors

Thanks to everyone who contributed to this release:

- Constantine Romakhov ([@Solessfir](https://github.com/Solessfir))
- Haïm Dimer ([@hdimer](https://github.com/hdimer))
- ilyusha ([@rokokol](https://github.com/rokokol))
- Jonathan ([@Jab2870](https://github.com/Jab2870))
- René D. Obermüller ([@RobertMueller2](https://github.com/RobertMueller2))
- [@robertwidfen](https://github.com/robertwidfen)
- Sitiyou ([@sitiyou](https://github.com/sitiyou))

<!-- link targets of the [#N] references above, not rendered -->
[#469]: https://github.com/Satty-org/Satty/pull/469
[#555]: https://github.com/Satty-org/Satty/pull/555
[#563]: https://github.com/Satty-org/Satty/pull/563
[#600]: https://github.com/Satty-org/Satty/pull/600
[#606]: https://github.com/Satty-org/Satty/pull/606
[#607]: https://github.com/Satty-org/Satty/pull/607
[#608]: https://github.com/Satty-org/Satty/pull/608
[#618]: https://github.com/Satty-org/Satty/pull/618
[#624]: https://github.com/Satty-org/Satty/pull/624
[#625]: https://github.com/Satty-org/Satty/pull/625
[#631]: https://github.com/Satty-org/Satty/pull/631
[#635]: https://github.com/Satty-org/Satty/pull/635
[#641]: https://github.com/Satty-org/Satty/pull/641
[#642]: https://github.com/Satty-org/Satty/pull/642
[#643]: https://github.com/Satty-org/Satty/pull/643
[#646]: https://github.com/Satty-org/Satty/pull/646
[#647]: https://github.com/Satty-org/Satty/pull/647
[#648]: https://github.com/Satty-org/Satty/pull/648
[#652]: https://github.com/Satty-org/Satty/pull/652
[#659]: https://github.com/Satty-org/Satty/pull/659
[#660]: https://github.com/Satty-org/Satty/pull/660
[#661]: https://github.com/Satty-org/Satty/pull/661
[#662]: https://github.com/Satty-org/Satty/pull/662
[#667]: https://github.com/Satty-org/Satty/pull/667
[#668]: https://github.com/Satty-org/Satty/pull/668
[#669]: https://github.com/Satty-org/Satty/pull/669
[#673]: https://github.com/Satty-org/Satty/pull/673
[#679]: https://github.com/Satty-org/Satty/pull/679
[#690]: https://github.com/Satty-org/Satty/pull/690
[#693]: https://github.com/Satty-org/Satty/pull/693
[#694]: https://github.com/Satty-org/Satty/pull/694
[#696]: https://github.com/Satty-org/Satty/pull/696
[#697]: https://github.com/Satty-org/Satty/pull/697
[#698]: https://github.com/Satty-org/Satty/pull/698
[#700]: https://github.com/Satty-org/Satty/pull/700
[#702]: https://github.com/Satty-org/Satty/pull/702
[#711]: https://github.com/Satty-org/Satty/pull/711
[#714]: https://github.com/Satty-org/Satty/pull/714
[#717]: https://github.com/Satty-org/Satty/pull/717
[#720]: https://github.com/Satty-org/Satty/pull/720
[#723]: https://github.com/Satty-org/Satty/pull/723
[#728]: https://github.com/Satty-org/Satty/pull/728
[#734]: https://github.com/Satty-org/Satty/pull/734
[#737]: https://github.com/Satty-org/Satty/pull/737
[#743]: https://github.com/Satty-org/Satty/pull/743
[#744]: https://github.com/Satty-org/Satty/pull/744
[#748]: https://github.com/Satty-org/Satty/pull/748
[#750]: https://github.com/Satty-org/Satty/pull/750
[#754]: https://github.com/Satty-org/Satty/pull/754
[#761]: https://github.com/Satty-org/Satty/pull/761
[#763]: https://github.com/Satty-org/Satty/pull/763
[#766]: https://github.com/Satty-org/Satty/pull/766
[#769]: https://github.com/Satty-org/Satty/pull/769
[#771]: https://github.com/Satty-org/Satty/pull/771
[#772]: https://github.com/Satty-org/Satty/pull/772
[#773]: https://github.com/Satty-org/Satty/pull/773
[#778]: https://github.com/Satty-org/Satty/pull/778
[#781]: https://github.com/Satty-org/Satty/pull/781
[#783]: https://github.com/Satty-org/Satty/pull/783
[#785]: https://github.com/Satty-org/Satty/pull/785
[#787]: https://github.com/Satty-org/Satty/pull/787
[#795]: https://github.com/Satty-org/Satty/pull/795
[#799]: https://github.com/Satty-org/Satty/pull/799
[#801]: https://github.com/Satty-org/Satty/pull/801
[#803]: https://github.com/Satty-org/Satty/pull/803
[#804]: https://github.com/Satty-org/Satty/pull/804
[#806]: https://github.com/Satty-org/Satty/pull/806
[#808]: https://github.com/Satty-org/Satty/pull/808
[#810]: https://github.com/Satty-org/Satty/pull/810
[#813]: https://github.com/Satty-org/Satty/pull/813
[#814]: https://github.com/Satty-org/Satty/pull/814
[#817]: https://github.com/Satty-org/Satty/pull/817
[#823]: https://github.com/Satty-org/Satty/pull/823
[#824]: https://github.com/Satty-org/Satty/pull/824
[#826]: https://github.com/Satty-org/Satty/pull/826
[#827]: https://github.com/Satty-org/Satty/pull/827

<!-- --8<-- [end:changelog] -->
