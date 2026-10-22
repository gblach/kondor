use std::ffi::{OsStr, OsString};
use std::fs;
use std::ops::ControlFlow;
use std::path::{Component, Path, PathBuf};

use globset::Glob;

struct Kind {
    name: &'static str,
    markers: &'static [&'static str],
    artifacts: &'static [&'static str],
}

const JS_ARTIFACTS: &[&str] = &["node_modules", ".angular", ".next", ".nuxt", ".svelte-kit"];

/// Patterns starting with `*` match by suffix. Artifacts containing `/` are nested paths.
const KINDS: &[Kind] = &[
    Kind {
        name: "Bun",
        markers: &["bun.lock", "bun.lockb", "bunfig.toml"],
        artifacts: JS_ARTIFACTS,
    },
    Kind {
        name: "C/C++",
        markers: &["CMakeLists.txt", "Makefile"],
        artifacts: &["build", "cmake-build-debug", "cmake-build-release"],
    },
    Kind {
        name: "Cargo",
        markers: &["Cargo.toml"],
        artifacts: &["target"],
    },
    Kind {
        name: "Dart",
        markers: &["pubspec.yaml"],
        artifacts: &[".dart_tool", "build"],
    },
    Kind {
        name: "Deno",
        markers: &["deno.json", "deno.jsonc"],
        artifacts: &[
            "node_modules",
            "vendor",
            ".angular",
            ".next",
            ".nuxt",
            ".svelte-kit",
        ],
    },
    Kind {
        name: "Elixir",
        markers: &["mix.exs"],
        artifacts: &["_build"],
    },
    Kind {
        name: "Go",
        markers: &["go.mod"],
        artifacts: &["vendor"],
    },
    Kind {
        name: "Gradle",
        markers: &["build.gradle", "build.gradle.kts"],
        artifacts: &["build", ".gradle"],
    },
    Kind {
        name: "Haskell",
        markers: &["stack.yaml", "cabal.project", "*.cabal", "package.yaml"],
        artifacts: &[".stack-work", "dist-newstyle"],
    },
    Kind {
        name: "Maven",
        markers: &["pom.xml"],
        artifacts: &["target"],
    },
    Kind {
        name: "Meson",
        markers: &["meson.build"],
        artifacts: &["build", "builddir", "_build"],
    },
    Kind {
        name: ".NET",
        markers: &["*.csproj"],
        artifacts: &["bin", "obj"],
    },
    Kind {
        name: "Node",
        markers: &["package.json"],
        artifacts: JS_ARTIFACTS,
    },
    Kind {
        name: "PHP",
        markers: &["composer.json"],
        artifacts: &["vendor"],
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
        artifacts: &[
            "__pycache__",
            ".venv",
            "venv",
            ".mypy_cache",
            ".pytest_cache",
            ".ruff_cache",
            ".tox",
            ".eggs",
            "*.egg-info",
            "build",
            "dist",
        ],
    },
    Kind {
        name: "Ruby",
        markers: &["Gemfile"],
        artifacts: &[".bundle", "vendor/bundle"],
    },
    Kind {
        name: "Scala",
        markers: &["build.sbt"],
        artifacts: &["target"],
    },
    Kind {
        name: "Swift",
        markers: &["Package.swift"],
        artifacts: &[".build"],
    },
    Kind {
        name: "Zig",
        markers: &["build.zig"],
        artifacts: &["zig-cache", ".zig-cache", "zig-out"],
    },
];

const KONDOR_FILE: &str = ".kondor";

pub struct Project {
    pub path: PathBuf,
    pub kinds: Vec<&'static str>,
    pub artifacts: Vec<(PathBuf, u64)>,
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

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => dir_size(&entry.path()),
            _ => entry.metadata().map_or(0, |meta| meta.len()),
        })
        .sum()
}

