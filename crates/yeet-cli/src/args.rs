//! Minimal argument parsing: a command, positional arguments, `--flag value` options and
//! `--switch`es. Enough for a few commands without pulling in a parser crate.

use std::str::FromStr;

/// Returned when the arguments don't make sense; `main` prints the usage text.
#[derive(Debug, PartialEq, Eq)]
pub struct Usage;

#[derive(Debug, Default)]
pub struct Args {
    command: Option<String>,
    positional: Vec<String>,
    options: Vec<(String, String)>,
    switches: Vec<String>,
}

impl Args {
    /// `switches` lists the flags that take no value.
    pub fn parse(raw: impl IntoIterator<Item = String>, switches: &[&str]) -> Args {
        let mut out = Args::default();
        let mut it = raw.into_iter();
        out.command = it.next();
        while let Some(a) = it.next() {
            if !a.starts_with("--") {
                out.positional.push(a);
            } else if switches.contains(&a.as_str()) {
                out.switches.push(a);
            } else {
                let value = it.next().unwrap_or_default();
                out.options.push((a, value));
            }
        }
        out
    }

    pub fn command(&self) -> Option<&str> {
        self.command.as_deref()
    }

    pub fn positional(&self) -> &[String] {
        &self.positional
    }

    pub fn has(&self, switch: &str) -> bool {
        self.switches.iter().any(|s| s == switch)
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.options.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// The option parsed as `T`, `default` if absent, or [`Usage`] if it doesn't parse.
    pub fn parsed<T: FromStr>(&self, name: &str, default: T) -> Result<T, Usage> {
        self.get(name).map_or(Ok(default), |v| v.parse().map_err(|_| Usage))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Args {
        Args::parse(s.split_whitespace().map(String::from), &["--once"])
    }

    #[test]
    fn splits_command_positionals_options_and_switches() {
        let a = parse("send a b --to host:1 --once c --threads 4");
        assert_eq!(a.command(), Some("send"));
        assert_eq!(a.positional(), ["a", "b", "c"]);
        assert_eq!(a.get("--to"), Some("host:1"));
        assert!(a.has("--once"));
        assert_eq!(a.parsed("--threads", 1), Ok(4));
        assert_eq!(a.parsed("--port", 7878), Ok(7878));
        assert_eq!(parse("x --threads lots").parsed("--threads", 1), Err(Usage));
    }
}
