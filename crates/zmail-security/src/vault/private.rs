//! Directories and files only their owner can reach.
//!
//! Everything Zmail keeps on disk is created for the user alone: 0700
//! directories and 0600 files on Unix, whatever the umask. The store is
//! encrypted, but its sizes and timestamps, the lock files (anyone who can
//! open one can hold its lock) and the crash log are nobody else's
//! business. Existing ones, from before this, are tightened when they are
//! next used.
//!
//! On Windows these create with the defaults: files under the user's
//! profile inherit an access list that already admits only the user, the
//! system and administrators, and nothing here changes one.

use std::{fs, io, path::Path};

/// Creates `path` and any missing parents, each 0700 on Unix, and tightens
/// `path` itself when it already existed.
pub fn create_private_dir_all(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    // Best effort: a directory that is someone else's (a shared location
    // the user chose) still works as before.
    let _ = make_private(path);
    Ok(())
}

/// Options that create a file 0600 on Unix. The mode only applies to a
/// file the open creates; see [`make_file_private`] for one that exists.
pub fn private_file_options() -> fs::OpenOptions {
    #[cfg_attr(not(unix), expect(unused_mut))]
    let mut options = fs::OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Creates (or truncates) the file at `path` for writing, 0600 on Unix.
pub fn create_private_file(path: &Path) -> io::Result<fs::File> {
    let file = private_file_options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    let _ = make_file_private(&file);
    Ok(file)
}

/// Makes sure a file exists at `path`, creating an empty one 0600 on Unix
/// and tightening one that is there. For files another library fills in
/// (SQLite gives a database's journals the database's own mode).
pub fn ensure_private_file(path: &Path) -> io::Result<()> {
    let file = private_file_options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let _ = make_file_private(&file);
    Ok(())
}

/// Takes group and other access away from the open file. Nothing on
/// Windows.
pub fn make_file_private(file: &fs::File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = file.metadata()?;
        let mode = metadata.permissions().mode();
        if metadata.is_file() && mode & 0o077 != 0 {
            file.set_permissions(fs::Permissions::from_mode(mode & !0o077))?;
        }
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

/// Takes group and other access away from the file or directory at `path`.
/// A link is left alone (its target is not Zmail's to change), and so is a
/// path that isn't there. Nothing on Windows.
pub fn make_private(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let mode = metadata.permissions().mode();
        if !metadata.file_type().is_symlink() && mode & 0o077 != 0 {
            fs::set_permissions(path, fs::Permissions::from_mode(mode & !0o077))?;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Whether the directory at `path` lets its group or others in, and so
/// what is inside it still needs tightening. Always `false` on Windows.
pub fn is_open_to_others(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::symlink_metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o077 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "zmail-private-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("temp dir");
        directory
    }

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path)
            .expect("exists")
            .permissions()
            .mode()
            & 0o7777
    }

    #[test]
    fn new_directories_and_files_are_for_the_owner_only() {
        let root = temp_dir("new");
        let nested = root.join("a").join("b");
        create_private_dir_all(&nested).expect("created");
        assert_eq!(mode(&root.join("a")), 0o700);
        assert_eq!(mode(&nested), 0o700);

        let file = nested.join("file");
        drop(create_private_file(&file).expect("created"));
        assert_eq!(mode(&file), 0o600);
        let other = nested.join("other");
        ensure_private_file(&other).expect("created");
        assert_eq!(mode(&other), 0o600);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn existing_ones_are_tightened_and_keep_their_contents() {
        let root = temp_dir("existing");
        let directory = root.join("data");
        fs::create_dir_all(&directory).expect("created");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).expect("set");
        let file = directory.join("file");
        fs::write(&file, b"kept").expect("written");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("set");

        assert!(is_open_to_others(&directory));
        create_private_dir_all(&directory).expect("exists");
        assert_eq!(mode(&directory), 0o700);
        assert!(!is_open_to_others(&directory));
        ensure_private_file(&file).expect("exists");
        assert_eq!(mode(&file), 0o600);
        assert_eq!(fs::read(&file).expect("readable"), b"kept");

        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).expect("set");
        make_private(&file).expect("tightened");
        assert_eq!(mode(&file), 0o700, "the owner's own bits are kept");
        make_private(&directory.join("missing")).expect("a missing path is not an error");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_link_is_not_followed() {
        let root = temp_dir("link");
        let target = root.join("target");
        fs::write(&target, b"someone else's").expect("written");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).expect("set");
        let link = root.join("link");
        std::os::unix::fs::symlink(&target, &link).expect("linked");

        make_private(&link).expect("skipped");
        assert_eq!(mode(&target), 0o644);
        let _ = fs::remove_dir_all(root);
    }
}
