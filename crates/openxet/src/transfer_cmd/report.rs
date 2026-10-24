//! The `openxet.upload.v1` and `openxet.download.v1` results, and how
//! people see them.

use openxet_client::{Uploaded, Uploads};
use serde::Serialize;

use crate::output::HumanDisplay;

/// The `openxet.upload.v1` result.
#[derive(Serialize)]
pub(super) struct UploadResult {
    schema: &'static str,
    files: Vec<UploadedFile>,
    /// Content bytes the server didn't have, sent before compression.
    bytes_uploaded: u64,
    /// Content bytes the server or the client cache already had.
    bytes_deduplicated: u64,
}

#[derive(Serialize)]
struct UploadedFile {
    path: String,
    hash: String,
    size: u64,
    sha256: String,
}

impl From<Uploaded> for UploadedFile {
    fn from(u: Uploaded) -> Self {
        Self {
            path: u.path.to_string_lossy().into_owned(),
            hash: u.hash,
            size: u.size,
            sha256: u.sha256,
        }
    }
}

impl From<Uploads> for UploadResult {
    fn from(u: Uploads) -> Self {
        Self {
            schema: "openxet.upload.v1",
            files: u.files.into_iter().map(UploadedFile::from).collect(),
            bytes_uploaded: u.bytes_uploaded,
            bytes_deduplicated: u.bytes_deduplicated,
        }
    }
}

impl HumanDisplay for UploadResult {
    fn human(&self) -> String {
        let width = self
            .files
            .iter()
            .map(|f| f.path.chars().count())
            .chain(["PATH".len()])
            .max()
            .unwrap_or(0);
        let size_width = self
            .files
            .iter()
            .map(|f| f.size.to_string().len())
            .chain(["SIZE".len()])
            .max()
            .unwrap_or(0);
        let mut lines = vec![format!(
            "{:<width$}  {:<64}  {:>size_width$}  SHA256",
            "PATH", "XET HASH", "SIZE"
        )];
        lines.extend(self.files.iter().map(|f| {
            format!(
                "{:<width$}  {}  {:>size_width$}  {}",
                f.path, f.hash, f.size, f.sha256
            )
        }));
        let total = self.bytes_uploaded + self.bytes_deduplicated;
        let percent = (self.bytes_deduplicated * 100)
            .checked_div(total)
            .unwrap_or(0);
        lines.push(format!(
            "Uploaded {} of {} ({percent}% deduplicated).",
            human_bytes(self.bytes_uploaded),
            human_bytes(total)
        ));
        lines.join("\n")
    }
}

/// The `openxet.download.v1` result.
#[derive(Serialize)]
pub(super) struct DownloadResult {
    pub schema: &'static str,
    pub hash: String,
    pub path: String,
    pub size: u64,
}

impl HumanDisplay for DownloadResult {
    fn human(&self) -> String {
        format!(
            "Downloaded {} to {} ({}).",
            self.hash,
            self.path,
            human_bytes(self.size)
        )
    }
}

/// `n` bytes in binary units, to one decimal: `512 B`, `3.0 MiB`.
#[allow(clippy::cast_precision_loss)] // display only
pub(super) fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: u64) -> Uploaded {
        Uploaded {
            path: path.into(),
            hash: "a".repeat(64),
            size,
            sha256: "b".repeat(64),
        }
    }

    #[test]
    fn the_upload_table_aligns_its_columns_and_shows_dedup() {
        let hash = "a".repeat(64);
        let table = UploadResult::from(Uploads {
            files: vec![file("a.bin", 7), file("models/large.bin", 3_145_728)],
            bytes_uploaded: 1 << 20,
            bytes_deduplicated: 3 << 20,
        })
        .human();
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 4, "{table}");
        assert!(lines[0].starts_with("PATH  "), "{table}");
        let column = |needle: &str| lines.iter().map(|l| l.find(needle)).collect::<Vec<_>>();
        let hashes = column(&hash);
        assert_eq!(hashes[1], hashes[2], "{table}");
        assert_eq!(lines[0].find("XET HASH"), hashes[1], "{table}");
        assert!(lines[1].contains("        7  "), "{table}");
        assert_eq!(lines[3], "Uploaded 1.0 MiB of 4.0 MiB (75% deduplicated).");
    }

    #[test]
    fn an_empty_upload_dedups_nothing() {
        let table = UploadResult::from(Uploads {
            files: vec![file("empty", 0)],
            bytes_uploaded: 0,
            bytes_deduplicated: 0,
        })
        .human();
        assert!(
            table.ends_with("Uploaded 0 B of 0 B (0% deduplicated)."),
            "{table}"
        );
    }

    #[test]
    fn sizes_are_shown_in_binary_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(3 << 20), "3.0 MiB");
    }
}
