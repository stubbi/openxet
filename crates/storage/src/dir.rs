//! Directories only the server's user may enter.

use std::fs::DirBuilder;
use std::path::Path;

/// Creates `dir` and its missing parents, each mode `0700` on unix. The
/// umask can only narrow the mode, never widen it; existing directories
/// keep theirs.
///
/// # Errors
///
/// Any error creating a directory.
pub fn create_private_dir_all(dir: &Path) -> std::io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(dir)
}
