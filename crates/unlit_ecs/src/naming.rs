//! File-naming helpers every save path shares.
//!
//! Reading an extension and appending one when a name lacks it are pure string
//! operations, so they are compiled on every target — unlike the saves that
//! write a file, which need a filesystem and a random id.

use std::path::Path;

/// The extension `file_name` carries, unless it is empty.
pub fn extension_of(file_name: &str) -> Option<&str> {
    Path::new(file_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty())
}

/// `file_name` with `extension` appended when it carries none, or `None`
/// when there is no file name to extend.
pub fn with_extension(file_name: Option<&str>, extension: &str) -> Option<String> {
    let file_name = file_name?;
    Some(match extension_of(file_name) {
        Some(_) => file_name.to_string(),
        None => format!("{file_name}.{extension}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_extension_is_only_read_when_it_is_present() {
        assert_eq!(extension_of("a.png"), Some("png"));
        assert_eq!(extension_of("dir/a.webp"), Some("webp"));
        assert_eq!(extension_of("a"), None);
        assert_eq!(extension_of("a."), None);
    }

    #[test]
    fn a_missing_extension_is_appended() {
        assert_eq!(with_extension(None, "webp"), None);
        assert_eq!(with_extension(Some("a"), "webp").as_deref(), Some("a.webp"));
        assert_eq!(
            with_extension(Some("a.png"), "webp").as_deref(),
            Some("a.png")
        );
    }
}
