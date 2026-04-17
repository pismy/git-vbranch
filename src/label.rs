use regex::Regex;

use crate::error::Error;

/// Key used when the label is static (no regex capture group).
const STATIC_TARGET: &str = "default";

/// Matches PR/MR labels against either a static string or a regex with a single capture group.
///
/// - Static label (e.g. `vbranch`): matches exactly that string. Target key is `"default"`.
/// - Regex with one capture group (e.g. `vbranch:(.+)`): matches any label satisfying
///   the pattern. Target key is the captured value (e.g. `"dev"` for label `vbranch:dev`).
///
/// Regexes with more than one capture group are rejected at construction time.
#[derive(Debug)]
pub struct LabelMatcher {
    regex: Regex,
    has_capture: bool,
}

impl LabelMatcher {
    pub fn new(label: &str) -> Result<Self, Error> {
        let raw_anchored = format!("^(?:{label})$");
        let raw_regex = Regex::new(&raw_anchored).map_err(|e| {
            Error::Config(format!("invalid label pattern '{label}': {e}"))
        })?;

        let n_groups = raw_regex.captures_len() - 1;

        match n_groups {
            0 => {
                let escaped = format!("^{}$", regex::escape(label));
                let regex = Regex::new(&escaped).expect("escaped pattern must compile");
                Ok(Self {
                    regex,
                    has_capture: false,
                })
            }
            1 => Ok(Self {
                regex: raw_regex,
                has_capture: true,
            }),
            n => Err(Error::Config(format!(
                "label regex '{label}' must have at most one capture group, found {n}"
            ))),
        }
    }

    /// Returns the target key if the given label matches, `None` otherwise.
    pub fn match_target(&self, label: &str) -> Option<String> {
        let caps = self.regex.captures(label)?;
        if self.has_capture {
            caps.get(1).map(|m| m.as_str().to_string())
        } else {
            Some(STATIC_TARGET.to_string())
        }
    }

    /// Bitbucket-specific: find a `[<label>]` marker in a PR title and return its target key.
    pub fn match_target_in_title(&self, title: &str) -> Option<String> {
        let bracket_re = Regex::new(r"\[([^\]]+)\]").ok()?;
        for cap in bracket_re.captures_iter(title) {
            if let Some(inner) = cap.get(1) {
                if let Some(target) = self.match_target(inner.as_str()) {
                    return Some(target);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_label_matches_exactly() {
        let m = LabelMatcher::new("vbranch").unwrap();
        assert_eq!(m.match_target("vbranch").as_deref(), Some("default"));
        assert_eq!(m.match_target("vbranch-v2"), None);
        assert_eq!(m.match_target("other"), None);
    }

    #[test]
    fn static_label_escapes_regex_metachars() {
        let m = LabelMatcher::new("v.branch").unwrap();
        assert_eq!(m.match_target("v.branch").as_deref(), Some("default"));
        assert_eq!(m.match_target("vxbranch"), None);
    }

    #[test]
    fn regex_label_extracts_capture() {
        let m = LabelMatcher::new("vbranch:(.+)").unwrap();
        assert_eq!(m.match_target("vbranch:dev").as_deref(), Some("dev"));
        assert_eq!(m.match_target("vbranch:feature-x").as_deref(), Some("feature-x"));
        assert_eq!(m.match_target("vbranch:"), None);
        assert_eq!(m.match_target("other"), None);
    }

    #[test]
    fn multiple_capture_groups_rejected() {
        let err = LabelMatcher::new("(foo)(bar)").unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn title_marker_extracts_target() {
        let m = LabelMatcher::new("vbranch:(.+)").unwrap();
        assert_eq!(
            m.match_target_in_title("feat: add widget [vbranch:dev]").as_deref(),
            Some("dev")
        );
        assert_eq!(m.match_target_in_title("no marker here"), None);
    }
}
