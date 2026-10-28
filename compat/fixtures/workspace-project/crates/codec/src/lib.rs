//! JSON in and out for the fixture's records.

use model::Build;

#[must_use]
pub fn encode(builds: &[Build]) -> String {
    serde_json::to_string(builds).expect("a Build always serializes")
}

/// # Errors
///
/// When `text` is not a JSON list of builds.
pub fn decode(text: &str) -> serde_json::Result<Vec<Build>> {
    serde_json::from_str(text)
}

#[must_use]
pub fn bytes_label(bytes: u64) -> String {
    let mut buffer = itoa::Buffer::new();
    format!("{} B", buffer.format(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let builds = vec![Build {
            workspace: "a".into(),
            units: 3,
            bytes: 1024,
        }];
        assert_eq!(decode(&encode(&builds)).unwrap(), builds);
    }
}
