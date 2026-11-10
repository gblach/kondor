use std::io;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use argp::FromArgs;
use console::Term;

mod remove;
mod scan;
mod ui;

use remove::remove;
use scan::{Project, modified, scan, size};
use ui::{Choice, Outcome, ask, display_path, print_outcome, print_total};

/// Clean build artifacts from software projects, including projects nested in other projects.
#[derive(FromArgs)]
struct Args {
    /// Clean all found projects without asking.
    #[argp(switch, short = 'y')]
    yes: bool,

    /// List what would be cleaned without removing anything.
    #[argp(switch, short = 'n')]
    dry_run: bool,

    /// Make read-only directories writable so they can be removed.
    #[argp(switch, short = 'f')]
    force: bool,

    /// Only clean projects with no file changed for this long, e.g. 20d. Units: m, h, d, w,
    /// M (30 days), y (365 days).
    #[argp(option, short = 'o', arg_name = "age", from_str_fn(parse_age))]
    older: Option<Duration>,

    /// Directories to scan (default: current directory).
    #[argp(positional)]
    dirs: Vec<PathBuf>,
}

struct State {
    found: bool,
    clean_all: bool,
    after_prompt: bool,
    dry_run: bool,
    force: bool,
    older: Option<Duration>,
    failed: bool,
    freed: u64,
}

fn parse_age(value: &str) -> Result<Duration, String> {
    const DAY: u64 = 24 * 60 * 60;
    let error = || format!("invalid age `{value}`, expected a number and m, h, d, w, M or y");
    let unit = value.chars().last().ok_or_else(error)?;
    let unit_secs = match unit {
        'm' => 60,
        'h' => 60 * 60,
        'd' => DAY,
        'w' => 7 * DAY,
        'M' => 30 * DAY,
        'y' => 365 * DAY,
        _ => return Err(error()),
    };
    let count: u64 = value[..value.len() - 1].parse().map_err(|_| error())?;
    let secs = count.checked_mul(unit_secs).ok_or_else(error)?;
    Ok(Duration::from_secs(secs))
}

/// Returns the bytes freed and a message for each artifact that could not be removed.
fn clean(root: &Path, project: &Project, force: bool) -> (u64, Vec<String>) {
    let mut freed = 0;
    let mut errors = Vec::new();
    for (path, bytes) in &project.artifacts {
        if let Err(e) = remove(root, path, force) {
            errors.push(format!("Failed to remove {}: {e}", display_path(path)));
        }
        // A removal that fails partway has already deleted part of the artifact.
        freed += bytes.saturating_sub(size(path));
    }
    (freed, errors)
}

fn on_project(root: &Path, project: Project, state: &mut State) -> io::Result<ControlFlow<()>> {
    if let Some(older) = state.older {
        // A modification time in the future fails `elapsed`, so the project counts as new.
        let age = modified(&project).elapsed().unwrap_or_default();
        if age < older {
            return Ok(ControlFlow::Continue(()));
        }
    }
    state.found = true;
    if state.dry_run {
        state.freed += project.total();
        print_outcome(&project, false, Outcome::WouldFree(project.total()));
        return Ok(ControlFlow::Continue(()));
    }
    if !state.clean_all {
        match ask(&project)? {
            Choice::Yes => {}
            Choice::All => state.clean_all = true,
            Choice::Quit => return Ok(ControlFlow::Break(())),
            Choice::No => {
                print_outcome(&project, state.after_prompt, Outcome::Skipped);
                return Ok(ControlFlow::Continue(()));
            }
        }
    }
    let (freed, errors) = clean(root, &project, state.force);
    state.freed += freed;
    state.failed |= !errors.is_empty();
    let outcome = if freed == 0 && !errors.is_empty() {
        Outcome::Failed
    } else {
        Outcome::Freed(freed)
    };
    print_outcome(&project, state.after_prompt, outcome);
    for error in errors {
        eprintln!("{error}");
    }
    Ok(ControlFlow::Continue(()))
}

fn main() -> ExitCode {
    let args: Args = argp::parse_args_or_exit(argp::DEFAULT);
    // read_key never returns a real key without a terminal, so the prompt would hang.
    if !args.yes && !args.dry_run && !Term::stderr().is_term() {
        eprintln!(
            "Error: an interactive terminal is needed to ask, use --yes to clean without asking"
        );
        return ExitCode::FAILURE;
    }
    let dirs = if args.dirs.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        args.dirs
    };

    let mut state = State {
        found: false,
        clean_all: args.yes,
        // Stays true after "All", so every project in a run gets the same kind of line.
        after_prompt: !args.yes,
        dry_run: args.dry_run,
        force: args.force,
        older: args.older,
        failed: false,
        freed: 0,
    };
    for dir in &dirs {
        let mut handle = |project| match on_project(dir, project, &mut state) {
            Ok(flow) => flow,
            // Esc in the prompt returns Interrupted; that is a normal way to stop.
            Err(e) if e.kind() == io::ErrorKind::Interrupted => ControlFlow::Break(()),
            Err(e) => {
                eprintln!("Error: {e}");
                state.failed = true;
                ControlFlow::Break(())
            }
        };
        if scan(dir, &[], &mut handle).is_break() {
            break;
        }
    }
    print_total(state.found, state.freed, state.dry_run);
    if state.failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ages() {
        assert_eq!(parse_age("30m"), Ok(Duration::from_secs(30 * 60)));
        assert_eq!(parse_age("2h"), Ok(Duration::from_secs(2 * 3600)));
        assert_eq!(parse_age("20d"), Ok(Duration::from_secs(20 * 86400)));
        assert_eq!(parse_age("1w"), Ok(Duration::from_secs(7 * 86400)));
        assert_eq!(parse_age("3M"), Ok(Duration::from_secs(90 * 86400)));
        assert_eq!(parse_age("1y"), Ok(Duration::from_secs(365 * 86400)));
        for bad in [
            "",
            "d",
            "20",
            "20 d",
            "-5d",
            "1.5d",
            "20x",
            "99999999999999y",
        ] {
            assert!(parse_age(bad).is_err(), "{bad}");
        }
    }
}
