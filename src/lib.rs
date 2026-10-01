//! Filters for the `ff` search tool. A [`Filters`] value is built once and shared between the
//! walker threads; [`Filters::matches`] is pure so it is tested without touching the filesystem.

use regex::{Regex, RegexBuilder};
use std::time::{Duration, SystemTime};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
}

/// What the walker learned about one entry. Built from metadata so tests can fake it.
#[derive(Clone, Debug)]
pub struct Candidate<'a> {
    pub path: &'a str,
    pub name: &'a str,
    pub kind: Kind,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub readonly: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeLimit {
    AtLeast(u64),
    AtMost(u64),
    Exactly(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Perm {
    Readonly,
    Writable,
}

#[derive(Debug, Default)]
pub struct Filters {
    pub pattern: Option<Regex>,
    pub full_path: bool,
    pub extensions: Vec<String>,
    pub kinds: Vec<Kind>,
    pub sizes: Vec<SizeLimit>,
    /// Modified no longer ago than this.
    pub newer: Option<Duration>,
    /// Modified at least this long ago.
    pub older: Option<Duration>,
    pub perm: Option<Perm>,
}

impl Filters {
    pub fn matches(&self, c: &Candidate, now: SystemTime) -> bool {
        if !self.kinds.is_empty() && !self.kinds.contains(&c.kind) {
            return false;
        }
        if let Some(re) = &self.pattern {
            let hay = if self.full_path { c.path } else { c.name };
            if !re.is_match(hay) {
                return false;
            }
        }
        if !self.extensions.is_empty() {
            let ext = c.name.rsplit_once('.').map(|(stem, e)| (stem, e.to_ascii_lowercase()));
            // A leading dot alone (".gitignore") is a name, not an extension.
            if !matches!(&ext, Some((stem, e)) if !stem.is_empty() && self.extensions.contains(e)) {
                return false;
            }
        }
        if !self.sizes.is_empty() {
            if c.kind != Kind::File {
                return false;
            }
            let ok = self.sizes.iter().all(|s| match *s {
                SizeLimit::AtLeast(n) => c.size >= n,
                SizeLimit::AtMost(n) => c.size <= n,
                SizeLimit::Exactly(n) => c.size == n,
            });
            if !ok {
                return false;
            }
        }
        if self.newer.is_some() || self.older.is_some() {
            let Some(m) = c.modified else { return false };
            // A timestamp in the future counts as age zero.
            let age = now.duration_since(m).unwrap_or(Duration::ZERO);
            if self.newer.is_some_and(|d| age > d) || self.older.is_some_and(|d| age < d) {
                return false;
            }
        }
        match self.perm {
            Some(Perm::Readonly) if !c.readonly => return false,
            Some(Perm::Writable) if c.readonly => return false,
            _ => {}
        }
        true
    }
}

/// Builds the name regex. `pattern` is a regex unless `literal` is set.
pub fn build_pattern(pattern: &str, literal: bool, case_insensitive: bool) -> Result<Regex, String> {
    let src = if literal { regex::escape(pattern) } else { pattern.to_string() };
    RegexBuilder::new(&src)
        .case_insensitive(case_insensitive)
        .build()
        .map_err(|e| format!("invalid pattern: {e}"))
}

/// Smart case: a pattern with no uppercase letters matches case-insensitively.
pub fn smart_case(pattern: &str) -> bool {
    !pattern.chars().any(|c| c.is_uppercase())
}

/// Parses `+10M` (at least), `-1k` (at most) or `512` (exactly). Units are 1024-based.
pub fn parse_size(s: &str) -> Result<SizeLimit, String> {
    let bad = || format!("invalid size '{s}', expected like +10M, -512k or 4096");
    let (mode, rest) = match s.as_bytes().first() {
        Some(b'+') => ('+', &s[1..]),
        Some(b'-') => ('-', &s[1..]),
        Some(_) => ('=', s),
        None => return Err(bad()),
    };
    let digits_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let (num, unit) = rest.split_at(digits_end);
    let n: u64 = num.parse().map_err(|_| bad())?;
    let mult: u64 = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        "t" | "tb" | "tib" => 1 << 40,
        _ => return Err(bad()),
    };
    let bytes = n.checked_mul(mult).ok_or_else(bad)?;
    Ok(match mode {
        '+' => SizeLimit::AtLeast(bytes),
        '-' => SizeLimit::AtMost(bytes),
        _ => SizeLimit::Exactly(bytes),
    })
}

/// Parses a duration like `90s`, `15m`, `2h`, `3d` or `1w`.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let bad = || format!("invalid duration '{s}', expected like 30m, 2h, 7d or 1w");
    let digits_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(digits_end);
    let n: u64 = num.parse().map_err(|_| bad())?;
    let secs: u64 = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 604_800,
        _ => return Err(bad()),
    };
    n.checked_mul(secs).map(Duration::from_secs).ok_or_else(bad)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand<'a>(path: &'a str, kind: Kind, size: u64) -> Candidate<'a> {
        let name = path.rsplit(['/', '\\']).next().unwrap();
        Candidate { path, name, kind, size, modified: None, readonly: false }
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(10_000_000)
    }

    #[test]
    fn size_parsing() {
        assert_eq!(parse_size("+10M"), Ok(SizeLimit::AtLeast(10 << 20)));
        assert_eq!(parse_size("-512k"), Ok(SizeLimit::AtMost(512 << 10)));
        assert_eq!(parse_size("4096"), Ok(SizeLimit::Exactly(4096)));
        assert_eq!(parse_size("+1GiB"), Ok(SizeLimit::AtLeast(1 << 30)));
        for bad in ["", "+", "abc", "10x", "-", "99999999999999999999", "+99999999T"] {
            assert!(parse_size(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn duration_parsing() {
        assert_eq!(parse_duration("90s"), Ok(Duration::from_secs(90)));
        assert_eq!(parse_duration("2h"), Ok(Duration::from_secs(7200)));
        assert_eq!(parse_duration("1w"), Ok(Duration::from_secs(604_800)));
        for bad in ["", "d", "5", "5y", "-1d"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn name_match_vs_full_path_match() {
        let re = build_pattern("src", false, false).unwrap();
        let mut f = Filters { pattern: Some(re), ..Default::default() };
        let c = cand("proj/src/main.rs", Kind::File, 1);
        assert!(!f.matches(&c, now()), "name is main.rs, which has no 'src'");
        f.full_path = true;
        assert!(f.matches(&c, now()));
    }

    #[test]
    fn literal_patterns_escape_regex_characters() {
        let re = build_pattern("a.c", true, false).unwrap();
        assert!(re.is_match("a.c") && !re.is_match("abc"));
        assert!(build_pattern("(", false, false).is_err());
    }

    #[test]
    fn smart_case_rules() {
        assert!(smart_case("readme"));
        assert!(!smart_case("README"));
        assert!(!smart_case("Read"));
    }

    #[test]
    fn extension_filter_ignores_dotfiles_and_case() {
        let f = Filters { extensions: vec!["rs".into()], ..Default::default() };
        assert!(f.matches(&cand("a/B.RS", Kind::File, 0), now()));
        assert!(!f.matches(&cand("a/.rs", Kind::File, 0), now()));
        assert!(!f.matches(&cand("a/rs", Kind::File, 0), now()));
        assert!(!f.matches(&cand("a/x.rs.bak", Kind::File, 0), now()));
    }

    #[test]
    fn size_filters_combine_and_skip_directories() {
        let f = Filters {
            sizes: vec![SizeLimit::AtLeast(10), SizeLimit::AtMost(20)],
            ..Default::default()
        };
        assert!(f.matches(&cand("f", Kind::File, 10), now()));
        assert!(f.matches(&cand("f", Kind::File, 20), now()));
        assert!(!f.matches(&cand("f", Kind::File, 21), now()));
        assert!(!f.matches(&cand("d", Kind::Dir, 15), now()));
    }

    #[test]
    fn time_filters() {
        let mut c = cand("f", Kind::File, 0);
        c.modified = Some(now() - Duration::from_secs(3600));
        let newer = Filters { newer: Some(Duration::from_secs(7200)), ..Default::default() };
        let older = Filters { older: Some(Duration::from_secs(7200)), ..Default::default() };
        assert!(newer.matches(&c, now()) && !older.matches(&c, now()));
        c.modified = Some(now() - Duration::from_secs(10_800));
        assert!(!newer.matches(&c, now()) && older.matches(&c, now()));
        c.modified = Some(now() + Duration::from_secs(60));
        assert!(newer.matches(&c, now()), "future timestamps count as just now");
        c.modified = None;
        assert!(!newer.matches(&c, now()) && !older.matches(&c, now()));
    }

    #[test]
    fn kind_and_permission_filters() {
        let f = Filters { kinds: vec![Kind::Dir], ..Default::default() };
        assert!(f.matches(&cand("d", Kind::Dir, 0), now()));
        assert!(!f.matches(&cand("f", Kind::File, 0), now()));
        let mut c = cand("f", Kind::File, 0);
        c.readonly = true;
        let ro = Filters { perm: Some(Perm::Readonly), ..Default::default() };
        let rw = Filters { perm: Some(Perm::Writable), ..Default::default() };
        assert!(ro.matches(&c, now()) && !rw.matches(&c, now()));
    }
}
