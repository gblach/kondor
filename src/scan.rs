use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, Metadata};
use std::ops::ControlFlow;
use std::os::unix::fs::MetadataExt;
use std::path::{self, Component, Path, PathBuf};
use std::time::SystemTime;

use globset::{Glob, GlobSet};

struct Kind {
    name: &'static str,
    markers: &'static [&'static str],
    /// Files that projects of any type can have, like `Makefile`. They count only when no
    /// kind is found by its `markers`.
    weak_markers: &'static [&'static str],
    artifact_dirs: &'static [&'static str],
    artifact_files: &'static [&'static str],
    /// Names of kinds that are not reported next to this one, because their marker is part of
    /// this kind too, like Node's `package.json` in a Bun project.
    overrides: &'static [&'static str],
}

/// Patterns starting with `*` match by suffix. Artifacts containing `/` are nested paths, and
/// artifacts starting with `**/` are found at any depth below the project.
const KINDS: &[Kind] = &[
    Kind {
        name: "Bun",
        markers: &["bun.lock", "bun.lockb", "bunfig.toml"],
        weak_markers: &[],
        artifact_dirs: &[
            "node_modules",
            ".angular",
            ".astro",
            ".next",
            ".nuxt",
            ".parcel-cache",
            ".svelte-kit",
            ".turbo",
            ".vite",
        ],
        artifact_files: &[],
        overrides: &["Node"],
    },
    Kind {
        name: "C/C++",
        markers: &["CMakeLists.txt"],
        weak_markers: &["Makefile"],
        artifact_dirs: &["build", "cmake-build-debug", "cmake-build-release"],
        artifact_files: &[],
        overrides: &[],
    },
    Kind {
        name: "Cargo",
        markers: &["Cargo.toml"],
        weak_markers: &[],
        artifact_dirs: &["target"],
        artifact_files: &[],
        overrides: &[],
    },
    Kind {
        name: "Dart",
        markers: &["pubspec.yaml"],
        weak_markers: &[],
        artifact_dirs: &[".dart_tool", "build"],
        artifact_files: &[],
        overrides: &[],
    },
    Kind {
        name: "Deno",
        markers: &["deno.json", "deno.jsonc"],
        weak_markers: &[],
        artifact_dirs: &[
            "node_modules",
            "vendor",
            ".angular",
            ".astro",
            ".next",
            ".nuxt",
            ".parcel-cache",
            ".svelte-kit",
            ".turbo",
            ".vite",
        ],
        artifact_files: &[],
        overrides: &["Node"],
    },
    Kind {
        name: "Elixir",
        markers: &["mix.exs"],
        weak_markers: &[],
        artifact_dirs: &["_build", "deps"],
        artifact_files: &[],
        overrides: &[],
    },
    Kind {
        name: "Go",
        markers: &["go.mod"],
        weak_markers: &[],
        artifact_dirs: &["vendor"],
        artifact_files: &[],
        overrides: &[],
    },
    Kind {
        name: "Gradle",
        markers: &["build.gradle", "build.gradle.kts"],
        weak_markers: &[],
        artifact_dirs: &["build", ".gradle"],
        artifact_files: &[],
        overrides: &[],
    },
    Kind {
        name: "Haskell",
        markers: &["stack.yaml", "cabal.project", "*.cabal", "package.yaml"],
        weak_markers: &[],
        artifact_dirs: &[".stack-work", "dist-newstyle"],
        artifact_files: &[],
        overrides: &[],
    },
    Kind {
        name: "KiCad",
        markers: &["*.kicad_pro"],
        weak_markers: &[],
        artifact_dirs: &["*-backups"],
        artifact_files: &["fp-info-cache", "*.kicad_pcb-bak", "*.kicad_sch-bak"],
        overrides: &[],
    },
    Kind {
        name: "Maven",
        markers: &["pom.xml"],
        weak_markers: &[],
        artifact_dirs: &["target"],
        artifact_files: &[],
        overrides: &[],
    },
    Kind {
        name: "Meson",
        markers: &["meson.build"],
        weak_markers: &[],
        artifact_dirs: &["build", "builddir", "_build"],
        artifact_files: &[],
        overrides: &[],
    },
    Kind {
        name: ".NET",
        markers: &["*.csproj", "*.fsproj", "*.vbproj"],
        weak_markers: &[],
        artifact_dirs: &["bin", "obj"],
        artifact_files: &[],
        overrides: &[],
    },
    Kind {
        name: "Node",
        markers: &["package.json"],
        weak_markers: &[],
        artifact_dirs: &[
            "node_modules",
            ".angular",
            ".astro",
            ".next",
            ".nuxt",
            ".parcel-cache",
            ".svelte-kit",
            ".turbo",
            ".vite",
        ],
        artifact_files: &[],
        overrides: &[],
    },
    Kind {
        name: "PHP",
        markers: &["composer.json"],
        weak_markers: &[],
        artifact_dirs: &["vendor"],
        artifact_files: &[],
        overrides: &[],
    },
    Kind {
        name: "Python",
        markers: &[
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
            "Pipfile",
            "poetry.lock",
        ],
        weak_markers: &[],
        artifact_dirs: &[
            "**/__pycache__",
            ".venv",
            "venv",
            ".mypy_cache",
            ".pytest_cache",
            ".ruff_cache",
            ".tox",
            ".eggs",
            "*.egg-info",
        ],
        artifact_files: &[],
        overrides: &[],
    },
    Kind {
        name: "Ruby",
        markers: &["Gemfile"],
        weak_markers: &[],
        artifact_dirs: &[".bundle", "vendor/bundle"],
        artifact_files: &[],
        overrides: &[],
    },
    Kind {
        name: "Scala",
        markers: &["build.sbt"],
        weak_markers: &[],
        artifact_dirs: &["target", "project/target"],
        artifact_files: &[],
        overrides: &[],
    },
    Kind {
        name: "Swift",
        markers: &["Package.swift"],
        weak_markers: &[],
        artifact_dirs: &[".build", ".swiftpm"],
        artifact_files: &[],
        overrides: &[],
    },
    Kind {
        name: "Zig",
        markers: &["build.zig"],
        weak_markers: &[],
        artifact_dirs: &["zig-cache", ".zig-cache", "zig-out"],
        artifact_files: &[],
        overrides: &[],
    },
];

