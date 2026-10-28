//! Workspace names: also the branch name and the directory name, so a
//! lowercase slug that is valid as all three.

use crate::error::{CliError, ExitCode};

const MAX_LEN: usize = 64;

const ADJECTIVES: [&str; 16] = [
    "amber", "brisk", "calm", "deft", "eager", "fond", "glad", "hardy", "keen", "lucid", "merry",
    "nimble", "plucky", "quiet", "steady", "vivid",
];

const NOUNS: [&str; 16] = [
    "badger", "comet", "delta", "ember", "fjord", "gecko", "heron", "iris", "lynx", "maple",
    "otter", "pebble", "quartz", "raven", "spruce", "tern",
];

/// A fresh `adjective-noun-xxxx` name, with four random hex digits so
/// agents creating workspaces at once don't collide.
pub fn random() -> Result<String, CliError> {
    let mut bytes = [0u8; 3];
    getrandom::fill(&mut bytes).map_err(|err| {
        CliError::new(
            "random",
            format!("the operating system's random number generator failed: {err}"),
            "Pass `--name` to name the workspace yourself.",
        )
    })?;
    let adjective = ADJECTIVES[usize::from(bytes[0] >> 4)];
    let noun = NOUNS[usize::from(bytes[0] & 0x0f)];
    Ok(format!(
        "{adjective}-{noun}-{:02x}{:02x}",
        bytes[1], bytes[2]
    ))
}

/// `name` if it is a slug of lowercase ASCII letters, digits and single
/// hyphens, at most 64 bytes.
pub fn check(name: &str) -> Result<&str, CliError> {
    let valid = !name.is_empty()
        && name.len() <= MAX_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--");
    if valid {
        Ok(name)
    } else {
        Err(CliError::new(
            "invalid_name",
            format!("{name:?} is not a valid workspace name"),
            "Pass `--name` a slug of lowercase letters, digits and single hyphens, at most 64 \
             bytes, such as `fix-login`; it names the branch and the directory too.",
        )
        .with_exit(ExitCode::USAGE))
    }
}

/// `name` made a valid name, for names others chose: Claude Code passes
/// whatever the user typed after `--worktree`. Lowercased, each run of
/// other characters a hyphen, cut to 64 bytes; a random name if nothing is
/// left.
pub fn slug(name: &str) -> Result<String, CliError> {
    if check(name).is_ok() {
        return Ok(name.to_owned());
    }
    let mut slug = String::with_capacity(name.len());
    for c in name.chars().map(|c| c.to_ascii_lowercase()) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.truncate(MAX_LEN);
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        random()
    } else {
        Ok(slug.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_names_become_slugs() {
        assert_eq!(slug("bold-oak-a3f2").unwrap(), "bold-oak-a3f2");
        assert_eq!(slug("Feature/Auth").unwrap(), "feature-auth");
        assert_eq!(slug("--fix  the_bug!--").unwrap(), "fix-the-bug");
        assert_eq!(slug(&"ab-".repeat(40)).unwrap().len(), 64);
        let fallback = slug("ü/!").unwrap();
        assert_eq!(check(&fallback).unwrap(), fallback);
    }

    #[test]
    fn random_names_are_valid_names() {
        for _ in 0..32 {
            let name = random().unwrap();
            assert_eq!(check(&name).unwrap(), name);
            assert_eq!(name.matches('-').count(), 2, "{name}");
        }
    }

    #[test]
    fn only_slugs_are_names() {
        for good in ["a", "fix-login", "m2-t4", &"x".repeat(64)] {
            assert!(check(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "-a",
            "a-",
            "a--b",
            "Fix",
            "a/b",
            "a.lock",
            "a b",
            &"x".repeat(65),
        ] {
            let err = check(bad).unwrap_err();
            assert_eq!((err.code, err.exit), ("invalid_name", ExitCode::USAGE));
        }
    }
}
