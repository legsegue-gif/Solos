//! Where the workspace lives on the device, and moving it there.
//!
//! The workspace (`/solos/ws`) was a folder of the app's private data. The
//! app can keep it in its Documents folder instead, which the Files app shows
//! and edits in place; an earlier version's files are moved across once, at
//! start-up.

use std::path::Path;

/// What `migrate` did.
#[derive(Debug, PartialEq, Eq)]
pub struct Migrated {
    pub moved: usize,
    /// Items left in the old folder because the new one has one of that name.
    pub left: usize,
}

/// Move what is in `old` into `new`. An item whose name is already in `new`
/// stays where it was: nothing is overwritten, nothing is lost. `old` is
/// removed once empty. A missing `old`, or `old` and `new` the same folder,
/// is nothing to do.
pub fn migrate(old: &Path, new: &Path) -> std::io::Result<Migrated> {
    let none = Migrated { moved: 0, left: 0 };
    if old == new || !old.is_dir() {
        return Ok(none);
    }
    std::fs::create_dir_all(new)?;
    let mut done = Migrated { moved: 0, left: 0 };
    for entry in std::fs::read_dir(old)? {
        let entry = entry?;
        let to = new.join(entry.file_name());
        if to.symlink_metadata().is_ok() {
            done.left += 1;
        } else {
            std::fs::rename(entry.path(), &to)?;
            done.moved += 1;
        }
    }
    if done.left == 0 {
        std::fs::remove_dir(old)?;
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("solos-ws-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn what_was_there_moves_across_and_the_old_folder_goes() {
        let (old, new) = (dir("old"), dir("new"));
        std::fs::create_dir_all(old.join("skills/a")).unwrap();
        std::fs::write(old.join("skills/a/SKILL.md"), "s").unwrap();
        std::fs::write(old.join("note.txt"), "n").unwrap();
        assert_eq!(migrate(&old, &new).unwrap(), Migrated { moved: 2, left: 0 });
        assert_eq!(std::fs::read_to_string(new.join("note.txt")).unwrap(), "n");
        assert_eq!(std::fs::read_to_string(new.join("skills/a/SKILL.md")).unwrap(), "s");
        assert!(!old.exists());
    }

    #[test]
    fn a_name_already_in_the_new_folder_is_not_overwritten() {
        let (old, new) = (dir("old"), dir("new"));
        std::fs::write(old.join("a.txt"), "old a").unwrap();
        std::fs::write(old.join("b.txt"), "old b").unwrap();
        std::fs::write(new.join("a.txt"), "new a").unwrap();
        assert_eq!(migrate(&old, &new).unwrap(), Migrated { moved: 1, left: 1 });
        assert_eq!(std::fs::read_to_string(new.join("a.txt")).unwrap(), "new a");
        assert_eq!(std::fs::read_to_string(old.join("a.txt")).unwrap(), "old a", "kept, not lost");
        assert_eq!(std::fs::read_to_string(new.join("b.txt")).unwrap(), "old b");
    }

    #[test]
    fn nothing_to_move_is_not_an_error() {
        let new = dir("new");
        assert_eq!(migrate(&new.join("missing"), &new).unwrap(), Migrated { moved: 0, left: 0 });
        assert_eq!(migrate(&new, &new).unwrap(), Migrated { moved: 0, left: 0 });
    }
}