const KONDOR_FILE: &str = ".kondor";

pub struct Project {
    pub path: PathBuf,
    pub kinds: Vec<&'static str>,
    pub artifacts: Vec<(PathBuf, u64)>,
    /// Newest modification time of the files in the artifacts.
    pub artifacts_modified: SystemTime,
}

impl Project {
    pub fn total(&self) -> u64 {
        self.artifacts.iter().map(|(_, bytes)| bytes).sum()
    }

    pub fn kinds_label(&self) -> String {
        self.kinds.join(", ")
    }
}

/// Artifact paths read from a `.kondor` file.
struct KondorFile {
    includes: Vec<PathBuf>,
    excludes: Vec<PathBuf>,
}

fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

fn file_modified(meta: &Metadata) -> SystemTime {
    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Hard links to one file share its data, so only the first link found adds to the size.
/// `seen` holds the device and inode numbers of the hard-linked files counted so far.
fn file_stats(meta: &Metadata, seen: &mut HashSet<(u64, u64)>) -> (u64, SystemTime) {
    let counted = meta.nlink() < 2 || seen.insert((meta.dev(), meta.ino()));
    let bytes = if counted { meta.len() } else { 0 };
    (bytes, file_modified(meta))
}

/// Returns the total size and the newest modification time of the files in a directory.
fn dir_stats(path: &Path, seen: &mut HashSet<(u64, u64)>) -> (u64, SystemTime) {
    let Ok(entries) = fs::read_dir(path) else {
        return (0, SystemTime::UNIX_EPOCH);
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => dir_stats(&entry.path(), seen),
            _ => entry
                .metadata()
                .map_or((0, SystemTime::UNIX_EPOCH), |meta| file_stats(&meta, seen)),
        })
        .fold((0, SystemTime::UNIX_EPOCH), |(bytes, newest), (b, m)| {
            (bytes + b, newest.max(m))
        })
}

/// Size and modification time of a directory's contents or of a single file.
fn stats(path: &Path) -> (u64, SystemTime) {
    if is_real_dir(path) {
        dir_stats(path, &mut HashSet::new())
    } else {
        fs::symlink_metadata(path).map_or((0, SystemTime::UNIX_EPOCH), |meta| {
            (meta.len(), file_modified(&meta))
        })
    }
}

/// Size of a directory's contents or of a single file; 0 when the path does not exist.
pub fn size(path: &Path) -> u64 {
    stats(path).0
}

/// Newest modification time of the files below each directory walked by `newest_source`.
pub type Mtimes = HashMap<PathBuf, SystemTime>;

