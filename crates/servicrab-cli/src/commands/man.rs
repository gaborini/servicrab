//! `servicrab man` — print a roff man page to stdout, or write the whole set.
//!
//! Written for packagers and for anyone installing from source:
//!
//! ```console
//! $ servicrab man > /usr/local/share/man/man1/servicrab.1
//! $ servicrab man | man -l -                            # preview it
//! $ servicrab man --output /usr/local/share/man/man1     # one page per command
//! ```
//!
//! Released tarballs already contain the generated pages under `man/`.
//!
//! The main page's `SUBCOMMANDS` section cross-references `servicrab-up(1)` and
//! friends, so `--output` writes those too; stdout can only carry one page and
//! carries the main one.
//!
//! The bulk of every page comes from the clap definitions, so they cannot drift
//! away from `--help`.  The sections clap knows nothing about — files,
//! environment variables, exit codes — are appended here.

use std::io::Write;
use std::path::Path;

use clap::CommandFactory;

/// Sections clap cannot derive, in roff.
///
/// `.SH` starts a section, `.TP` a tagged paragraph, `\fB…\fR` sets bold.  A
/// backslash is doubled in these string literals because roff and Rust both
/// want it.
const EXTRA_SECTIONS: &str = r#".SH FILES
.TP
\fBservicrab.toml\fR
The project configuration.  Commands that take \fB--config\fR default to
discovering it by walking up from the current directory.
.TP
\fB.servicrab/daemon.sock\fR
The socket \fBservicrab start\fR listens on, created next to the configuration
file with mode 0600.  Connecting to it is enough to control every service in the
project, so the file permissions are the whole access control.  When the path
would exceed the platform's socket length limit, the socket moves to the
temporary directory instead.
.TP
\fB.servicrab/daemon.pid\fR
The process id of the running daemon.
.TP
\fB.servicrab/daemon.log\fR
The detached daemon's own output.  Service output goes to the directory named by
\fB[project.logs]\fR, not here.
.SH ENVIRONMENT
.TP
\fBRUST_LOG\fR
Overrides the log filter, using the \fBtracing\-subscriber\fR syntax, for
example \fBRUST_LOG=debug\fR or \fBRUST_LOG=servicrab_core=debug\fR.  Diagnostics
go to stderr; stdout carries command output and the output of supervised
services.
.TP
\fBNO_COLOR\fR
Set to any value to disable coloured output.  Colour is also disabled when
\fBTERM\fR is \fBdumb\fR, and for whichever of stdout and stderr is not a
terminal.
.TP
\fBCLICOLOR_FORCE\fR
Set to anything other than \fB0\fR to colour output even when it is redirected
to a pipe or a file.  \fBNO_COLOR\fR wins over it; \fB--color\fR wins over both.
.SH EXIT STATUS
.TP
\fB0\fR
Success.  For \fBrun\fR and \fBup\fR this means the services were shut down as
asked, not that they never failed.  \fBdown\fR uses it when a daemon was there
and stopped.
.TP
\fB1\fR
The command failed: an invalid configuration, an unknown service, a service that
exhausted its restart budget, a per-service command the daemon refused, or a
\fBstart --wait\fR that timed out.
.TP
\fB3\fR
No daemon is running for this project.  Its own status so that a script can tell
"there is nothing to talk to" from a real failure, and it is what \fBstatus\fR,
\fBstop\fR, \fBrestart\fR, \fBstart\fR \fISERVICE\fR, \fBreload\fR,
\fBevents\fR and \fBdown\fR all report.  \fBdown\fR still does not \fIfail\fR
because nothing was running — this only says there was nothing to do.
.TP
\fB126\fR, \fB127\fR
\fBexec\fR could not run the command: found but not executable (126), or not
found (127).  These follow the shell convention, so a script can tell a missing
command from one that ran and failed.
.TP
\fB129\fR, \fB130\fR, \fB143\fR
\fBup\fR and \fBwatch\fR were cut short by a signal and shut the stack down
cleanly: \fBSIGHUP\fR (129), Ctrl+C (130), or \fBSIGTERM\fR (143).  These follow
the \fB128+N\fR convention, and a clean shutdown is what they mean — not a
failure.
.TP
\fBanything else\fR
\fBexec\fR and \fBrun\fR pass through the status of the process they ran: its own
exit code, or \fB128+N\fR when a signal \fIN\fR killed it.
.PP
Every error is written to standard error, prefixed with \fBerror: \fR, with the
individual problems as bullets below it.  Under \fB--json\fR it is a JSON object
on standard error instead, carrying \fBschema_version\fR and a stable \fBcode\fR,
so that standard output holds nothing but the document that was asked for.
.SH SEE ALSO
Full documentation, including the configuration reference, at
\fBhttps://github.com/gaborini/servicrab\fR
"#;

/// Render the roff man page for `C`.
///
/// Rendered section by section rather than through `Man::render` so that the
/// hand-written sections land where a reader expects them: after the options,
/// before the version and author trailer.
fn page<C: CommandFactory>() -> std::io::Result<Vec<u8>> {
    // `help` is left out for the same reason the per-command pages leave it
    // out: SUBCOMMANDS is a list of cross-references, and there is no
    // servicrab-help(1) to reference.
    let man = clap_mangen::Man::new(C::command().disable_help_subcommand(true)).section("1");
    let mut out: Vec<u8> = Vec::new();

    man.render_title(&mut out)?;
    man.render_name_section(&mut out)?;
    man.render_synopsis_section(&mut out)?;
    man.render_description_section(&mut out)?;
    man.render_options_section(&mut out)?;
    man.render_subcommands_section(&mut out)?;
    out.write_all(EXTRA_SECTIONS.as_bytes())?;
    man.render_version_section(&mut out)?;
    man.render_authors_section(&mut out)?;

    Ok(out)
}

/// Print the main page to stdout, or write every page into `output`.
pub fn run<C: CommandFactory>(output: Option<&Path>) -> Result<(), String> {
    let page = page::<C>().map_err(|e| format!("failed to render the man page: {e}"))?;

    let Some(dir) = output else {
        return match std::io::stdout().write_all(&page) {
            Ok(()) => Ok(()),
            // `servicrab man | head` closes the pipe partway through the page,
            // and that is the reader saying it has seen enough — not a failure
            // to report and not a reason to exit non-zero.
            Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
            Err(err) => Err(format!("failed to write the man page: {err}")),
        };
    };

    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;

    // `build` is what fills in the subcommands' display names, and those are
    // what the per-command files are named after.
    let mut command = C::command().disable_help_subcommand(true);
    command.build();
    let name = command.get_name().to_string();

    for subcommand in command
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set())
        .cloned()
    {
        let man = clap_mangen::Man::new(subcommand).section("1");
        let file = man.get_filename();
        man.generate_to(dir)
            .map_err(|e| format!("could not write {}: {e}", dir.join(&file).display()))?;
        println!("{}", dir.join(file).display());
    }

    // Written last, and by hand, because this is the page with the sections
    // clap cannot derive.
    let main = dir.join(format!("{name}.1"));
    std::fs::write(&main, &page).map_err(|e| format!("could not write {}: {e}", main.display()))?;
    println!("{}", main.display());

    Ok(())
}
