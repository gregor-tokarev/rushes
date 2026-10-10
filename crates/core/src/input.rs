//! Normalisation of paths and links typed by the creator.

use std::path::{Path, PathBuf};

/// Schemes a link may use to be handed to the OS.
const ALLOWED_SCHEMES: [&str; 3] = ["http", "https", "mailto"];

/// Errors from [`expand_user_path`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathInputError {
    #[error("Enter a folder path.")]
    Empty,
    #[error("Use a full path, like ~/Videos/VideoCRM or /home/you/Videos.")]
    Relative,
    #[error("Couldn’t find your home folder to expand “~”. Type the full path instead.")]
    NoHome,
}

/// Turns a typed path into an absolute one: trims whitespace, expands a leading `~` or `~/`
/// to the home directory, and rejects empty and relative paths (they would resolve against
/// the process working directory, which the creator never sees).
pub fn expand_user_path(input: &str) -> Result<PathBuf, PathInputError> {
    expand_with_home(input, dirs::home_dir())
}

fn expand_with_home(input: &str, home: Option<PathBuf>) -> Result<PathBuf, PathInputError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(PathInputError::Empty);
    }
    let path = if input == "~" {
        home.ok_or(PathInputError::NoHome)?
    } else if let Some(rest) = input.strip_prefix("~/").or_else(|| input.strip_prefix("~\\")) {
        home.ok_or(PathInputError::NoHome)?.join(rest)
    } else {
        PathBuf::from(input)
    };
    if path.is_absolute() { Ok(path) } else { Err(PathInputError::Relative) }
}

/// Shortens a path under the home directory to `~/…` for display.
pub fn display_user_path(path: &Path) -> String {
    match dirs::home_dir() {
        Some(home) => display_with_home(path, &home),
        None => path.display().to_string(),
    }
}

fn display_with_home(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Normalises a link before it is opened: trims it, adds `https://` to a bare domain such as
/// `example.com/page`, and returns `None` for anything that is not an http(s) or mailto link
/// (e.g. `file://`, `javascript:` or an app scheme), so documents can't launch local programs.
pub fn normalize_link(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() || input.chars().any(char::is_whitespace) {
        return None;
    }
    if let Some((scheme, rest)) = input.split_once(':') {
        let is_scheme = !scheme.is_empty()
            && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        // `example.com:8080/x` or `localhost:3000`: digits after the colon mean host and port.
        let looks_like_port = rest.chars().next().is_some_and(|c| c.is_ascii_digit());
        if is_scheme && !looks_like_port {
            let scheme = scheme.to_ascii_lowercase();
            return ALLOWED_SCHEMES.contains(&scheme.as_str()).then(|| input.to_string());
        }
    }
    if input.starts_with("//") {
        return Some(format!("https:{input}"));
    }
    let host = input.split(['/', '?', '#']).next().unwrap_or_default();
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    let host = host.split(':').next().unwrap_or_default();
    let is_host = host == "localhost"
        || (host.contains('.')
            && !host.starts_with('.')
            && !host.ends_with('.')
            && host.chars().all(|c| c.is_alphanumeric() || matches!(c, '.' | '-')));
    is_host.then(|| format!("https://{input}"))
}

/// A video title as it is saved: one line, with line breaks and runs of
/// whitespace as single spaces. `None` for a title of only whitespace, which
/// is never saved: the stored title stays.
pub fn normalize_title(input: &str) -> Option<String> {
    let title = input.split_whitespace().collect::<Vec<_>>().join(" ");
    (!title.is_empty()).then_some(title)
}

/// Reads a number typed into a Number property. Commas that group thousands
/// are dropped (`1,234,567`, and `1,500` as the app shows numbers); a comma
/// that can't be grouping is the decimal point (`3,5` is 3.5, `1,25` is 1.25),
/// as is a comma after thousands grouped with dots (`1.234,5`). Anything else,
/// such as `1,2,3`, `12,34.5` or `abc`, is `None`, as are infinities and NaN.
pub fn parse_number(input: &str) -> Option<f64> {
    let input = input.trim();
    let (sign, digits) = match input.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", input.strip_prefix('+').unwrap_or(input)),
    };
    let commas = digits.matches(',').count();
    let dots = digits.matches('.').count();
    let (whole, _) = digits.split_once('.').unwrap_or((digits, ""));
    let plain = if commas == 0 {
        digits.to_string()
    } else if dots <= 1 && is_grouped(whole, ',') {
        // `1,234` or `1,234,567.89`: the commas group thousands.
        digits.replace(',', "")
    } else if commas > 1 {
        return None;
    } else if dots > 0 {
        // `1.234,5`: the dots group thousands and the comma is the decimal point.
        let (whole, fraction) = digits.split_once(',')?;
        if !is_grouped(whole, '.') {
            return None;
        }
        format!("{}.{fraction}", whole.replace('.', ""))
    } else {
        // `3,5`, `1,25`, `1234,5`: a lone comma that can't be grouping.
        digits.replacen(',', ".", 1)
    };
    let is_plain = plain.bytes().all(|byte| byte.is_ascii_digit() || byte == b'.');
    if !is_plain || !plain.bytes().any(|byte| byte.is_ascii_digit()) {
        return None;
    }
    format!("{sign}{plain}").parse::<f64>().ok().filter(|number| number.is_finite())
}

