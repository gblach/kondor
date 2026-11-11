use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::{self, ExitCode};
use std::time::Duration;

use argp::{EarlyExit, FromArgs};
use console::Term;
use globset::{Glob, GlobBuilder, GlobSet};

mod remove;
mod scan;
mod ui;

use remove::remove;
use scan::{Mtimes, Project, modified, scan, size};
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

    /// Skip directories whose path matches one of these glob patterns, with everything in
    /// them. Takes every argument up to the next option or `--`, so put the directories to
    /// scan first.
    #[argp(option, short = 'e', arg_name = "pattern", from_str_fn(parse_exclude))]
    exclude: Vec<Glob>,

    /// Print the version.
    #[argp(switch, short = 'V')]
    version: bool,

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
    mtimes: Mtimes,
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

fn parse_exclude(value: &str) -> Result<Glob, String> {
    let pattern = value.trim_end_matches('/');
    if pattern.is_empty() {
        return Err(format!("invalid pattern `{value}`"));
    }
    // Scanned paths are made absolute before matching, so the globs must be absolute too.
    let glob = if pattern.starts_with('/') {
        pattern.to_string()
    } else {
        let cwd = env::current_dir().map_err(|e| e.to_string())?;
        let relative = pattern.strip_prefix("./").unwrap_or(pattern);
        Path::new(&globset::escape(&cwd.to_string_lossy()))
            .join(relative)
            .to_string_lossy()
            .into_owned()
    };
    // Without this `*` would also match `/`, so `work/*` would exclude nested directories.
    GlobBuilder::new(&glob)
        .literal_separator(true)
        .build()
        .map_err(|e| format!("invalid pattern `{value}`: {}", e.kind()))
}

/// An unquoted `--exclude work/*` is expanded by the shell into several arguments, so every plain
/// argument after `--exclude` is an exclude. argp takes one value per option, so the option is
/// repeated in front of each further value.
fn repeat_exclude(args: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut args = args.into_iter();
    let mut out = Vec::new();
    // None outside an exclude list, otherwise whether the list already has its first value.
    let mut list = None;
    for arg in args.by_ref() {
        let text = arg.to_string_lossy();
        if text == "--" {
            out.push(arg);
            break;
        }
        if text.starts_with('-') && text.len() > 1 {
            // `-ne` is `-n -e`, so a group of short options ending in `e` starts a list too.
            let exclude = text == "--exclude" || (!text.starts_with("--") && text.ends_with('e'));
            list = exclude.then_some(false);
        } else if list == Some(true) {
            out.push("--exclude".into());
        } else if list == Some(false) {
            list = Some(true);
        }
        out.push(arg);
    }
    out.extend(args);
    out
}

fn parse_args() -> Args {
    let mut args = env::args_os();
    let cmd = args.next().unwrap_or_default();
    let cmd = Path::new(&cmd)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    match Args::from_args(&[&cmd], &repeat_exclude(args)) {
        Ok(args) if args.version => {
            println!("{cmd} {}", env!("CARGO_PKG_VERSION"));
            process::exit(0)
        }
        Ok(args) => args,
        Err(EarlyExit::Help(help)) => {
            println!("{}", help.generate(argp::DEFAULT));
            process::exit(0)
        }
        Err(EarlyExit::Err(e)) => {
            eprintln!("{e}\nRun {cmd} --help for more information.");
            process::exit(1)
        }
    }
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
        let age = modified(&project, &mut state.mtimes)
            .elapsed()
            .unwrap_or_default();
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
    let args = parse_args();
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
    // Checked before anything is cleaned, so a typo in one path does not clean only the others.
    let mut unreadable = false;
    for dir in &dirs {
        if let Err(e) = fs::read_dir(dir) {
            eprintln!("Error: cannot scan {}: {e}", display_path(dir));
            unreadable = true;
        }
    }
    if unreadable {
        return ExitCode::FAILURE;
    }

    let mut excludes = GlobSet::builder();
    for glob in args.exclude {
        excludes.add(glob);
    }
    let excludes = match excludes.build() {
        Ok(excludes) => excludes,
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut state = State {
        found: false,
        clean_all: args.yes,
        // Stays true after "All", so every project in a run gets the same kind of line.
        after_prompt: !args.yes,
        dry_run: args.dry_run,
        force: args.force,
        older: args.older,
        mtimes: Mtimes::new(),
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
        if scan(dir, &[], &excludes, &mut handle).is_break() {
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
    fn exclude_takes_arguments_up_to_the_next_option() {
        let repeat = |args: &[&str]| -> Vec<String> {
            repeat_exclude(args.iter().map(OsString::from))
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            repeat(&["dir", "-e", "a", "b", "c", "-o", "1d", "x"]),
            [
                "dir",
                "-e",
                "a",
                "--exclude",
                "b",
                "--exclude",
                "c",
                "-o",
                "1d",
                "x"
            ]
        );
        assert_eq!(
            repeat(&["-ne", "a", "b", "--", "dir", "-e", "c", "d"]),
            ["-ne", "a", "--exclude", "b", "--", "dir", "-e", "c", "d"]
        );
        assert_eq!(
            repeat(&["--exclude", "a", "--exclude", "b", "c"]),
            ["--exclude", "a", "--exclude", "b", "--exclude", "c"]
        );
        assert_eq!(repeat(&["-n", "a", "b"]), ["-n", "a", "b"]);
    }

    #[test]
    fn parses_excludes() {
        let cwd = env::current_dir().unwrap();
        let matches = |pattern: &str, path: &Path| {
            parse_exclude(pattern)
                .unwrap()
                .compile_matcher()
                .is_match(path)
        };
        assert!(matches("vendor", &cwd.join("vendor")));
        assert!(matches("vendor/", &cwd.join("vendor")));
        assert!(!matches("vendor", &cwd.join("a/vendor")));
        assert!(matches("**/vendor", &cwd.join("a/b/vendor")));
        assert!(matches("*-keep", &cwd.join("old-keep")));
        assert!(!matches("*-keep", &cwd.join("a/old-keep")));
        assert!(matches("./work/api", &cwd.join("work/api")));
        assert!(matches("work/*", &cwd.join("work/api")));
        assert!(!matches("work/*", &cwd.join("work/api/web")));
        assert!(!matches("work/api", &cwd.join("a/work/api")));
        assert!(matches("/srv/*/api", Path::new("/srv/x/api")));
        for bad in ["", "/", "a/[b"] {
            assert!(parse_exclude(bad).is_err(), "{bad}");
        }
    }

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
