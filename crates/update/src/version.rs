use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

/// A VideoCRM version: `<major>.<minor>.<patch>`, optionally with a pre-release
/// suffix (`0.2.0-dev.abc1234`, a bundle built outside a release), which sorts
/// before the release it leads up to. Build metadata (`+…`) is ignored.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Option<String>,
}

impl Version {
    pub fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self { major, minor, patch, pre: None }
    }

    /// Parses `1.2.3`, `v1.2.3` (a release tag) or `1.2.3-dev.abc1234`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let text = text.split_once('+').map_or(text, |(version, _build)| version);
        let (numbers, pre) = match text.split_once('-') {
            Some((numbers, pre)) if !pre.is_empty() => (numbers, Some(pre.to_owned())),
            Some(_) => return None,
            None => (text, None),
        };
        let mut parts = numbers.split('.').map(|part| {
            // Leading zeros and signs aren't versions.
            let digits = !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
            (digits && (part == "0" || !part.starts_with('0'))).then(|| part.parse::<u64>().ok()).flatten()
        });
        let (Some(Some(major)), Some(Some(minor)), Some(Some(patch)), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        Some(Self { major, minor, patch, pre })
    }

    /// Whether this is a build between releases rather than a release.
    pub fn is_prerelease(&self) -> bool {
        self.pre.is_some()
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch)).then_with(|| {
            match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(left), Some(right)) => compare_prerelease(left, right),
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Semantic versioning's order of pre-release suffixes: dot-separated
/// identifiers, numeric ones by value and before alphanumeric ones.
fn compare_prerelease(left: &str, right: &str) -> Ordering {
    let mut left = left.split('.');
    let mut right = right.split('.');
    loop {
        let ordering = match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left), Some(right)) => match (left.parse::<u64>(), right.parse::<u64>()) {
                (Ok(left), Ok(right)) => left.cmp(&right),
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
                (Err(_), Err(_)) => left.cmp(right),
            },
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        match &self.pre {
            Some(pre) => write!(f, "-{pre}"),
            None => Ok(()),
        }
    }
}

impl FromStr for Version {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text).ok_or_else(|| format!("not a version: {text:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap_or_else(|| panic!("{text} parses"))
    }

    #[test]
    fn parses_releases_tags_and_builds_between_them() {
        assert_eq!(v("0.1.1"), Version::new(0, 1, 1));
        assert_eq!(v("v0.1.1"), Version::new(0, 1, 1));
        assert_eq!(v("1.2.3+build.7"), Version::new(1, 2, 3));
        assert_eq!(v("0.2.0-dev.abc1234").to_string(), "0.2.0-dev.abc1234");
        assert!(v("0.2.0-dev.abc1234").is_prerelease());
        assert!(!v("0.2.0").is_prerelease());
        for bad in ["", "1", "1.2", "1.2.3.4", "01.2.3", "1.x.3", "1.2.3-", "-1.2.3", "v", "1..3"] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn orders_by_number_then_release_after_its_prereleases() {
        let ordered = ["0.1.0", "0.1.1-dev.2", "0.1.1-dev.10", "0.1.1-dev.abc", "0.1.1", "0.1.2", "0.2.0", "1.0.0"];
        for pair in ordered.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
        assert_eq!(v("0.10.0").cmp(&v("0.9.0")), Ordering::Greater);
        assert_eq!(v("v1.0.0").cmp(&v("1.0.0")), Ordering::Equal);
    }
}
