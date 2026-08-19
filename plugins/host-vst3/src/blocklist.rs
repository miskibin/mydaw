//! Which VST3 modules must never be opened again, and where that is written.
//!
//! The list itself is [`mydaw_plugin_scan::BlockList`] -- CLAP has exactly the
//! same problem and there was never a reason for two implementations of it. What
//! is VST3's own, and what lives here, is the *file name*: two formats sharing
//! one list would mean a user who deletes a wedged VST3 plugin's line also
//! forgets everything learned about their CLAP collection.
//!
//! See the shared crate's module docs for why a block list is not optional on a
//! machine with licence-protected plugins, and [`BlockList::clear`] for the way
//! back after one is licensed.

use std::path::PathBuf;

pub use mydaw_plugin_scan::{data_dir, BlockList};

/// File name under the application data directory.
pub const BLOCKLIST_FILE: &str = "vst3-blocklist.txt";

/// Where the VST3 block list is stored, or `None` when this platform will not
/// say -- in which case the list is in-memory only, which still fixes the
/// within-one-session half of the problem.
pub fn blocklist_path() -> Option<PathBuf> {
    mydaw_plugin_scan::data_file(BLOCKLIST_FILE)
}

/// The user's VST3 block list.
///
/// Never fails: a missing, unreadable or malformed file yields an empty list,
/// because a broken block list must not stop a DAW from starting.
pub fn load() -> BlockList {
    BlockList::load(BLOCKLIST_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vst3_list_has_a_name_of_its_own_under_the_users_application_data() {
        // `None` is legal (a machine that will not say where its app data lives).
        if let Some(path) = blocklist_path() {
            assert!(path.ends_with(BLOCKLIST_FILE));
            assert!(
                path.to_string_lossy().contains("MYDAW"),
                "{}",
                path.display()
            );
        }
    }

    #[test]
    fn the_loaded_list_is_the_one_the_file_name_points_at() {
        let list = load();
        assert_eq!(list.file(), blocklist_path().as_deref());
    }
}
