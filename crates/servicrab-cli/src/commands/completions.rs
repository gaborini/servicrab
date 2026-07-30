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
pub fn run<C: CommandFactory>(shell: Shell) -> Result<(), String> {
    let mut command = C::command();
    let name = command.get_name().to_string();
    clap_complete::generate(shell, &mut command, name, &mut std::io::stdout());
    Ok(())
}
