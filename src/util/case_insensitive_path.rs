//! Case-insensitive file lookup in a retail install directory.
//!
//! Retail installs come from Windows, where file names ignore case; on macOS and
//! Linux hosts a name such as `RA2.ICO` or `KeyboardMD.ini` must match however the
//! install spells it.

use std::path::{Path, PathBuf};

/// The entry in `directory` whose name equals `name`, ignoring ASCII case.
/// A case-sensitive file system can hold several spellings: the first in byte
/// order wins, whatever order the directory lists them in.
pub fn find(directory: &Path, name: &str) -> Option<PathBuf> {
    crate::util::file_system::read_dir(directory)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(name)
        })
        .map(|entry| entry.path())
        .min()
}

/// The entries of `directory` in the order retail's FindFirstFileA and
/// FindNextFileA list them on NTFS: by name ignoring ASCII case, then in byte
/// order between spellings only a case-sensitive file system holds. Hosts list
/// in their own orders (APFS and ext4 by name hashes, the browser's memory
/// store by bytes), and the archive registered first wins the names it shares
/// with later ones, so every enumeration that registers files takes this one.
pub fn native_listing(directory: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut entries: Vec<PathBuf> = crate::util::file_system::read_dir(directory)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    entries.sort_by_cached_key(|path| {
        let name = path.file_name().unwrap_or_default().as_encoded_bytes();
        (name.to_ascii_uppercase(), name.to_vec())
    });
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_file_whatever_its_case() {
        let dir = std::env::temp_dir().join(format!("vera20k-case-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Ra2.Ico"), b"icon").unwrap();

        assert_eq!(find(&dir, "RA2.ICO"), Some(dir.join("Ra2.Ico")));
        assert_eq!(find(&dir, "RA2MD.ICO"), None);
        assert_eq!(find(&dir.join("missing"), "RA2.ICO"), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn several_spellings_resolve_to_the_first_in_byte_order() {
        let dir =
            std::env::temp_dir().join(format!("vera20k-case-spellings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["ra2.ico", "Ra2.Ico", "RA2.ICO"] {
            std::fs::write(dir.join(name), b"icon").unwrap();
        }
        // A case-insensitive file system holds one spelling.
        if std::fs::read_dir(&dir).unwrap().count() == 3 {
            assert_eq!(find(&dir, "ra2.ico"), Some(dir.join("RA2.ICO")));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