/// Whether `whole` is digits grouped in threes by `separator`, like
/// `1,234,567`: a leading group of one to three digits, not starting with a
/// zero, then at least one group of exactly three.
fn is_grouped(whole: &str, separator: char) -> bool {
    let mut groups = whole.split(separator);
    let lead = groups.next().unwrap_or_default();
    let rest: Vec<&str> = groups.collect();
    let all_digits = |group: &str| group.bytes().all(|byte| byte.is_ascii_digit());
    (1..=3).contains(&lead.len())
        && all_digits(lead)
        && !lead.starts_with('0')
        && !rest.is_empty()
        && rest.iter().all(|group| group.len() == 3 && all_digits(group))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> Option<PathBuf> {
        Some(PathBuf::from("/home/creator"))
    }

    #[test]
    fn expands_home_and_rejects_relative_paths() {
        assert_eq!(expand_with_home("~/Videos/VideoCRM", home()), Ok(PathBuf::from("/home/creator/Videos/VideoCRM")));
        assert_eq!(expand_with_home("  ~  ", home()), Ok(PathBuf::from("/home/creator")));
        assert_eq!(expand_with_home("/srv/media", home()), Ok(PathBuf::from("/srv/media")));
        assert_eq!(expand_with_home("Videos", home()), Err(PathInputError::Relative));
        assert_eq!(expand_with_home("~other/x", home()), Err(PathInputError::Relative));
        assert_eq!(expand_with_home("   ", home()), Err(PathInputError::Empty));
        assert_eq!(expand_with_home("~/x", None), Err(PathInputError::NoHome));
    }

    #[test]
    fn displays_paths_under_home_with_a_tilde() {
        let home = Path::new("/home/creator");
        assert_eq!(display_with_home(Path::new("/home/creator/Videos"), home), "~/Videos");
        assert_eq!(display_with_home(home, home), "~");
        assert_eq!(display_with_home(Path::new("/srv/media"), home), "/srv/media");
    }

    #[test]
    fn normalizes_links() {
        assert_eq!(normalize_link("https://example.com/a?b=1").as_deref(), Some("https://example.com/a?b=1"));
        assert_eq!(normalize_link(" HTTP://Example.com ").as_deref(), Some("HTTP://Example.com"));
        assert_eq!(normalize_link("mailto:me@example.com").as_deref(), Some("mailto:me@example.com"));
        assert_eq!(normalize_link("example.com/page").as_deref(), Some("https://example.com/page"));
        assert_eq!(normalize_link("www.youtube.com").as_deref(), Some("https://www.youtube.com"));
        assert_eq!(normalize_link("example.com:8080/x").as_deref(), Some("https://example.com:8080/x"));
        assert_eq!(normalize_link("localhost:3000").as_deref(), Some("https://localhost:3000"));
        assert_eq!(normalize_link("//cdn.example.com/x").as_deref(), Some("https://cdn.example.com/x"));
        assert_eq!(normalize_link("file:///etc/passwd"), None);
        assert_eq!(normalize_link("javascript:alert(1)"), None);
        assert_eq!(normalize_link("vscode://open"), None);
        assert_eq!(normalize_link("just words"), None);
        assert_eq!(normalize_link("notes"), None);
        assert_eq!(normalize_link(""), None);
    }

    #[test]
    fn titles_are_one_line_and_never_blank() {
        assert_eq!(normalize_title("  Desk   setup\ntour ").as_deref(), Some("Desk setup tour"));
        assert_eq!(normalize_title(" \n\t "), None);
        assert_eq!(normalize_title(""), None);
    }

    #[test]
    fn reads_numbers_with_grouping_or_a_decimal_comma() {
        assert_eq!(parse_number("12000"), Some(12000.));
        assert_eq!(parse_number(" -2.5 "), Some(-2.5));
        assert_eq!(parse_number("+.5"), Some(0.5));
        // Grouping commas, as the app shows numbers.
        assert_eq!(parse_number("1,500"), Some(1500.));
        assert_eq!(parse_number("1,234,567"), Some(1_234_567.));
        assert_eq!(parse_number("1,234,567.89"), Some(1_234_567.89));
        // A comma that can't be grouping is the decimal point.
        assert_eq!(parse_number("3,5"), Some(3.5));
        assert_eq!(parse_number("1,25"), Some(1.25));
        assert_eq!(parse_number("-0,75"), Some(-0.75));
        assert_eq!(parse_number("0,500"), Some(0.5));
        assert_eq!(parse_number("1234,567"), Some(1234.567));
        assert_eq!(parse_number("1.234,5"), Some(1234.5));
        assert_eq!(parse_number("1.234.567,89"), Some(1_234_567.89));
    }

    #[test]
    fn refuses_what_is_not_clearly_a_number() {
        for text in ["", "abc", "1,2,3", "12,34.5", "1.2,5", "1,234.5.6", "--5", "1 000", "nan", "inf", "1e5", "."] {
            assert_eq!(parse_number(text), None, "{text:?}");
        }
    }
}
