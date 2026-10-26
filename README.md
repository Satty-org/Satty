# <img src="assets/satty.svg" height="42"> Satty: Modern Screenshot Annotation.

<!-- --8<-- [start:intro] -->
Satty is a screenshot annotation tool inspired by [Swappy](https://github.com/jtheoof/swappy) and [Flameshot](https://flameshot.org/).

![](assets/usage.gif)

Satty has been created to provide the following improvements over existing screenshot annotation tools:

- very simple and easy to understand toolset (like Swappy)
- fullscreen annotation mode and post shot cropping (like Flameshot)
- extremely smooth rendering thanks to HW acceleration (OpenGL)
- working on wlroots based compositors (Sway, Hyprland, River, ...)
- minimal, modern looking UI, thanks to GTK and Adwaita
- be a playground for new features (post window selection, post paint editing, ...)
<!-- --8<-- [end:intro] -->

See [CHANGELOG.md](CHANGELOG.md) for the changes in each release.

## Install
<!-- --8<-- [start:install] -->

Thanks to our package maintainers, Satty is available for many distributions on Linux and BSD:

[![Packaging status](https://repology.org/badge/vertical-allrepos/satty.svg)](https://repology.org/project/satty/versions)

For emoji support please install the "Noto Color Emoji" font.

### Specifics

| Distribution | Command | Note |
| --- | --- | --- |
| Gentoo | `emerge -av satty` | You need guru overlay (see [wiki](https://github.com/Satty-org/Satty/wiki/Gentoo-Guru)). Pending [PR](https://github.com/gentoo/gentoo/pull/33908) |
| Alpine Linux | `apk add satty` | Available in [Alpine Community](https://pkgs.alpinelinux.org/packages?name=satty&branch=edge&repo=&arch=&maintainer=) |

### Cargo

You can install satty via `cargo install satty` and `cargo binstall satty`, the latter requires [cargo-binstall](https://crates.io/crates/cargo-binstall). In that case, no manpage, LICENSE file, .Desktop file are available. But you can use the `--man` and `--license` arguments to display either.

`cargo install` and `cargo binstall` methods install satty to `$HOME/.cargo/bin`. Using rustup and/or installing distribution packages for the Rust toolchain should normally add this to the user's path. How environment variables such as path are set depends on shell, distribution, compositor/desktop environment etc. It's not really possible to anticipate every possible combination from an application perspective.

### Prebuilt Sources

You can download a prebuilt binary for x86-64 on the [Satty Releases](https://github.com/Satty-org/Satty/releases) page.

### Flatpak <sup>0.20.1</sup>

Satty is available as a Flatpak bundle. Pre-built bundles are automatically created for each release and can be downloaded from the [GitHub Releases](https://github.com/Satty-org/Satty/releases) page.

#### Installing from Flatpak bundle

```sh
# Download the .flatpak file from the latest release
# Then install it:
flatpak install satty-<version>.flatpak
```
<!-- --8<-- [end:install] -->

## Usage

See [USAGE.md](USAGE.md) for all command line options.

<!-- --8<-- [start:usage] -->

Start by providing a filename or a screenshot via stdin and annotate using the available tools. Save to clipboard or file when finished. Tools and Interface have been kept simple.

```shell
# open a file
satty -f assets/usage.png
# get screenshot with slurp and grim
grim -g "$(slurp -o)" - | satty -f -
# get screenshot with DMS
dms screenshot --stdout | satty -f -
```

<!-- --8<-- [end:usage] -->

## Configuration

See [config.toml](config.toml) for the configuration.

<!-- --8<-- [start:config-search] -->
The search order for configuration files is:

- builtin
- system wide config (`$XDG_CONFIG_DIRS/satty/config.toml`, usually `/etc/xdg/satty/config.toml`) if present <sup>NEXTRELEASE</sup>
- user config (`$XDG_CONFIG_HOME/satty/config.toml`, usually `$HOME/.config/satty/config.toml`) if present
- config specified in command line, if any. Please note: in this case Satty ignores the former two files, and a file not found error on the given config file becomes fatal
- command line options

<!-- --8<-- [end:config-search] -->

### Shortcuts
<!-- --8<-- [start:shortcuts] -->

#### General

- <kbd>Enter</kbd>: as configured (see below), default: copy-to-clipboard (may be masked by active tool)
- <kbd>Esc</kbd>: as configured (see below), default: exit (may be masked by active tool)
- <kbd>Shift+Delete</kbd> reset (clear) <sup>experimental</sup> <sup>NEXTRELEASE</sup>
- <kbd>Ctrl+C</kbd>: Save to clipboard (may be masked by active tool)
- <kbd>Ctrl+Shift+D</kbd> or <kbd>Ctrl+Shift+I</kbd>: Open GTK inspector if not already opened
- <kbd>Ctrl+S</kbd>: Save to specified output file, creating missing folders in its path <sup>NEXTRELEASE</sup>
- <kbd>Ctrl+Shift+S</kbd>: Save using file dialog <sup>0.20.0</sup>. The dialog uses `output-filename` as initial filename/path when available and remembers the last selected folder. <sup>0.21.0</sup>
- <kbd>Ctrl+Alt+C</kbd>: Copy last saved filepath to clipboard <sup>0.20.1</sup>
- <kbd>Ctrl+T</kbd>: Toggle toolbars
- <kbd>Ctrl+Y</kbd>: Redo
- <kbd>Ctrl+Z</kbd>: Undo
- <kbd>Alt</kbd>+(<kbd>Left</kbd>/<kbd>Right</kbd>/<kbd>Up</kbd>/<kbd>Down</kbd>): Pan, also available with middle mouse button drag <sup>0.20.1</sup>

Mouse:

- <kbd>Ctrl+Wheel</kbd>: Zoom in/out <sup>0.22.0</sup>
- <kbd>Middle</kbd>: Pan

Touchpad: <sup>0.22.0</sup>

- <kbd>Pinch</kbd>: Zoom (two finger)
- <kbd>Ctrl+Scroll</kbd>: Zoom in/out  (two finger scroll)
- <kbd>Scroll</kbd>: Pan  (two finger scroll)

#### Color Selection Shortcuts <sup>0.20.1</sup>

<kbd>1</kbd>, <kbd>2</kbd>, <kbd>3</kbd>, …, <kbd>9</kbd>, <kbd>0</kbd> — select nth color from the color palette

If out of range select custom color <sup>0.21.0</sup>

#### Size Selection <sup>0.22.0</sup>

In the bottom toolbar the size can be edited directly or by + and - buttons.
Minimum is 0.1 and maximum to 99.99.

The bindings are:

- Mouse <kbd>left-button</kbd>, <kbd>wheel</kbd> and key <kbd>up</kbd>/<kbd>down</kbd> step size is 0.1
- Holding <kbd>Shift</kbd> will switch to 0.01 step size
- Holding <kbd>Ctrl</kbd> will switch to 1.0 step size
- Mouse <kbd>middle-button</kbd> and <kbd>page up</kbd>/<kbd>page down</kbd> step size is 1.0
- Mouse <kbd>right-button</kbd> jumps to minimum/maximum
- <kbd>s</kbd> focuses the annotation size factor input field

#### Tool Selection Shortcuts (configurable) <sup>0.20.0</sup>

Default single-key shortcuts:

- <kbd>p</kbd>: Pointer tool
- <kbd>c</kbd>: Crop tool
- <kbd>b</kbd>: Brush tool
- <kbd>i</kbd>: Line tool
- <kbd>z</kbd>: Arrow tool
- <kbd>r</kbd>: Rectangle tool
- <kbd>e</kbd>: Ellipse tool
- <kbd>t</kbd>: Text tool
- <kbd>m</kbd>: Numbered Marker tool
- <kbd>u</kbd>: Blur tool
- <kbd>x</kbd>: Fringe inpaint+Pixelate tool
- <kbd>g</kbd>: Block Highlight tool
- <kbd>G</kbd>: Freehand Highlight tool
- <kbd>o</kbd>: Image tool <sup>NEXTRELEASE</sup>

#### Overwriting Keybindings <sup>NEXTRELEASE</sup>

Shortcuts can be overwritten in the config by 
```toml
[keybinds]
"BINDING" = "TOOL-OR-COMMAND"
```

Where `BINDING` follows the GTK syntax. This means modifiers are enclosed in angle brackets (e.g., `<mod>`) and keys are specified by name (for example, `-` must be written as `minus`). 

Pressing any unbound key will print its name to the console.

Setting a binding to `"none"` will unbind it.

#### Keyboard Layouts <sup>NEXTRELEASE</sup>

A binding matches the key that the active layout produces. In QWERTZ or Dvorak, it follows the letters on the keys, not their positions.

When the active layout produces a letter of a non-Latin script, for example Cyrillic or Greek, a binding also matches the same key in the first configured Latin layout. If no Latin layout is configured, it matches the key at the same position in the US QWERTY layout. A binding for the non-Latin key itself has priority. This also applies to <kbd>Ctrl+A</kbd>, <kbd>Ctrl+C</kbd>, <kbd>Ctrl+V</kbd> and <kbd>Ctrl+X</kbd> in the text tool.

<!-- --8<-- [end:shortcuts] -->

The defaults are listed in the [config.toml](config.toml).

<!-- --8<-- [start:tools] -->
### Pointer Tool <sup>NEXTRELEASE</sup>

Annotations can be selected by click.
This will update the toolbar to the annotation's style and allow style changes.
Newly created annotations can be autoselected if enabled in the config.

- Hold <kbd>Alt</kbd> while clicking to select between overlapping annotations.
- <kbd>Delete</kbd> deletes the selected annotation.
- Drag the resize handles - mouse cursor will change - to change the size. 
  - Hold <kbd>Alt</kbd> for centered resize
  - Hold <kbd>Shift</kbd> to snap to defined aspect ratios. <sup>NEXTRELEASE</sup>
  - Hold <kbd>Control</kbd> at corners for aspect ratio preserving resize. <sup>NEXTRELEASE</sup>
  - Hold mutiple to combine them.
- Grab an annotation - mouse cursor will change to a hand - to move.
- <kbd>Alt</kbd>+scroll up/down or <kbd>Alt</kbd>+<kbd>PageUp</kbd>/<kbd>PageDown</kbd> to change the annotation's layer.
- Nudge (small move) with the cursor keys.
- Double-click on a text annotation to edit it.
- Horizontal resizing on a marker changes its number, and vertical resizing changes the extra ring.

### Image Tool <sup>NEXTRELEASE</sup>

Click the screenshot with the tool to open a file chooser, and the picked image is inserted as an annotation centred where you clicked, scaled down to at most half the screenshot if it does not fit. Drag a box instead of clicking, and the image is scaled to fit into it with its aspect ratio kept. From there it behaves like any other annotation: select it with the pointer tool to move it, resize it by its handles or change its layer.

Images can also arrive without selecting the tool first:

- <kbd>Ctrl+V</kbd> pastes an image or a copied image file from the clipboard, centred on the mouse pointer. The text tool takes the shortcut for itself while it has input focus.
- Dropping an image or an image file anywhere on the window inserts it where it was dropped.

Formats are those gdk-pixbuf has a loader for, plus webp.

### Tool Modifiers and Keys

Arrow and line:

- <kbd>Shift</kbd> to make tool snap to 15° steps.

Crop <sup>NEXTRELEASE</sup>, rectangle, ellipse, blur <sup>0.22.0</sup>, highlight block mode<sup>0.22.0</sup>, pixelate<sup>NEXTRELASE</sup>, fringe-pixelate<sup>NEXTRELEASE</sup>, fringe<sup>NEXTRELEASE</sup>, image<sup>NEXTRELEASE</sup>:

- <kbd>Alt</kbd> to center the tool around origin.
- <kbd>Shift</kbd> to snap to defined aspect ratios. <sup>NEXTRELEASE</sup>
- Hold <kbd>Control</kbd> to preserve aspect ratio. Draws square if starting from zero size. <sup>NEXTRELEASE</sup>
- Hold mutiple to combine them.

Text:

- Press <kbd>Shift+Enter</kbd> to insert line break.
- Combine <kbd>Ctrl</kbd> with <kbd>Left</kbd> or <kbd>Right</kbd> for word jump or <kbd>Ctrl</kbd> with <kbd>Backspace</kbd> or <kbd>Delete</kbd> for word delete.
- Press <kbd>Enter</kbd> or switch to another tool to accept input, press <kbd>Escape</kbd> to discard entered text.
- <kbd>Home</kbd> and <kbd>End</kbd> go to the start/end of current line or previous/next line if already on first/last character of line (automatic wrapping is not considered for this). <kbd>Ctrl</kbd> with <kbd>Home</kbd>/<kbd>End</kbd> jumps to start/end of text buffer.
- <kbd>Up</kbd> or <kbd>Down</kbd> to jump to previous/next line (if already on first/last line, it jumps to the start/end of text buffer). <sup>0.20.1</sup>
- Combine <kbd>Shift</kbd> with other keys to select text (e.g. `Shift+Home` to select from start of line to cursor,  <kbd>Shift+Left</kbd> to select characters before cursor,  <kbd>Ctrl+Shift+Left</kbd> to select words before cursor,and so on) <sup>0.20.1</sup>
- <kbd>Double-click </kbd> to select word under cursor.Triple-click to select all text. <sup>0.20.1</sup>
- <kbd>Ctrl+A</kbd> to select all text. <sup>0.20.1</sup>
- <kbd>Ctrl+C</kbd> to copy selected text to clipboard. <sup>0.20.1</sup>
- <kbd>Ctrl+X</kbd> to cut selected text to clipboard. <sup>0.20.1</sup>
- <kbd>Ctrl+V</kbd> to paste text from clipboard. <sup>0.20.1</sup>
- <kbd>Alt+Ctrl</kbd> with <kbd>Left</kbd> or <kbd>Right</kbd> or <kbd>Up</kbd> or <kbd>Down</kbd> to move the text. Use <kbd>Alt+Ctrl+Shift</kbd> with arrow keys to nudge the text. <sup>0.20.1</sup>
- Press <kbd>Alt</kbd> to cycle the text effect: none → inverted outline → contrast outline (black/white) → drop shadow. <sup>experimental</sup> <sup>0.22.0</sup>

Marker:

- Hold <kbd>Alt</kbd> to get extra ring. <sup>0.22.0</sup>

Brush:

- Hold <kbd>Shift</kbd> for a straight 15° aligned line. Release <kbd>Shift</kbd> to continue freehand, or hold it again to start another straight segment. <sup>NEXTRELEASE</sup>

Highlight:

- Hold <kbd>Ctrl</kbd> to switch between block and freehand mode (default configurable, see below). Removed in favour of distinct tool variants <sup>NEXTRELEASE</sup>
- Hold <kbd>Shift</kbd> in freehand mode for a straight 15° aligned line. Stop at some position and release and hold <kbd>Shift</kbd> again to achieve perfectly aligned turns.

Additional note on blur, fringe inpaint, pixelate and fringe inpaint-pixelate <sup>NEXTRELEASE</sup>:

- **Maximum Privacy:** For the paranoid among us, we recommend using a solid, filled rectangle instead.
- **Optimized Pixelation:** For best results when censoring text, ensure your block size matches the line height to guarantee unreadable characters.
<!-- --8<-- [end:tools] -->

### CSS
<!-- --8<-- [start:css] -->

Satty ships with [minimal builtin CSS](https://github.com/Satty-org/Satty/tree/main/src/assets/default.css) which can be overridden by `$XDG_CONFIG_HOME/satty/overrides.css`. Adwaita defaults for headerbar (`@headerbar_fg_color` and `@headerbar_bg_color`) which Satty uses <sup>0.21.0</sup> may lack transparency, here's an override example:

```css
.outer_box,
.toolbar {
    color: #000000;
    background-color: #ddddddaa;
}
```

You can discover styleable elements by using the GTK inspector with env variable `GTK_DEBUG=interactive`. Also, see [wiki](https://github.com/Satty-org/Satty/wiki/Config-examples#css-overridescss) for more examples.
<!-- --8<-- [end:css] -->

### IME
<!-- --8<-- [start:ime] -->

Satty supports IME <sup>0.20.0</sup> via GTK with and without preediting. Please note, at this point Satty has no proper fallback font handling so the font used needs to contain the entered glyphs.
<!-- --8<-- [end:ime] -->

<!-- --8<-- [start:integration] -->
### wlroots based compositors (Sway, Wayfire, River, ...)

You can bind a key to the following command:

```sh
grim -g "$(slurp -o -r -c '#ff0000ff')" -t ppm - | satty --filename - --fullscreen --output-filename ~/Pictures/Screenshots/satty-%Y%m%d-%H:%M:%S.png
```

#### Hyprland

The following Hyprland binding will use slurp and grim to make the screenshot, copy it to the clipboard by `wl-copy` and start Satty with a predefined output path:

```lua
hl.bind(
    "Print",
    hl.dsp.exec_cmd(
        'sh -c "grim -g \\"$(slurp -o -c \'#ff0000ff\')\\" -t ppm - | tee >(wl-copy --type image/png) | satty -f - --output-filename ~/Pictures/screenshot-%Y-%m-%d_%H-%M-%S.png"'
    ),
    { description = "Satty Screenshot" }
)
```

To make Satty a floating window in Hyprland, add a window rule:

```lua
hl.window_rule({
    match = { class = "com.gabm.satty" },
    no_anim = true,
    float = true,
    center = true,
    --min_size = { 200, 200 },
    --min_size = { 600, 600 },
})
```

Please note we're using ppm in both examples. Compared to png, ppm is uncompressed and this can save time.

### Other examples

#### Image Resize

Satty does not provide a resize mechanism other than cropping. But you can pipe the result to other tools such as ImageMagick:

```sh
grim -g "0,0 3840x2160" -t ppm - | satty --filename - --output-filename - | convert -resize 50% - out.png
```

#### Sway mode

Add this to your ~/.config/sway/config.
It needs `grim` and `slurp`.
```sh
# screenshots
# inspiration: https://www.reddit.com/r/swaywm/comments/ghnlea/comment/fqnzxkx/?utm_source=share&utm_medium=web3x&utm_name=web3xcss&utm_term=1&utm_content=share_button
set $satty satty -f - --initial-tool=arrow --copy-command=wl-copy --actions-on-escape="save-to-clipboard,exit" --brush-smooth-history-size=5 --disable-notifications
set $printscreen_mode 'printscreen (r:region, f:full, w:window)'
mode $printscreen_mode {
    bindsym r exec swaymsg 'mode "default"' && grim -t ppm -g "$(slurp -d)" - | $satty
    bindsym f exec swaymsg 'mode "default"' && grim -t ppm - | $satty
    bindsym w exec swaymsg 'mode "default"' && swaymsg -t get_tree | jq -r '.. | select(.focused?) | .rect | "\(.x),\(.y) \(.width)x\(.height)"' | grim -t ppm -g - - | $satty

    bindsym Return mode "default"
    bindsym Escape mode "default"
}
bindsym $mod+Shift+p mode $printscreen_mode
```
<!-- --8<-- [end:integration] -->

<!-- --8<-- [start:build] -->
## Build from source

You first need to install the native dependencies of Satty (see below) and then run:

```sh
# build release binary, located in ./target/release/satty
make build-release

# optional: install to /usr/local
PREFIX=/usr/local make install

# optional: uninstall from /usr/local
PREFIX=/usr/local make uninstall
```

## Dependencies

Satty is based on GTK-4 and Adwaita.
Dependencies, depending of each distributions are:

- glib2
- gtk4 (libgtk-4-x)
- gdk-pixbuf2
- libadwaita
- libepoxy
- fontconfig
<!-- --8<-- [end:build] -->

## Credits
<!-- --8<-- [start:credits] -->
### Maintainers and Contributors

Satty wouldn't exist without the help of our contributors and maintainers. Current maintainers are: @RobertMueller2, @fabienjuif, @robertwidfen and @gabm (original author). Our contributors are:

<a href="https://github.com/Satty-org/Satty/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=Satty-org/Satty" />
</a>

Made with [contrib.rocks](https://contrib.rocks).

### Heavy Lifting in some areas

- [femtovg](https://crates.io/crates/femtovg/)
- [Relm4](https://relm4.org/)
- [GTK](https://gtk.org/)

### Inspiration

- [Swappy](https://github.com/jtheoof/swappy/)
- [Flameshot](https://flameshot.org/)
  - Fringe-Pixelate was inspired by https://github.com/flameshot-org/flameshot/pull/3765/changes

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=Satty-org/Satty&type=date&legend=top-left)](https://www.star-history.com/#Satty-org/Satty&type=date&legend=top-left)
<!-- --8<-- [end:credits] -->

## License
<!-- --8<-- [start:license] -->

The source code is released under the MPL-2.0 license.

The Font 'Roboto Regular' from Google is released under Apache-2.0 license.
<!-- --8<-- [end:license] -->
