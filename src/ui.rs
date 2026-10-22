use std::io;
use std::path::Path;

use console::{Key, Style, Term, measure_text_width};

use crate::scan::Project;

type Rgb = (u8, u8, u8);

const AMBER: Rgb = (255, 180, 84);
const GREEN: Rgb = (120, 200, 120);
const GRAY: Rgb = (150, 150, 150);
const RED: Rgb = (235, 100, 100);
const BLACK: Rgb = (0, 0, 0);
const LIGHT_GRAY: u8 = 250;
const DIM_GRAY: u8 = 244;
const BUTTON_BACKGROUND: u8 = 236;

#[derive(Clone, Copy)]
pub enum Choice {
    Yes,
    No,
    All,
    Quit,
}

struct Button {
    label: &'static str,
    choice: Choice,
    color: Rgb,
}

// Each button is colored by what it does.
const BUTTONS: [Button; 4] = [
    Button {
        label: "Yes",
        choice: Choice::Yes,
        color: GREEN,
    },
    Button {
        label: "No",
        choice: Choice::No,
        color: GRAY,
    },
    Button {
        label: "All",
        choice: Choice::All,
        color: AMBER,
    },
    Button {
        label: "Quit",
        choice: Choice::Quit,
        color: RED,
    },
];

pub enum Outcome {
    Freed(u64),
    Skipped,
    Failed,
    WouldFree(u64),
}

/// Styles for one output stream: the prompt goes to stderr, results go to stdout. Each stream
/// decides on its own whether colors are shown.
struct Paint(Style);

impl Paint {
    fn stdout() -> Self {
        Self(Style::new())
    }

    fn stderr() -> Self {
        Self(Style::new().for_stderr())
    }

    fn plain(&self) -> Style {
        self.0.clone()
    }

    fn rgb(&self, (r, g, b): Rgb) -> Style {
        self.plain().true_color(r, g, b)
    }

    fn accent(&self) -> Style {
        self.rgb(AMBER).bold()
    }

    fn light(&self) -> Style {
        self.plain().color256(LIGHT_GRAY)
    }

