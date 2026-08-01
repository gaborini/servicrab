//! `servicrab completions <SHELL>` — print a shell completion script.
//!
//! The script is written to stdout so it can be piped straight into the
//! shell's completion directory:
//!
//! ```console
//! $ servicrab completions bash > /etc/bash_completion.d/servicrab
//! $ servicrab completions zsh  > ~/.zfunc/_servicrab
//! $ servicrab completions fish > ~/.config/fish/completions/servicrab.fish
//! ```

use clap::CommandFactory;
use clap_complete::Shell;

/// Write the completion script for `shell` to stdout.
///
/// Rendered into memory first because the generator writes straight to the
/// stream and panics if a write fails: a reader that closes the pipe early —
/// `servicrab completions bash | head` — would otherwise take the process down
/// over something that is not an error at all.
pub fn run<C: CommandFactory>(shell: Shell) -> Result<(), String> {
    use std::io::Write;

    let mut command = C::command();
    let name = command.get_name().to_string();
    let mut script: Vec<u8> = Vec::new();
    clap_complete::generate(shell, &mut command, name, &mut script);

    match std::io::stdout().write_all(&script) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(err) => Err(format!("failed to write the completion script: {err}")),
    }
}