/// Directory mtimes are left out, because removing an artifact updates its parent's mtime and
/// would make a cleaned project look recently used. Hidden directories are skipped like in
/// `scan`, so a `git fetch` into `.git` does not count as work on the project.
fn newest_source(dir: &Path, artifacts: &[(PathBuf, u64)], cache: &mut Mtimes) -> SystemTime {
    let newest = fs::read_dir(dir).map_or(SystemTime::UNIX_EPOCH, |entries| {
        entries
            .flatten()
            .map(|entry| {
                let path = entry.path();
                match entry.file_type() {
                    Ok(kind) if kind.is_dir() => {
                        let hidden = entry.file_name().as_encoded_bytes().starts_with(b".");
                        if hidden || artifacts.iter().any(|(artifact, _)| *artifact == path) {
                            SystemTime::UNIX_EPOCH
                        } else {
                            newest_source(&path, artifacts, cache)
                        }
                    }
                    _ => entry
                        .metadata()
                        .map_or(SystemTime::UNIX_EPOCH, |meta| file_modified(&meta)),
                }
            })
            .max()
            .unwrap_or(SystemTime::UNIX_EPOCH)
    });
    cache.insert(dir.to_path_buf(), newest);
    newest
}

/// Newest modification time of any file in the project, artifacts and nested projects included.
/// The walk of a project already covers the projects nested in it, so their times are taken
/// from `cache` instead of walking their files again.
pub fn modified(project: &Project, cache: &mut Mtimes) -> SystemTime {
    let source = match cache.get(&project.path) {
        Some(newest) => *newest,
        None => {
            // The scan has left the tree walked last, so its times are not needed anymore.
            cache.clear();
            newest_source(&project.path, &project.artifacts, cache)
        }
    };
    project.artifacts_modified.max(source)
}

/// Paths from `.kondor` may come from untrusted repos, so they must stay inside the project.
fn is_contained(path: &str) -> bool {
    Path::new(path)
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
}

/// A symlink anywhere in a nested path would make the removal escape the project.
fn is_real_nested_dir(dir: &Path, relative: &str) -> bool {
    let mut path = dir.to_path_buf();
    Path::new(relative).components().all(|component| {
        path.push(component);
        is_real_dir(&path)
    })
}

/// Each path component is a glob matched one level at a time, so matches never go through
/// symlinks. Only the last component can match files.
fn glob_paths(dir: &Path, pattern: &str) -> Vec<PathBuf> {
    if !is_contained(pattern) {
        return Vec::new();
    }
    let mut paths = vec![dir.to_path_buf()];
    let mut components = Path::new(pattern).components().peekable();
    while let Some(component) = components.next() {
        let Ok(glob) = Glob::new(&component.as_os_str().to_string_lossy()) else {
            return Vec::new();
        };
        let glob = glob.compile_matcher();
        let last = components.peek().is_none();
        paths = paths
            .iter()
            .filter_map(|path| fs::read_dir(path).ok())
            .flat_map(|entries| entries.flatten())
            .filter(|entry| {
                entry
                    .file_type()
                    .is_ok_and(|kind| kind.is_dir() || (last && kind.is_file()))
            })
            .filter(|entry| glob.is_match(entry.file_name()))
            .map(|entry| entry.path())
            .collect();
    }
    paths.sort();
    paths
}

fn matches(pattern: &str, name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    match pattern.strip_prefix('*') {
        Some(suffix) => name.ends_with(suffix),
        None => name == pattern,
    }
}

/// Returns the sorted file and directory names in `dir`. Symlinks are left out.
fn read_entries(dir: &Path) -> Option<(Vec<OsString>, Vec<OsString>)> {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for entry in fs::read_dir(dir).ok()?.flatten() {
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => dirs.push(entry.file_name()),
            Ok(kind) if kind.is_file() => files.push(entry.file_name()),
            _ => {}
        }
    }
    files.sort();
    dirs.sort();
    Some((files, dirs))
}

struct Patterns {
    dirs: Vec<&'static str>,
    files: Vec<&'static str>,
    /// Whether `build` and `dist` are checked with `is_python_output`.
    python: bool,
}

/// `build` and `dist` are common names for hand-written directories too, so in a Python
/// project they are artifacts only when everything in them looks like packaging output.
fn is_python_output(dir: &Path, name: &OsStr) -> bool {
    const BUILD_DIRS: [&str; 4] = ["lib.", "bdist.", "temp.", "scripts-"];
    const DIST_FILES: [&str; 4] = [".whl", ".tar.gz", ".zip", ".egg"];
    let all = |names: &[OsString], ok: &dyn Fn(&str) -> bool| {
        names.iter().all(|name| name.to_str().is_some_and(ok))
    };
    let is_build = name == "build";
    if !is_build && name != "dist" {
        return false;
    }
    let Some((files, dirs)) = read_entries(&dir.join(name)) else {
        return false;
    };
    if is_build {
        let ok = |n: &str| n == "lib" || BUILD_DIRS.iter().any(|prefix| n.starts_with(prefix));
        files.is_empty() && all(&dirs, &ok)
    } else {
        let ok = |n: &str| DIST_FILES.iter().any(|suffix| n.ends_with(suffix));
        dirs.is_empty() && all(&files, &ok)
    }
}