    fn dim(&self) -> Style {
        self.plain().color256(DIM_GRAY)
    }
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// File names come from the scanned tree, which may be an untrusted repository. Control
/// characters in them could move the cursor or recolor the screen and fake the prompt, so they
/// are shown as escapes.
fn escape_controls(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

pub fn display_path(path: &Path) -> String {
    escape_controls(&path.display().to_string())
}

fn display_name(path: &Path) -> String {
    escape_controls(&path.file_name().unwrap_or_default().to_string_lossy())
}

fn title(project: &Project, paint: &Paint) -> String {
    let (parent, name) = match project.path.parent() {
        Some(parent) if project.path.file_name().is_some() => {
            let parent = display_path(parent);
            let separator = if parent.is_empty() || parent.ends_with('/') {
                ""
            } else {
                "/"
            };
            (format!("{parent}{separator}"), display_name(&project.path))
        }
        _ => (String::new(), display_path(&project.path)),
    };
    format!(
        "{}{}{}{}",
        paint.dim().apply_to("Clean "),
        paint.plain().apply_to(parent),
        paint.accent().apply_to(name),
        paint
            .dim()
            .apply_to(format!(" ({})?", project.kinds_label()))
    )
}

fn details(project: &Project, paint: &Paint) -> String {
    let names: Vec<String> = project
        .artifacts
        .iter()
        .map(|(path, _)| display_name(path))
        .collect();
    let sizes: Vec<String> = project
        .artifacts
        .iter()
        .map(|(_, bytes)| format_size(*bytes))
        .collect();
    let name_width = names
        .iter()
        .map(|name| measure_text_width(name))
        .max()
        .unwrap_or(0);
    let size_width = sizes.iter().map(String::len).max().unwrap_or(0);
    names
        .iter()
        .zip(&sizes)
        .map(|(name, size)| {
            let padding = " ".repeat(name_width - measure_text_width(name));
            let name = paint.light().apply_to(format!("  {name}{padding}  "));
            let size = paint.accent().apply_to(format!("{size:>size_width$}"));
            format!("{name}{size}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn buttons(selected: usize, paint: &Paint) -> String {
    BUTTONS
        .iter()
        .enumerate()
        .map(|(i, button)| {
            let (r, g, b) = button.color;
            let style = if i == selected {
                paint.rgb(BLACK).on_true_color(r, g, b)
            } else {
                paint.rgb(button.color).on_color256(BUTTON_BACKGROUND)
            };
            let (first, rest) = button.label.split_at(1);
            format!(
                " {}{}{}",
                style.apply_to("  "),
                style.clone().bold().apply_to(first),
                style.apply_to(format!("{rest}  "))
            )
        })
        .collect()
}

/// Rows the text takes on screen, counting lines that wrap.
fn rows(text: &str, columns: usize) -> usize {
    text.lines()
        .map(|line| measure_text_width(line).div_ceil(columns).max(1))
        .sum()
}

/// Clears the current row and `above` rows above it, leaving the cursor on the top one.
fn clear_rows(term: &Term, above: usize) -> io::Result<()> {
    term.clear_line()?;
    if above > 0 {
        term.clear_last_lines(above)?;
    }
    Ok(())
}

fn read_button(term: &Term, paint: &Paint, button_rows: usize) -> io::Result<Choice> {
    let mut selected = 1;
    loop {
        term.write_str(&buttons(selected, paint))?;
        let key = term.read_key_raw()?;
        // On a narrow terminal the buttons line wraps onto more than one row.
        clear_rows(term, button_rows - 1)?;
        match key {
            Key::ArrowLeft | Key::Char('h') => {
                selected = (selected + BUTTONS.len() - 1) % BUTTONS.len()
            }
            Key::ArrowRight | Key::Char('l') => selected = (selected + 1) % BUTTONS.len(),
            Key::Enter => return Ok(BUTTONS[selected].choice),
            Key::Escape | Key::CtrlC => {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            Key::Char(c) => {
                let c = c.to_ascii_uppercase();
                if let Some(button) = BUTTONS.iter().find(|button| button.label.starts_with(c)) {
                    return Ok(button.choice);
                }
            }
            _ => {}
        }
    }
}

pub fn ask(project: &Project) -> io::Result<Choice> {
    let term = Term::stderr();
    let paint = Paint::stderr();
    let head = format!("{}\n{}\n", title(project, &paint), details(project, &paint));
    let columns = (term.size().1 as usize).max(1);
    term.write_line(&head)?;
    term.hide_cursor()?;
    let choice = read_button(&term, &paint, rows(&buttons(0, &paint), columns));
    // The whole prompt is cleared; the title is printed again with the result after it.
    term.clear_last_lines(rows(&head, columns) + 1)?;
    term.show_cursor()?;
    choice
}

/// After a prompt the result goes at the end of the prompt's title; without one (`--yes`,
/// `--dry-run`) each project gets a line of its own.
pub fn print_outcome(project: &Project, after_prompt: bool, outcome: Outcome) {
    let paint = Paint::stdout();
    if after_prompt {
        let result = match outcome {
            Outcome::Freed(bytes) | Outcome::WouldFree(bytes) => {
                paint.accent().apply_to(format_size(bytes))
            }
            Outcome::Skipped => paint.light().apply_to("skipped".into()),
            Outcome::Failed => paint.rgb(RED).apply_to("failed".into()),
        };
        println!("{} {result}", title(project, &paint));
        return;
    }
    let path = display_path(&project.path);
    let kinds = project.kinds_label();
    match outcome {
        Outcome::Freed(bytes) => println!("Cleaned {path} ({kinds}) {}", format_size(bytes)),
        Outcome::Skipped => println!("Skipped {path} ({kinds})"),
        Outcome::Failed => println!("Failed {path} ({kinds})"),
        Outcome::WouldFree(bytes) => {
            println!("Would clean {path} ({kinds}) {}", format_size(bytes));
        }
    }
}

pub fn print_total(found: bool, freed: u64, dry_run: bool) {
    if !found {
        println!("No projects with artifacts found.");
        return;
    }
    let paint = Paint::stdout();
    let label = if dry_run { "Would free" } else { "Freed" };
    println!(
        "{} {}",
        paint.light().apply_to(label),
        paint.accent().apply_to(format_size(freed))
    );
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn project(path: &str, artifacts: &[(&str, u64)]) -> Project {
        Project {
            path: PathBuf::from(path),
            kinds: vec!["Node", "Bun"],
            artifacts: artifacts
                .iter()
                .map(|(name, size)| (Path::new(path).join(name), *size))
                .collect(),
        }
    }

    #[test]
    fn escape_controls_escapes_control_characters() {
        assert_eq!(
            escape_controls("a\x1b[2Kb\rc\u{9b}d"),
            "a\\u{1b}[2Kb\\rc\\u{9b}d"
        );
        assert_eq!(escape_controls("zażółć/build"), "zażółć/build");
    }

    #[test]
    fn format_size_uses_binary_units() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(1536), "1.5 KiB");
        assert_eq!(format_size(5 * 1024 * 1024 * 1024), "5.0 GiB");
        assert_eq!(format_size(3 * 1024_u64.pow(5)), "3072.0 TiB");
    }

    #[test]
    fn rows_counts_wrapped_lines() {
        assert_eq!(rows("abc", 4), 1);
        assert_eq!(rows("abcd", 4), 1);
        assert_eq!(rows("abcde", 4), 2);
        assert_eq!(rows("abc\n\nabcdefghi", 4), 5);
    }

    #[test]
    fn title_splits_path_and_project_name() {
        let plain = Paint(Style::new().force_styling(false));
        assert_eq!(
            title(&project("./api/web", &[]), &plain),
            "Clean ./api/web (Node, Bun)?"
        );
        assert_eq!(
            title(&project("web", &[]), &plain),
            "Clean web (Node, Bun)?"
        );
        assert_eq!(
            title(&project("/web", &[]), &plain),
            "Clean /web (Node, Bun)?"
        );
        assert_eq!(title(&project(".", &[]), &plain), "Clean . (Node, Bun)?");
        assert_eq!(
            title(&project("./a\x1b/b\x1b", &[]), &plain),
            "Clean ./a\\u{1b}/b\\u{1b} (Node, Bun)?"
        );
    }

    #[test]
    fn title_makes_only_the_project_name_bold() {
        let styled = Paint(Style::new().force_styling(true));
        let title = title(&project("./api/web", &[]), &styled);
        let name_start = title.find("web").unwrap();
        assert!(title[..name_start].ends_with("\x1b[1m"));
        assert!(!title[..name_start].contains("\x1b[1m./"));
    }

    #[test]
    fn details_are_aligned() {
        let plain = Paint(Style::new().force_styling(false));
        let details = details(
            &project(".", &[("node_modules", 2048), (".next", 10)]),
            &plain,
        );
        assert_eq!(details, "  node_modules  2.0 KiB\n  .next            10 B");
    }

    #[test]
    fn buttons_match_the_y_n_a_q_keys() {
        let letters: Vec<char> = BUTTONS
            .iter()
            .map(|button| button.label.chars().next().unwrap())
            .collect();
        assert_eq!(letters, ['Y', 'N', 'A', 'Q']);
    }
}
