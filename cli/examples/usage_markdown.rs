use clapdown::Options;
use satty_cli::command_line::CommandLine;

fn main() {
    let options = Options::new()
        .title("Command Line Options for Satty")
        .metadata(false);
    print!(
        "<!-- --8<-- [start:cli] -->\n\n{}\n<!-- --8<-- [end:cli] -->\n",
        clapdown::render_from::<CommandLine>(&options).trim_end()
    );
}
