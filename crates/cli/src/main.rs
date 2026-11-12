//! The `nanoprobe` command. The work is in the library.

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();
    let code = nanoprobe_cli::run(&args, &mut out, &mut err);
    let _ = out.flush();
    std::process::exit(code);
}