/// Size of a directory's contents or of a single file; 0 when the path does not exist.
pub fn size(path: &Path) -> u64 {
    if is_real_dir(path) {
        dir_size(path)
    } else {
        fs::symlink_metadata(path).map_or(0, |meta| meta.len())
    }
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

/// Returns the file names and the sorted directory names in `dir`. Symlinks are left out.
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
    dirs.sort();
    Some((files, dirs))
}

/// Returns the names of the detected kinds and their artifact patterns.
fn detect_kinds(files: &[OsString]) -> (Vec<&'static str>, Vec<&'static str>) {
    let mut kinds = Vec::new();
    let mut patterns = Vec::new();
    for kind in KINDS {
        let found = kind
            .markers
            .iter()
            .any(|marker| files.iter().any(|file| matches(marker, file)));
        if found {
            kinds.push(kind.name);
            patterns.extend(kind.artifacts);
        }
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

/// Returns the artifact paths and whether the `.kondor` file added any of them.
fn collect_artifacts(
    dir: &Path,
    dirs: &[OsString],
    patterns: &[&str],
    kondor: KondorFile,
) -> (Vec<PathBuf>, bool) {
    let mut artifacts: Vec<PathBuf> = dirs
        .iter()
        .filter(|name| patterns.iter().any(|pattern| matches(pattern, name)))
        .map(|name| dir.join(name))
        .collect();
    for pattern in patterns.iter().filter(|pattern| pattern.contains('/')) {
        let path = dir.join(pattern);
        if !artifacts.contains(&path) && is_real_nested_dir(dir, pattern) {
            artifacts.push(path);
        }
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

/// Unlike kondo, scanning continues below a detected project so nested projects are found too.
/// Only artifact directories are skipped. `skip` carries ancestors' nested artifact paths
/// (like Ruby's `vendor/bundle`) which cannot be pruned at the ancestor's level.
/// Projects are passed to `on_project` as soon as they are found, so the caller can clean them
/// before the scan continues; `Break` stops the scan.
pub fn scan(
    dir: &Path,
    skip: &[PathBuf],
    on_project: &mut impl FnMut(Project) -> ControlFlow<()>,
) -> ControlFlow<()> {
    let Some((files, dirs)) = read_entries(dir) else {
        return ControlFlow::Continue(());
    };
    let (mut kinds, patterns) = detect_kinds(&files);
    let kondor = KondorFile::read(dir, &files);
    let (artifacts, from_kondor) = collect_artifacts(dir, &dirs, &patterns, kondor);
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
        let artifacts = artifacts
            .into_iter()
            .map(|path| {
                let bytes = size(&path);
                (path, bytes)
            })
            .collect();
        on_project(Project {
            path: dir.to_path_buf(),
            kinds,
            artifacts,
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
        if !skip.contains(&child) {
            scan(&child, &skip, on_project)?;
        }
    }
    ControlFlow::Continue(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use tempfile::TempDir;

    use super::*;

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x").unwrap();
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
        let _ = scan(root, &[], &mut |p| {
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
        let _ = scan(root, &[], &mut |p| {
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
        touch(&root.join("pyproject.toml"));
        touch(&root.join("build/handwritten.py"));
        touch(&root.join("build/tool/Cargo.toml"));
        touch(&root.join("build/tool/target/x"));
        touch(&root.join("dist/x"));
        touch(&root.join("gen/a/x"));
        touch(&root.join("gen/b/x"));
        fs::write(root.join(KONDOR_FILE), "gen/*\n!build/\n! gen/b\n").unwrap();

        let mut found = Vec::new();
        let _ = scan(root, &[], &mut |p| {
            let artifacts = p.artifacts.into_iter().map(|(a, _)| a);
            found.push((p.path, p.kinds, artifacts.collect::<Vec<_>>()));
            ControlFlow::Continue(())
        });

        let expected = vec![
            (
                root.to_path_buf(),
                vec!["Python", KONDOR_FILE],
                vec![root.join("dist"), root.join("gen/a")],
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
        let _ = scan(root, &[], &mut |p| {
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
