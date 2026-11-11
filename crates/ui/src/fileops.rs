//! Creating and renaming files and folders. None of these overwrite anything.
//! (Deleting goes through the Recycle Bin: see `sys::dialog::recycle`.)

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};

/// Why `name` cannot be a Windows file name, if it can't.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Enter a name".into());
    }
    if let Some(c) = name
        .chars()
        .find(|c| c.is_control() || r#"\/:*?"<>|"#.contains(*c))
    {
        return Err(format!("A name cannot contain {c:?}"));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err("A name cannot end with a dot or a space".into());
    }
    // Windows reserves these with any extension: `con.txt` is the console.
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let numbered = |prefix: &str| {
        stem.strip_prefix(prefix)
            .is_some_and(|n| n.len() == 1 && n.as_bytes()[0].is_ascii_digit() && n != "0")
    };
    if ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str()) || numbered("COM") || numbered("LPT") {
        return Err(format!("{stem} is a name Windows reserves"));
    }
    Ok(())
}

/// Creates an empty file or a folder; fails if `path` already exists.
pub fn create(path: &Path, folder: bool) -> io::Result<()> {
    if folder {
        std::fs::create_dir(path)
    } else {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map(|_| ())
    }
}

/// Renames `from` to `to`; fails if another item is already there.
pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    // The file system ignores case, so `a.c` to `A.c` finds "itself" at `to`.
    let same_item = from
        .to_string_lossy()
        .eq_ignore_ascii_case(&to.to_string_lossy());
    if to.exists() && !same_item {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", to.display()),
        ));
    }
    std::fs::rename(from, to)
}

/// Where dropping `from` on the folder `dir` would put it, if that would do
/// anything and is possible: not onto its own folder, nor into itself.
pub fn move_target(from: &Path, dir: &Path) -> Option<PathBuf> {
    if from.parent() == Some(dir) || dir.starts_with(from) {
        return None;
    }
    Some(dir.join(from.file_name()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn where_a_drop_goes() {
        let (file, src) = (Path::new(r"C:\p\src\a.c"), Path::new(r"C:\p\src"));
        assert_eq!(
            move_target(file, Path::new(r"C:\p\lib")).unwrap(),
            Path::new(r"C:\p\lib\a.c")
        );
        assert_eq!(move_target(file, src), None, "already there");
        assert_eq!(move_target(src, src), None, "onto itself");
        assert_eq!(
            move_target(src, Path::new(r"C:\p\src\sub")),
            None,
            "into itself"
        );
        assert_eq!(move_target(src, Path::new(r"C:\p")), None, "already there");
        assert_eq!(
            move_target(src, Path::new(r"C:\p\src2")).unwrap(),
            Path::new(r"C:\p\src2\src")
        );
    }

    #[test]
    fn names() {
        for ok in [
            "main.cpp",
            "a",
            ".gitignore",
            "con2.txt",
            "COM0",
            "my file.h",
        ] {
            assert_eq!(validate_name(ok), Ok(()), "{ok}");
        }
        for bad in [
            "", "a/b", r"a\b", "a:b", "a*", "a?", "a\"", "<a>", "a|b", "a.", "a ", "..", "con",
            "CON.txt", "nul", "Com1", "lpt9.c", "a\tb",
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn nothing_is_overwritten() {
        let dir = std::env::temp_dir().join(format!("lattice-fileops-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.txt"), dir.join("b.txt"));

        create(&a, false).unwrap();
        std::fs::write(&a, "keep").unwrap();
        assert!(create(&a, false).is_err(), "file exists");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "keep");

        std::fs::write(&b, "other").unwrap();
        assert!(rename(&a, &b).is_err(), "target exists");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "other");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "keep");

        // Changing only the case is allowed.
        let upper = dir.join("A.txt");
        rename(&a, &upper).unwrap();
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"A.txt".to_string()), "{names:?}");

        let sub = dir.join("sub");
        create(&sub, true).unwrap();
        assert!(sub.is_dir());
        assert!(create(&sub, true).is_err(), "folder exists");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
