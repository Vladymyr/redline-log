use alloc::{string::String, vec::Vec};
use core::{fmt, str::FromStr};
use tracing_core::{
    Level, Metadata,
    metadata::{LevelFilter, ParseLevelFilterError},
};

/// One target-specific filter rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Directive {
    target: String,
    level: LevelFilter,
}

impl Directive {
    /// Creates a directive for a target prefix and level.
    pub fn new(target: impl Into<String>, level: LevelFilter) -> Self {
        Self {
            target: target.into(),
            level,
        }
    }

    /// Returns the target prefix.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Returns the level for this directive.
    pub fn level(&self) -> LevelFilter {
        self.level
    }
}

/// Target and level filter used by `redline`.
///
/// The syntax is intentionally small. Examples:
/// - `info`
/// - `warn,app::db=trace`
/// - `error,hyper=warn,app::http=debug`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetFilter {
    default_level: LevelFilter,
    directives: Vec<Directive>,
    max_level: LevelFilter,
}

impl TargetFilter {
    /// Default level used when no directive matches.
    pub const DEFAULT_LEVEL: LevelFilter = LevelFilter::TRACE;

    /// Creates a filter from a default level and explicit directives.
    pub fn new(default_level: LevelFilter, mut directives: Vec<Directive>) -> Self {
        directives.sort_by(|left, right| right.target.len().cmp(&left.target.len()));
        let max_level = directives.iter().fold(default_level, |current, directive| {
            max_filter(current, directive.level)
        });
        Self {
            default_level,
            directives,
            max_level,
        }
    }

    /// Parses a filter spec.
    ///
    /// A bare level sets the default level. `target=level` applies to that
    /// target and its `::` children. The most specific matching target wins.
    pub fn parse(spec: &str) -> Result<Self, FilterParseError> {
        if spec.trim().is_empty() {
            return Ok(Self::new(Self::DEFAULT_LEVEL, Vec::new()));
        }

        let mut default_level = None;
        let mut directives = Vec::new();

        for raw_piece in spec.split(',') {
            let piece = raw_piece.trim();
            if piece.is_empty() {
                continue;
            }

            match piece.split_once('=') {
                Some((target, level)) => {
                    let target = target.trim();
                    if target.is_empty() {
                        return Err(FilterParseError::EmptyTarget);
                    }
                    directives.push(Directive::new(target, parse_level(level.trim())?));
                }
                None => {
                    default_level = Some(parse_level(piece)?);
                }
            }
        }

        Ok(Self::new(
            default_level.unwrap_or(Self::DEFAULT_LEVEL),
            directives,
        ))
    }

    /// Returns the configured directives, ordered by match priority.
    pub fn directives(&self) -> &[Directive] {
        &self.directives
    }

    /// Returns the default level.
    pub fn default_level(&self) -> LevelFilter {
        self.default_level
    }

    /// Returns the highest enabled level across all directives.
    pub fn max_level(&self) -> LevelFilter {
        self.max_level
    }

    /// Returns whether a metadata item is enabled.
    pub fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        let level = *metadata.level();
        let matched = self
            .directives
            .iter()
            .find(|directive| target_matches(directive.target(), metadata.target()))
            .map(Directive::level)
            .unwrap_or(self.default_level);

        level_allowed(matched, level)
    }
}

impl Default for TargetFilter {
    fn default() -> Self {
        Self::new(Self::DEFAULT_LEVEL, Vec::new())
    }
}

impl FromStr for TargetFilter {
    type Err = FilterParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Error returned when parsing a filter spec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilterParseError {
    /// A `target=level` directive used an empty target.
    EmptyTarget,
    /// A level string could not be parsed.
    InvalidLevel(String),
}

impl fmt::Display for FilterParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyTarget => f.write_str("directive target cannot be empty"),
            Self::InvalidLevel(level) => write!(f, "invalid level `{level}`"),
        }
    }
}

impl core::error::Error for FilterParseError {}

fn parse_level(value: &str) -> Result<LevelFilter, FilterParseError> {
    LevelFilter::from_str(value)
        .map_err(|_: ParseLevelFilterError| FilterParseError::InvalidLevel(value.into()))
}

fn target_matches(prefix: &str, target: &str) -> bool {
    if prefix == target {
        return true;
    }

    target
        .strip_prefix(prefix)
        .is_some_and(|suffix| suffix.starts_with("::"))
}

fn level_allowed(filter: LevelFilter, level: Level) -> bool {
    match filter.into_level() {
        Some(max_level) => level <= max_level,
        None => false,
    }
}

fn max_filter(left: LevelFilter, right: LevelFilter) -> LevelFilter {
    match (left.into_level(), right.into_level()) {
        (Some(left), Some(right)) => {
            LevelFilter::from_level(if left >= right { left } else { right })
        }
        (Some(level), None) | (None, Some(level)) => LevelFilter::from_level(level),
        (None, None) => LevelFilter::OFF,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_and_target_rules() {
        let filter = TargetFilter::parse("info,app::db=trace,hyper=warn").unwrap();
        assert_eq!(filter.default_level(), LevelFilter::INFO);
        assert_eq!(filter.directives().len(), 2);
        assert_eq!(filter.max_level(), LevelFilter::TRACE);
        assert!(target_matches("app::db", "app::db::pool"));
    }

    #[test]
    fn rejects_invalid_level() {
        let error = TargetFilter::parse("app=loud").unwrap_err();
        assert!(matches!(error, FilterParseError::InvalidLevel(_)));
    }

    #[test]
    fn prefix_match_requires_module_boundary() {
        assert!(target_matches("app", "app"));
        assert!(target_matches("app", "app::db"));
        assert!(!target_matches("app", "application"));
    }
}
