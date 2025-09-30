# <img src="assets/satty.svg" height="42"> Satty: Modern Screenshot Annotation.

Satty is a screenshot annotation tool inspired by [Swappy](https://github.com/jtheoof/swappy) and [Flameshot](https://flameshot.org/).

![](assets/usage.gif)

Satty has been created to provide the following improvements over existing screenshot annotation tools:

- very simple and easy to understand toolset (like Swappy)
- fullscreen annotation mode and post shot cropping (like Flameshot)
- extremely smooth rendering thanks to HW acceleration (OpenGL)
- working on wlroots based compositors (Sway, Hyprland, River, ...)
- minimal, modern looking UI, thanks to GTK and Adwaita
- be a playground for new features (post window selection, post paint editing, ...)

## Install

Thanks to our package maintainers, Satty is available for many distributions on Linux and BSD:

[![Packaging status](https://repology.org/badge/vertical-allrepos/satty.svg)](https://repology.org/project/satty/versions)

### Specifics
| Distribution | Command | Note |
| --- | --- | --- |
| Gentoo | `emerge -av satty` | You need guru overlay (see [wiki](https://github.com/gabm/Satty/wiki/Gentoo-Guru)). Pending [PR](https://github.com/gentoo/gentoo/pull/33908) |
| Alpine Linux | `apk add satty` | Available in [Alpine Community](https://pkgs.alpinelinux.org/packages?name=satty&branch=edge&repo=&arch=&maintainer=) |

### Prebuilt Sources

You can download a prebuilt binary for x86-64 on the [Satty Releases](https://github.com/gabm/satty/releases) page.

## Usage

Start by providing a filename or a screenshot via stdin and annotate using the available tools. Save to clipboard or file when finished. Tools and Interface have been kept simple.

All configuration is done either at the config file in `XDG_CONFIG_DIR/.config/satty/config.toml` or via the command line interface. In case both are specified, the command line options always override the configuration file.

### Shortcuts

- `Enter`: as configured (see below), default: copy-to-clipboard
- `Esc`: as configured (see below), default: exit
- `Ctrl+C`: Save to clipboard
- `Ctrl+S`: Save to specified output file
- `Ctrl+T`: Toggle toolbars
- `Ctrl+Y`: Redo
- `Ctrl+Z`: Undo

### Tool Modifiers and Keys

- Arrow: Hold `Shift` to make arrow snap to 15° steps
- Ellipse: Hold `Alt` to center the ellipse around origin, hold `Shift` for a circle
- Highlight: Hold `Ctrl` to switch between block and freehand mode (default configurable, see below), hold Shift for a square (if the default mode is block) or a straight line (if the default mode is freehand)
- Line: Hold `Shift` to make line snap to 15° steps
- Rectangle: Hold `Alt` to center the rectangle around origin, hold `Shift` for a square
- Text: Press `Shift+Enter` to insert line break, combine `Ctrl` with `Left` or `Right` for word jump or `Ctrl` with `Backspace` or `Delete` for word delete. Press `Enter` or switch to another tool to accept input, press `Escape` to discard entered text.

### Configuration File

```toml
[general]
# Start Satty in fullscreen mode
fullscreen = true
# Exit directly after copy/save action
early-exit = true
# Draw corners of rectangles round if the value is greater than 0 (0 disables rounded corners)
corner-roundness = 12
# Select the tool on startup [possible values: pointer, crop, line, arrow, rectangle, text, marker, blur, brush]
initial-tool = "brush"
# Configure the command to be called on copy, for example `wl-copy`
copy-command = "wl-copy"
# Increase or decrease the size of the annotations
annotation-size-factor = 2
# Filename to use for saving action. Omit to disable saving to file. Might contain format specifiers: https://docs.rs/chrono/latest/chrono/format/strftime/index.html
output-filename = "/tmp/test-%Y-%m-%d_%H:%M:%S.png"
# After copying the screenshot, save it to a file as well
save-after-copy = false
# Hide toolbars by default
default-hide-toolbars = false
# Experimental: whether window focus shows/hides toolbars. This does not affect initial state of toolbars, see default-hide-toolbars.
focus-toggles-toolbars = false
# Fill shapes by default
default-fill-shapes = false
# The primary highlighter to use, the other is accessible by holding CTRL at the start of a highlight [possible values: block, freehand]
primary-highlighter = "block"
# Disable notifications
disable-notifications = false
# Actions to trigger on right click (order is important)
# [possible values: save-to-clipboard, save-to-file, exit]
actions-on-right-click = []
# Actions to trigger on Enter key (order is important)
# [possible values: save-to-clipboard, save-to-file, exit]
actions-on-enter = ["save-to-clipboard"]
# Actions to trigger on Escape key (order is important)
# [possible values: save-to-clipboard, save-to-file, exit]
actions-on-escape = ["exit"]
# Action to perform when the Enter key is pressed [possible values: save-to-clipboard, save-to-file]
# Deprecated: use actions-on-enter instead
action-on-enter = "save-to-clipboard"
# Right click to copy
# Deprecated: use actions-on-right-click instead
right-click-copy = false
# request no window decoration. Please note that the compositor has the final say in this. At this point. requires xdg-decoration-unstable-v1.
no-window-decoration = true
# experimental feature: adjust history size for brush input smooting (0: disabled, default: 0, try e.g. 5 or 10)
brush-smooth-history-size = 10

# Font to use for text annotations
[font]
family = "Roboto"
style = "Bold"

# Custom colours for the colour palette
[color-palette]
# These will be shown in the toolbar for quick selection
palette = [
    "#00ffff",
    "#a52a2a",
    "#dc143c",
    "#ff1493",
    "#ffd700",
    "#008000",
]

# These will be available in the color picker as presets
# Leave empty to use GTK's default
custom = [
    "#00ffff",
    "#a52a2a",
    "#dc143c",
    "#ff1493",
    "#ffd700",
    "#008000",
]
```

### Command Line

```sh
» satty --help
Modern Screenshot Annotation.

Usage: satty [OPTIONS] --filename <FILENAME>

Options:
  -c, --config <CONFIG>
          Path to the config file. Otherwise will be read from XDG_CONFIG_DIR/satty/config.toml
  -f, --filename <FILENAME>
          Path to input image or '-' to read from stdin
      --fullscreen
          Start Satty in fullscreen mode
  -o, --output-filename <OUTPUT_FILENAME>
          Filename to use for saving action or '-' to print to stdout. Omit to disable saving to file. Might contain format specifiers: <https://docs.rs/chrono/latest/chrono/format/strftime/index.html>
      --early-exit
          Exit directly after copy/save action
      --corner-roundness <CORNER_ROUNDNESS>
          Draw corners of rectangles round if the value is greater than 0 (Defaults to 12) (0 disables rounded corners)
      --initial-tool <TOOL>
          Select the tool on startup [aliases: --init-tool] [possible values: pointer, crop, line, arrow, rectangle, ellipse, text, marker, blur, highlight, brush]
      --copy-command <COPY_COMMAND>
          Configure the command to be called on copy, for example `wl-copy`
      --annotation-size-factor <ANNOTATION_SIZE_FACTOR>
          Increase or decrease the size of the annotations
      --save-after-copy
          After copying the screenshot, save it to a file as well Preferably use the `action_on_copy` option instead
      --actions-on-enter <ACTIONS_ON_ENTER>
          Actions to perform when pressing Enter [possible values: save-to-clipboard, save-to-file, exit]
      --actions-on-escape <ACTIONS_ON_ESCAPE>
          Actions to perform when pressing Escape [possible values: save-to-clipboard, save-to-file, exit]
      --actions-on-right-click <ACTIONS_ON_RIGHT_CLICK>
          Actions to perform when hitting the copy Button [possible values: save-to-clipboard, save-to-file, exit]
  -d, --default-hide-toolbars
          Hide toolbars by default
      --focus-toggles-toolbars
          Experimental: Whether to toggle toolbars based on focus. Doesn't affect initial state
      --default-fill-shapes
          Experimental feature: Fill shapes by default
      --font-family <FONT_FAMILY>
          Font family to use for text annotations
      --font-style <FONT_STYLE>
          Font style to use for text annotations
      --primary-highlighter <PRIMARY_HIGHLIGHTER>
          The primary highlighter to use, secondary is accessible with CTRL [possible values: block, freehand]
      --disable-notifications
          Disable notifications
      --profile-startup
          Print profiling
      --no-window-decoration
          Disable the window decoration (title bar, borders, etc.) Please note that the compositor has the final say in this. Requires xdg-decoration-unstable-v1
      --brush-smooth-history-size <BRUSH_SMOOTH_HISTORY_SIZE>
          Experimental feature: How many points to use for the brush smoothing algorithm. 0 disables smoothing. The default value is 0 (disabled)
      --right-click-copy
          Right click to copy. Preferably use the `action_on_right_click` option instead
      --action-on-enter <ACTION_ON_ENTER>
          Action to perform when pressing Enter. Preferably use the `actions_on_enter` option instead [possible values: save-to-clipboard, save-to-file, exit]
  -h, --help
          Print help
  -V, --version
          Print version
```

### wlroots based compositors (Sway, Wayfire, River, ...)

You can bind a key to the following command:

```
grim -g "$(slurp -o -r -c '#ff0000ff')" -t ppm - | satty --filename - --fullscreen --output-filename ~/Pictures/Screenshots/satty-$(date '+%Y%m%d-%H:%M:%S').png
```

Hyprland users must escape the `#` with another `#`:

```
grim -g "$(slurp -o -r -c '##ff0000ff')" -t ppm - | satty --filename - --fullscreen --output-filename ~/Pictures/Screenshots/satty-$(date '+%Y%m%d-%H:%M:%S').png
```

Please note we're using ppm in both examples. Compared to png, ppm is uncompressed and this can save time.

### Other examples

#### Image Resize

Satty does not provide a resize mechanism other than cropping. But you can pipe the result to other tools such as ImageMagick:

```
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

## Maintainers and Contributors

Satty wouldn't exist without the help of our contributors and maintainers. Current maintainers are @RobertMueller2 (maintainer), @fabienjuif (maintainer) and @gabm (maintainer, original author). Our contributors are:

<a href="https://github.com/gabm/satty/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=gabm/satty" />
</a>

Made with [contrib.rocks](https://contrib.rocks).

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=gabm/Satty&type=Date)](https://star-history.com/#gabm/Satty&Date)

## License

The source code is released under the MPL-2.0 license.

The Font 'Roboto Regular' from Google is released under Apache-2.0 license.