/// Returns the names of the detected kinds and their artifact patterns.
fn detect_kinds(files: &[OsString]) -> (Vec<&'static str>, Patterns) {
    let has = |markers: &[&str]| {
        markers
            .iter()
            .any(|marker| files.iter().any(|file| matches(marker, file)))
    };
    let mut matched: Vec<&Kind> = KINDS.iter().filter(|kind| has(kind.markers)).collect();
    if matched.is_empty() {
        matched = KINDS.iter().filter(|kind| has(kind.weak_markers)).collect();
    }
    let mut kinds = Vec::new();
    let mut patterns = Patterns {
        dirs: Vec::new(),
        files: Vec::new(),
        python: false,
    };
    for kind in &matched {
        if !matched
            .iter()
            .any(|other| other.overrides.contains(&kind.name))
        {
            kinds.push(kind.name);
        }
        patterns.dirs.extend(kind.artifact_dirs);
        patterns.files.extend(kind.artifact_files);
        patterns.python |= kind.name == "Python";
    }
    (kinds, patterns)
}

impl KondorFile {
    fn read(dir: &Path, files: &[OsString]) -> Self {
        let text = if files.iter().any(|file| file == KONDOR_FILE) {
            fs::read_to_string(dir.join(KONDOR_FILE)).unwrap_or_default()
        } else {
            String::new()
        };
        let (excludes, includes): (Vec<&str>, Vec<&str>) = text
            .lines()
            .map(|line| line.trim().trim_end_matches('/'))
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .partition(|line| line.starts_with('!'));
        Self {
            includes: includes
                .iter()
                .flat_map(|line| glob_paths(dir, line))
                .collect(),
            excludes: excludes
                .iter()
                .flat_map(|line| glob_paths(dir, line[1..].trim_start()))
                .collect(),
        }
    }
}

/// Adds the directories called `name` at any depth below `dir` to `found`. Hidden directories,
/// paths already in `found` and the artifacts of nested projects are not searched, so a nested
/// project's `node_modules` or virtual environment does not add thousands of entries.
fn deep_dirs(dir: &Path, name: &str, found: &mut Vec<PathBuf>) {
    let Some((files, dirs)) = read_entries(dir) else {
        return;
    };
    let (_, patterns) = detect_kinds(&files);
    for child in dirs {
        let path = dir.join(&child);
        if found.contains(&path) {
            continue;
        }
        if child == name {
            found.push(path);
            continue;
        }
        let hidden = child.as_encoded_bytes().starts_with(b".");
        let artifact = patterns.dirs.iter().any(|pattern| matches(pattern, &child))
            || (patterns.python && is_python_output(dir, &child));
        // A virtual environment in a directory without a Python marker is not an artifact.
        if !hidden && !artifact && !path.join("pyvenv.cfg").exists() {
            deep_dirs(&path, name, found);
        }
    }
}

/// Returns the artifact paths and whether the `.kondor` file added any of them.
fn collect_artifacts(
    dir: &Path,
    files: &[OsString],
    dirs: &[OsString],
    patterns: &Patterns,
    kondor: KondorFile,
) -> (Vec<PathBuf>, bool) {
    let matching = |names: &[OsString], patterns: &[&str], python: bool| -> Vec<PathBuf> {
        names
            .iter()
            .filter(|name| {
                patterns.iter().any(|pattern| matches(pattern, name))
                    || (python && is_python_output(dir, name))
            })
            .map(|name| dir.join(name))
            .collect()
    };
    let mut artifacts = matching(dirs, &patterns.dirs, patterns.python);
    artifacts.extend(matching(files, &patterns.files, false));
    let (deep, shallow): (Vec<&str>, Vec<&str>) = patterns
        .dirs
        .iter()
        .partition(|pattern| pattern.starts_with("**/"));
    for pattern in shallow.iter().filter(|pattern| pattern.contains('/')) {
        let path = dir.join(pattern);
        if !artifacts.contains(&path) && is_real_nested_dir(dir, pattern) {
            artifacts.push(path);
        }
    }
    for pattern in deep {
        deep_dirs(dir, &pattern[3..], &mut artifacts);
    }
    artifacts.retain(|path| !kondor.excludes.contains(path));
    let builtin_count = artifacts.len();
    for path in kondor.includes {
        if !artifacts.contains(&path) && !kondor.excludes.contains(&path) {
            artifacts.push(path);
        }
    }
    let from_kondor = artifacts.len() > builtin_count;
    (artifacts, from_kondor)
}

