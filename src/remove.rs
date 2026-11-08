use std::ffi::CString;
use std::io;
use std::os::fd::OwnedFd;
use std::path::Path;

use rustix::fs::{
    AtFlags, CWD, Dir, FileType, Mode, OFlags, fchmod, fstat, openat, statat, unlinkat,
};
use rustix::path::Arg;

const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::CLOEXEC);

/// `NOFOLLOW` makes the open fail on a symlink, so a directory swapped for a symlink after the
/// scan cannot lead the removal outside the project.
fn open_child<P: Arg>(parent: &OwnedFd, name: P) -> io::Result<OwnedFd> {
    Ok(openat(
        parent,
        name,
        DIR_FLAGS | OFlags::NOFOLLOW,
        Mode::empty(),
    )?)
}

/// Some build outputs, like Go's module cache, contain read-only directories, which cannot be
/// emptied until they are writable. The change goes through the open handle, never the path.
fn make_writable(dir: &OwnedFd) -> io::Result<()> {
    let mode = Mode::from_raw_mode(fstat(dir)?.st_mode);
    if !mode.contains(Mode::RWXU) {
        fchmod(dir, mode | Mode::RWXU)?;
    }
    Ok(())
}

fn remove_tree<P: Arg + Copy>(parent: &OwnedFd, name: P, force: bool) -> io::Result<()> {
    let dir = open_child(parent, name)?;
    if force {
        make_writable(&dir)?;
    }
    let mut entries: Vec<(CString, FileType)> = Vec::new();
    for entry in Dir::read_from(&dir)? {
        let entry = entry?;
        let entry_name = entry.file_name();
        if entry_name != c"." && entry_name != c".." {
            entries.push((entry_name.to_owned(), entry.file_type()));
        }
    }
    for (entry_name, mut kind) in entries {
        if kind == FileType::Unknown {
            kind = FileType::from_raw_mode(
                statat(&dir, &entry_name, AtFlags::SYMLINK_NOFOLLOW)?.st_mode,
            );
        }
        if kind == FileType::Directory {
            remove_tree(&dir, entry_name.as_c_str(), force)?;
        } else {
            unlinkat(&dir, &entry_name, AtFlags::empty())?;
        }
    }
    Ok(unlinkat(parent, name, AtFlags::REMOVEDIR)?)
}

/// Removes `path`, which must be inside `root`. Every directory below `root` is opened without
/// following symlinks, so the removal stays inside `root` even if the tree changes after the
/// scan. With `force`, read-only directories are made writable first.
pub fn remove(root: &Path, path: &Path, force: bool) -> io::Result<()> {
    let relative = path
        .strip_prefix(root)
        .expect("artifacts are found below the scanned directory");
    let mut parts: Vec<_> = relative.components().map(|c| c.as_os_str()).collect();
    let name = parts.pop().expect("an artifact path has a name");
    let mut dir = openat(CWD, root, DIR_FLAGS, Mode::empty())?;
    for part in parts {
        dir = open_child(&dir, part)?;
    }
    let kind = FileType::from_raw_mode(statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW)?.st_mode);
    if kind == FileType::Directory {
        remove_tree(&dir, name, force)
    } else {
        Ok(unlinkat(&dir, name, AtFlags::empty())?)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn removes_read_only_directories_only_with_force() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let cache = root.join("go/pkg/mod/cache");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("x"), b"x").unwrap();
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o555)).unwrap();

        // Running as root ignores permissions, so only check the failure as a normal user.
        if fs::write(cache.join("probe"), b"").is_err() {
            assert!(remove(root, &root.join("go/pkg"), false).is_err());
            assert!(cache.join("x").exists());
        }
        remove(root, &root.join("go/pkg"), true).unwrap();
        assert!(!root.join("go/pkg").exists());
    }

    #[test]
    fn does_not_follow_symlinks() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let outside_temp = TempDir::new().unwrap();
        let outside = outside_temp.path();
        fs::create_dir_all(outside.join("target")).unwrap();
        fs::write(outside.join("kept"), b"x").unwrap();
        // A project directory swapped for a symlink after the scan.
        symlink(outside, root.join("project")).unwrap();
        // A symlink inside an artifact.
        fs::create_dir_all(root.join("web/node_modules")).unwrap();
        symlink(outside, root.join("web/node_modules/link")).unwrap();

        assert!(remove(root, &root.join("project/target"), false).is_err());
        remove(root, &root.join("web/node_modules"), false).unwrap();
        assert!(outside.join("target").exists());
        assert!(outside.join("kept").exists());
        assert!(!root.join("web/node_modules").exists());
    }

    #[test]
    fn removes_files_and_keeps_siblings() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("logs")).unwrap();
        fs::write(root.join("logs/a.log"), b"x").unwrap();
        fs::write(root.join("logs/b.txt"), b"x").unwrap();

        remove(root, &root.join("logs/a.log"), false).unwrap();
        assert!(!root.join("logs/a.log").exists());
        assert!(root.join("logs/b.txt").exists());
    }
}
