<!-- --8<-- [start:cli] -->

# CLI Options

Modern Screenshot Annotation.

**Usage:** `satty [OPTIONS]`

## Options

`--man`
:   Show manpage. Pipe to `man -l -`. (Exclusive option)

`--license`
:   Show license. (Exclusive option)

`-c`, `--config <CONFIG>`
:   Path to the config file, if specified, this will be an exclusive config. Otherwise, config will be stacked from system and user config using XDG_CONFIG_DIRS/satty/config.toml and XDG_CONFIG_HOME/satty/config.toml

`-f`, `--filename <FILENAME>`
:   Path to input image or `-` to read from stdin. Required unless --from_clipboard is provided

`--from-clipboard <CLIPBOARD_COMMAND>`
:   Configure CLIPBOARD_COMMAND to read clipboard at startup to use instead of filename. (NEXTRELEASE) If CLIPBOARD_COMMAND missing, uses wl-paste. Required unless --filename is provided

`--log-level <LOG_LEVEL>`
:   set log level, note that this is not effective before config has been read (NEXTRELEASE)

    Possible values: `fatal`, `error`, `warn`, `info`, `debug`, `trace`

`--fullscreen <FULLSCREEN>`
:   Start Satty in fullscreen mode. Since 0.20.1, takes optional parameter. --fullscreen without parameter is equivalent to --fullscreen current. Mileage may vary depending on compositor

    Possible values: `all`, `current-screen`

`--resize <MODE|WIDTHxHEIGHT>`
:   Resize to coordinates or use smart mode (0.20.1). --resize without parameter is equivalent to --resize smart

    Possible values: `smart`, `WxH`

`--floating-hack`
:   Try to enforce floating (0.20.1). Mileage may vary depending on compositor

`-o`, `--output-filename <OUTPUT_FILENAME>`
:   Filename to use for saving action or `-` to print to stdout. Omit to disable saving to file. Might contain format specifiers: <https://docs.rs/chrono/latest/chrono/format/strftime/index.html>. Since 0.20.0, can contain tilde (~) for home dir

`--early-exit <EARLY_EXIT>`
:   Exit directly after save action. 0.21.0: changed to accommodate different triggers

    Possible values: `all`, `copy`, `save`, `save-as`

`--corner-roundness <CORNER_ROUNDNESS>`
:   Draw corners of rectangles round if the value is greater than 0 (Defaults to 12) (0 disables rounded corners)

`--initial-tool <TOOL>`
:   Select the tool on startup

    Possible values: `pointer`, `crop`, `pen`, `highlighter`, `brush`, `line`, `arrow`, `rectangle`, `ellipse`, `text`, `marker`, `fringe-pixelate`, `pixelate`, `fringe`, `blur`, `lensblur`, `highlight`, `spotlight`, `image`

`--copy-command <COPY_COMMAND>`
:   Configure the command to be called on copy, for example `wl-copy`

`--annotation-size-factor <ANNOTATION_SIZE_FACTOR>`
:   Increase or decrease the size of the annotations

`--save-after-copy`
:   After copying the screenshot, save it to a file as well

`--auto-copy`
:   Automatically copy to clipboard after every annotation change (0.21.0)

`--actions-on-enter <ACTIONS_ON_ENTER>`
:   Actions to perform when pressing Enter

    Possible values: `save-to-clipboard`, `save-to-file`, `save-to-file-as`, `copy-filepath-to-clipboard`, `exit`

`--actions-on-escape <ACTIONS_ON_ESCAPE>`
:   Actions to perform when pressing Escape

    Possible values: `save-to-clipboard`, `save-to-file`, `save-to-file-as`, `copy-filepath-to-clipboard`, `exit`

`--actions-on-right-click <ACTIONS_ON_RIGHT_CLICK>`
:   Actions to perform when hitting the copy Button

    Possible values: `save-to-clipboard`, `save-to-file`, `save-to-file-as`, `copy-filepath-to-clipboard`, `exit`

`-d`, `--default-hide-toolbars`
:   Hide toolbars by default

`--focus-toggles-toolbars`
:   Experimental (since 0.20.0): Whether to toggle toolbars based on focus. Doesn't affect initial state

`--default-fill-shapes`
:   Experimental feature (since 0.20.0): Fill shapes by default

`--font-family <FONT_FAMILY>`
:   Font family to use for text annotations

`--font-style <FONT_STYLE>`
:   Font style to use for text annotations

`--disable-notifications`
:   Disable notifications

`--no-window-decoration`
:   Disable the window decoration (title bar, borders, etc.) Please note that the compositor has the final say in this. Requires xdg-decoration-unstable-v1

`--brush-smooth-history-size <BRUSH_SMOOTH_HISTORY_SIZE>`
:   Experimental feature: How many points to use for the brush smoothing algorithm. 0 disables smoothing. The default value is 0 (disabled)

`--zoom-factor <ZOOM_FACTOR>`
:   Experimental feature (0.20.1): The zoom factor to use for the image. 1.0 means no zoom. defaults to 1.1

`--pan-step-size <PAN_STEP_SIZE>`
:   Experimental feature (0.20.1): The pan step size to use when panning with arrow keys. defaults to 50.0

`--text-move-length <TEXT_MOVE_LENGTH>`
:   Experimental feature (0.20.1): The length to move the text when using the arrow keys. defaults to 50.0

`--input-scale <INPUT_SCALE>`
:   Experimental feature (0.20.1): Scale the default window size to fit different displays. Note that before 0.21.0 this is ignored with explicit resize

`--title <TITLE>`
:   Experimental feature (0.21.0): Set window title

`--app-id <APP_ID>`
:   Experimental feature (0.21.0): Set toplevel app_id. Note that this has to match D-Bus well known name format, otherwise GTK does not accept it

`--notification-thumbnail <NOTIFICATION_THUMBNAIL>`
:   Experimental feature (0.22.0): use preview thumbnail in notifications where available

    Possible values: `screenshot`, `app-icon`

`--profile-startup`
:   Print profiling, deprecated since NEXTRELEASE Preferably use `log-level` option with at least `debug` instead

`--primary-highlighter <PRIMARY_HIGHLIGHTER>`
:   The primary highlighter to use, secondary is accessible with CTRL, deprecated since NEXTRELEASE

    Possible values: `block`, `freehand`

`--right-click-copy`
:   Right click to copy, deprecated since 0.19.0 Preferably use the `action_on_right_click` option instead

`--action-on-enter <ACTION_ON_ENTER>`
:   Action to perform when pressing Enter, deprecated since 0.19.0 Preferably use the `actions_on_enter` option instead

    Possible values: `save-to-clipboard`, `save-to-file`, `save-to-file-as`, `copy-filepath-to-clipboard`, `exit`
<!-- --8<-- [end:cli] -->