fn is_excluded(path: &Path, excludes: &GlobSet) -> bool {
    !excludes.is_empty() && path::absolute(path).is_ok_and(|path| excludes.is_match(path))
}

/// Unlike kondo, scanning continues below a detected project so nested projects are found too.
/// Only artifact directories are skipped. `skip` carries ancestors' nested artifact paths
/// (like Ruby's `vendor/bundle`) which cannot be pruned at the ancestor's level.
/// Projects are passed to `on_project` as soon as they are found, so the caller can clean them
/// before the scan continues; `Break` stops the scan.
pub fn scan(
    dir: &Path,
    skip: &[PathBuf],
    excludes: &GlobSet,
    on_project: &mut impl FnMut(Project) -> ControlFlow<()>,
) -> ControlFlow<()> {
    let Some((files, dirs)) = read_entries(dir) else {
        return ControlFlow::Continue(());
    };
    let (mut kinds, patterns) = detect_kinds(&files);
    let kondor = KondorFile::read(dir, &files);
    let (mut artifacts, from_kondor) = collect_artifacts(dir, &files, &dirs, &patterns, kondor);
    // A nested artifact like `vendor/bundle` is kept when a directory above it is excluded.
    // One that a project above already lists, like a `__pycache__`, is not reported twice.
    artifacts.retain(|path| {
        !skip.contains(path)
            && !path
                .ancestors()
                .take_while(|ancestor| *ancestor != dir)
                .any(|ancestor| is_excluded(ancestor, excludes))
    });
    if from_kondor {
        kinds.push(KONDOR_FILE);
    }

    // Excluded artifacts are not in this list, so nested projects inside them are found.
    let mut skip: Vec<PathBuf> = skip
        .iter()
        .filter(|path| path.starts_with(dir))
        .cloned()
        .collect();
    skip.extend(artifacts.iter().cloned());

    if !artifacts.is_empty() {
        let mut artifacts_modified = SystemTime::UNIX_EPOCH;
        let artifacts = artifacts
            .into_iter()
            .map(|path| {
                let (bytes, modified) = stats(&path);
                artifacts_modified = artifacts_modified.max(modified);
                (path, bytes)
            })
            .collect();
        on_project(Project {
            path: dir.to_path_buf(),
            kinds,
            artifacts,
            artifacts_modified,
        })?;
    }

    // Hidden directories mostly hold tool data, like `.git`, `~/.cache` or installed editor
    // plugins, which look like projects but must not be cleaned. Hidden artifacts like `.next`
    // are still found above, because they are matched in the project directory itself.
    let visible = dirs
        .iter()
        .filter(|name| !name.as_encoded_bytes().starts_with(b"."));
    for name in visible {
        let child = dir.join(name);
        if !skip.contains(&child) && !is_excluded(&child, excludes) {
            scan(&child, &skip, excludes, on_project)?;
        }
    }
    ControlFlow::Continue(())
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::unix::fs::symlink;
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x").unwrap();
    }

    #[test]
    fn bun_and_deno_projects_are_not_also_node() {
        let files = |names: &[&str]| names.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(detect_kinds(&files(&["package.json"])).0, ["Node"]);
        assert_eq!(
            detect_kinds(&files(&["bun.lock", "package.json"])).0,
            ["Bun"]
        );
        assert_eq!(
            detect_kinds(&files(&["deno.json", "package.json"])).0,
            ["Deno"]
        );
    }

    #[test]
    fn makefile_counts_only_without_other_markers() {
        let files = |names: &[&str]| names.iter().map(OsString::from).collect::<Vec<_>>();
        let (kinds, patterns) = detect_kinds(&files(&["Makefile"]));
        assert_eq!(kinds, ["C/C++"]);
        assert!(patterns.dirs.contains(&"build"));

        let (kinds, patterns) = detect_kinds(&files(&["Makefile", "go.mod"]));
        assert_eq!(kinds, ["Go"]);
        assert_eq!(patterns.dirs, ["vendor"]);

        let (kinds, _) = detect_kinds(&files(&["CMakeLists.txt", "Makefile", "go.mod"]));
        assert_eq!(kinds, ["C/C++", "Go"]);
    }

    #[test]
    fn finds_nested_projects_but_not_inside_artifacts() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        touch(&root.join("Cargo.toml"));
        touch(&root.join("target/debug/bin"));
        touch(&root.join("web/package.json"));
        touch(&root.join("web/node_modules/dep/package.json"));
        touch(&root.join("web/node_modules/dep/node_modules/x/index.js"));
        touch(&root.join("web/tools/py/pyproject.toml"));
        touch(&root.join("web/tools/py/.venv/lib/a.py"));
        touch(&root.join("app/App.csproj"));
        touch(&root.join("app/obj/x"));
        touch(&root.join("rb/Gemfile"));
        touch(&root.join("rb/vendor/bundle/gems/foo/Gemfile"));
        touch(&root.join("rb/vendor/bundle/gems/foo/.bundle/x"));

        let mut found = Vec::new();
        let _ = scan(root, &[], &GlobSet::empty(), &mut |p| {
            found.push(p.path.strip_prefix(root).unwrap().to_path_buf());
            ControlFlow::Continue(())
        });

        let expected: Vec<PathBuf> = ["", "app", "rb", "web", "web/tools/py"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(found, expected);
    }

    #[test]
    fn kicad_cleans_backup_dirs_and_files() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        touch(&root.join("board.kicad_pro"));
        touch(&root.join("board.kicad_pcb"));
        touch(&root.join("board.kicad_pcb-bak"));
        touch(&root.join("board.kicad_sch-bak"));
        touch(&root.join("fp-info-cache"));
        touch(&root.join("board-backups/board-2026-01-01_120000.zip"));
        touch(&root.join("fp-info-cache-dir/x"));
        touch(&root.join("other/fp-info-cache"));

        let mut found = Vec::new();
        let _ = scan(root, &[], &GlobSet::empty(), &mut |p| {
            let artifacts = p.artifacts.into_iter().map(|(a, _)| a);
            found.push((p.path, p.kinds, artifacts.collect::<Vec<_>>()));
            ControlFlow::Continue(())
        });

        let artifacts = [
            "board-backups",
            "board.kicad_pcb-bak",
            "board.kicad_sch-bak",
            "fp-info-cache",
        ];
        let expected = vec![(
            root.to_path_buf(),
            vec!["KiCad"],
            artifacts.iter().map(|a| root.join(a)).collect::<Vec<_>>(),
        )];
        assert_eq!(found, expected);
    }

    #[test]
    fn kondor_file_adds_artifacts() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        touch(&root.join("Cargo.toml"));
        touch(&root.join("target/x"));
        touch(&root.join("out/x"));
        touch(&root.join("gen/cache/x"));
        touch(&root.join("gen/src/x"));
        touch(&root.join("tmp-a/x"));
        touch(&root.join("outside/x"));
        touch(&root.join("logs/a.log"));
        touch(&root.join("logs/b.txt"));
        touch(&root.join("pkg/one/dist/x"));
        touch(&root.join("pkg/two/dist/x"));
        symlink(root.join("pkg"), root.join("link")).unwrap();
        fs::write(
            root.join(KONDOR_FILE),
            "# comment\n\nout/\ngen/cache\ntmp-?\ntarget\n../outside\n/etc\nmissing\n\
             logs/*.log\n*/*/dist\n[\n",
        )
        .unwrap();

        let mut found = Vec::new();
        let _ = scan(root, &[], &GlobSet::empty(), &mut |p| {
            found.push((
                p.kinds,
                p.artifacts.into_iter().map(|(a, _)| a).collect::<Vec<_>>(),
            ));
            ControlFlow::Continue(())
        });

        let expected = vec![(
            vec!["Cargo", KONDOR_FILE],
            [
                "target",
                "out",
                "gen/cache",
                "tmp-a",
                "logs/a.log",
                "pkg/one/dist",
                "pkg/two/dist",
            ]
            .iter()
            .map(|a| root.join(a))
            .collect::<Vec<_>>(),
        )];
        assert_eq!(found, expected);
    }

    #[test]
    fn kondor_file_excludes_artifacts() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        touch(&root.join("meson.build"));
        touch(&root.join("build/handwritten.py"));
        touch(&root.join("build/tool/Cargo.toml"));
        touch(&root.join("build/tool/target/x"));
        touch(&root.join("builddir/x"));
        touch(&root.join("gen/a/x"));
        touch(&root.join("gen/b/x"));
        fs::write(root.join(KONDOR_FILE), "gen/*\n!build/\n! gen/b\n").unwrap();

        let mut found = Vec::new();
        let _ = scan(root, &[], &GlobSet::empty(), &mut |p| {
            let artifacts = p.artifacts.into_iter().map(|(a, _)| a);
            found.push((p.path, p.kinds, artifacts.collect::<Vec<_>>()));
            ControlFlow::Continue(())
        });

        let expected = vec![
            (
                root.to_path_buf(),
                vec!["Meson", KONDOR_FILE],
                vec![root.join("builddir"), root.join("gen/a")],
            ),
            (
                root.join("build/tool"),
                vec!["Cargo"],
                vec![root.join("build/tool/target")],
            ),
        ];
        assert_eq!(found, expected);
    }

    fn found_paths(root: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let _ = scan(root, &[], &GlobSet::empty(), &mut |p| {
            found.push(p.path.strip_prefix(root).unwrap().to_path_buf());
            ControlFlow::Continue(())
        });
        found
    }

    #[test]
    fn matches_exact_names_and_suffixes() {
        assert!(matches("target", OsStr::new("target")));
        assert!(!matches("target", OsStr::new("targets")));
        assert!(matches("*.egg-info", OsStr::new("kondor.egg-info")));
        assert!(!matches("*.egg-info", OsStr::new("egg-info.txt")));
    }

    #[test]
    fn kondor_paths_must_stay_inside_the_directory() {
        assert!(is_contained("gen/cache"));
        assert!(is_contained("logs/*.log"));
        assert!(!is_contained("../outside"));
        assert!(!is_contained("gen/../../outside"));
        assert!(!is_contained("/etc"));
        assert!(!is_contained("./gen"));
    }

    #[test]
    fn skips_hidden_directories_but_finds_hidden_artifacts() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        touch(&root.join(".config/tool/Cargo.toml"));
        touch(&root.join(".config/tool/target/x"));
        touch(&root.join(".git/modules/web/package.json"));
        touch(&root.join(".git/modules/web/node_modules/x"));
        touch(&root.join("web/package.json"));
        touch(&root.join("web/.next/x"));

        assert_eq!(found_paths(root), vec![PathBuf::from("web")]);
        // A hidden directory passed as the scan root is scanned.
        assert_eq!(
            found_paths(&root.join(".config")),
            vec![PathBuf::from("tool")]
        );
    }

    #[test]
    fn does_not_follow_symlinks() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let outside_temp = TempDir::new().unwrap();
        let outside = outside_temp.path();
        touch(&outside.join("Cargo.toml"));
        touch(&outside.join("target/x"));
        touch(&root.join("web/package.json"));
        fs::create_dir_all(outside.join("modules")).unwrap();
        symlink(outside, root.join("linked-project")).unwrap();
        symlink(outside.join("modules"), root.join("web/node_modules")).unwrap();

        let found = found_paths(root);
        // A symlinked project is not entered, and a symlinked artifact is not an artifact.
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn modified_is_the_newest_file_outside_hidden_dirs() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let set_mtime = |path: &str, secs: u64| {
            touch(&root.join(path));
            let time = old + Duration::from_secs(secs);
            File::options()
                .write(true)
                .open(root.join(path))
                .unwrap()
                .set_modified(time)
                .unwrap();
        };
        set_mtime("Cargo.toml", 0);
        set_mtime("src/main.rs", 10);
        set_mtime("target/debug/bin", 20);
        set_mtime(".git/FETCH_HEAD", 1000);

        let mut projects = Vec::new();
        let _ = scan(root, &[], &GlobSet::empty(), &mut |p| {
            projects.push(p);
            ControlFlow::Continue(())
        });
        let fresh = |project: &Project| modified(project, &mut Mtimes::new());
        assert_eq!(fresh(&projects[0]), old + Duration::from_secs(20));

        set_mtime("web/package.json", 30);
        assert_eq!(fresh(&projects[0]), old + Duration::from_secs(30));
    }

    #[test]
    fn modified_of_a_nested_project_comes_from_the_outer_walk() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        touch(&root.join("Cargo.toml"));
        touch(&root.join("target/x"));
        touch(&root.join("web/package.json"));
        touch(&root.join("web/src/index.js"));
        touch(&root.join("web/node_modules/x"));
        touch(&root.join("other/Cargo.toml"));
        touch(&root.join("other/target/x"));

        let mut projects = Vec::new();
        let _ = scan(&root.join("web"), &[], &GlobSet::empty(), &mut |p| {
            projects.push(p);
            ControlFlow::Continue(())
        });
        let _ = scan(root, &[], &GlobSet::empty(), &mut |p| {
            projects.push(p);
            ControlFlow::Continue(())
        });
        let [web, outer, other, _] = &projects[..] else {
            panic!("expected four projects");
        };
        let expected = modified(web, &mut Mtimes::new());

        let mut cache = Mtimes::new();
        modified(outer, &mut cache);
        assert!(cache.contains_key(&root.join("web")));
        // A file removed after the outer walk shows that `web` is not walked again.
        fs::remove_file(root.join("web/src/index.js")).unwrap();
        fs::remove_dir(root.join("web/src")).unwrap();
        assert_eq!(modified(web, &mut cache), expected);
        assert!(cache.contains_key(root));

        modified(other, &mut cache);
        assert!(cache.contains_key(&root.join("other")));
    }

    #[test]
    fn python_build_and_dist_need_packaging_output() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        touch(&root.join("a/setup.py"));
        touch(&root.join("a/build/lib/pkg/mod.py"));
        touch(&root.join("a/build/bdist.linux-x86_64/x"));
        touch(&root.join("a/dist/pkg-1.0-py3-none-any.whl"));
        touch(&root.join("a/dist/pkg-1.0.tar.gz"));
        touch(&root.join("b/requirements.txt"));
        touch(&root.join("b/build/deploy.sh"));
        touch(&root.join("b/dist/index.html"));
        touch(&root.join("c/pyproject.toml"));
        touch(&root.join("c/build/lib/x"));
        touch(&root.join("c/build/scripts/release.py"));
        touch(&root.join("c/dist/pkg.whl"));
        touch(&root.join("c/dist/assets/app.js"));

        let mut found = Vec::new();
        let _ = scan(root, &[], &GlobSet::empty(), &mut |p| {
            let artifacts = p.artifacts.into_iter().map(|(a, _)| a);
            found.push((p.path, artifacts.collect::<Vec<_>>()));
            ControlFlow::Continue(())
        });

        let a = root.join("a");
        assert_eq!(found, [(a.clone(), vec![a.join("build"), a.join("dist")])]);
    }

    #[test]
    fn python_collects_nested_pycache() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        touch(&root.join("pyproject.toml"));
        touch(&root.join("__pycache__/a.pyc"));
        touch(&root.join("src/pkg/__pycache__/a.pyc"));
        touch(&root.join("src/pkg/db/__pycache__/a.pyc"));
        touch(&root.join(".venv/lib/__pycache__/a.pyc"));
        touch(&root.join(".git/hooks/__pycache__/a.pyc"));
        touch(&root.join("tools/venv/pyvenv.cfg"));
        touch(&root.join("tools/venv/lib/__pycache__/a.pyc"));
        touch(&root.join("web/package.json"));
        touch(&root.join("web/node_modules/gyp/__pycache__/a.pyc"));
        touch(&root.join("sub/setup.py"));
        touch(&root.join("sub/__pycache__/a.pyc"));

        let mut found = Vec::new();
        let _ = scan(root, &[], &GlobSet::empty(), &mut |p| {
            let relative = |path: &Path| path.strip_prefix(root).unwrap().to_path_buf();
            let artifacts: Vec<PathBuf> = p.artifacts.iter().map(|(a, _)| relative(a)).collect();
            found.push((relative(&p.path), artifacts));
            ControlFlow::Continue(())
        });

        let paths = |names: &[&str]| names.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(
            found,
            [
                (
                    PathBuf::new(),
                    paths(&[
                        ".venv",
                        "__pycache__",
                        "src/pkg/__pycache__",
                        "src/pkg/db/__pycache__",
                        "sub/__pycache__",
                    ])
                ),
                (PathBuf::from("web"), paths(&["web/node_modules"])),
            ]
        );
    }

    #[test]
    fn size_counts_hard_links_once() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("node_modules");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a"), [0; 100]).unwrap();
        fs::hard_link(dir.join("a"), dir.join("b")).unwrap();
        fs::hard_link(dir.join("a"), dir.join("sub/c")).unwrap();
        fs::write(dir.join("d"), [0; 10]).unwrap();

        assert_eq!(size(&dir), 110);
    }

    #[test]
    fn size_counts_files_without_following_symlinks() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("dir/sub")).unwrap();
        fs::write(root.join("dir/a"), [0; 100]).unwrap();
        fs::write(root.join("dir/sub/b"), [0; 20]).unwrap();
        fs::write(root.join("big"), [0; 5000]).unwrap();
        symlink(root.join("big"), root.join("dir/link")).unwrap();

        let dir_size = size(&root.join("dir"));
        let link_size = size(&root.join("dir/link"));
        let file_size = size(&root.join("big"));
        let missing_size = size(&root.join("missing"));
        let link_len = root.join("big").as_os_str().len() as u64;
        assert_eq!(dir_size, 120 + link_len);
        assert_eq!(link_size, link_len);
        assert_eq!(file_size, 5000);
        assert_eq!(missing_size, 0);
    }
}
