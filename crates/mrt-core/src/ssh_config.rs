//! Hosts from the OpenSSH client configuration (`~/.ssh/config`), for the Command
//! Palette: every `Host` name without wildcards, with the `HostName`, `User` and
//! `Port` set for it. `Include` lines are followed (relative paths are under
//! `~/.ssh`, `*` and `?` in file names match), and `Match` blocks are skipped.

use std::path::{Path, PathBuf};

/// One host alias from the SSH config.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SshHost {
    /// The name after `Host`, what `ssh <alias>` takes.
    pub alias: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<String>,
}

impl SshHost {
    /// `user@hostname:port` with the parts that are set (the alias when no
    /// `HostName` is given).
    pub fn target(&self) -> String {
        let mut out = String::new();
        if let Some(user) = &self.user {
            out.push_str(user);
            out.push('@');
        }
        out.push_str(self.hostname.as_deref().unwrap_or(&self.alias));
        if let Some(port) = self.port.as_deref().filter(|p| *p != "22") {
            out.push(':');
            out.push_str(port);
        }
        out
    }
}

/// How deep `Include` may nest (OpenSSH allows 16).
const MAX_INCLUDE_DEPTH: usize = 8;

/// The hosts in `~/.ssh/config` (empty when there is none).
pub fn load_ssh_hosts() -> Vec<SshHost> {
    let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) else { return Vec::new() };
    let ssh_dir = home.join(".ssh");
    let mut hosts = Vec::new();
    read_config(&ssh_dir.join("config"), &ssh_dir, &home, 0, &mut hosts);
    hosts
}

fn read_config(path: &Path, ssh_dir: &Path, home: &Path, depth: usize, hosts: &mut Vec<SshHost>) {
    if depth > MAX_INCLUDE_DEPTH {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else { return };
    parse_into(&text, ssh_dir, home, depth, hosts);
}

/// Parses config `text` into `hosts` (aliases already there are kept: as in
/// OpenSSH, the first value found wins).
pub fn parse_ssh_config(text: &str, ssh_dir: &Path, home: &Path) -> Vec<SshHost> {
    let mut hosts = Vec::new();
    parse_into(text, ssh_dir, home, 0, &mut hosts);
    hosts
}

fn parse_into(text: &str, ssh_dir: &Path, home: &Path, depth: usize, hosts: &mut Vec<SshHost>) {
    // Indices into `hosts` of the aliases the current `Host` block applies to.
    let mut current: Vec<usize> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (keyword, rest) = split_keyword(line);
        let args = split_args(rest);
        match keyword.to_ascii_lowercase().as_str() {
            "host" => {
                current.clear();
                for pattern in &args {
                    if pattern.contains(['*', '?', '!']) {
                        continue;
                    }
                    let index = match hosts.iter().position(|h| h.alias == *pattern) {
                        Some(index) => index,
                        None => {
                            hosts.push(SshHost { alias: pattern.clone(), ..Default::default() });
                            hosts.len() - 1
                        }
                    };
                    current.push(index);
                }
            }
            "match" => current.clear(),
            "include" => {
                for pattern in &args {
                    for file in expand_include(pattern, ssh_dir, home) {
                        read_config(&file, ssh_dir, home, depth + 1, hosts);
                    }
                }
            }
            key @ ("hostname" | "user" | "port") => {
                let Some(value) = args.first() else { continue };
                for &index in &current {
                    let host = &mut hosts[index];
                    let slot = match key {
                        "hostname" => &mut host.hostname,
                        "user" => &mut host.user,
                        _ => &mut host.port,
                    };
                    slot.get_or_insert_with(|| value.clone());
                }
            }
            _ => {}
        }
    }
}

/// `Keyword value` or `Keyword=value`.
fn split_keyword(line: &str) -> (&str, &str) {
    let end = line.find(|c: char| c.is_whitespace() || c == '=').unwrap_or(line.len());
    let rest = line[end..].trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest).trim_start();
    (&line[..end], rest)
}

/// Arguments separated by whitespace; double quotes group.
fn split_args(rest: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in rest.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

/// The files an `Include` pattern names: `~` is the home folder, relative paths
/// are under `~/.ssh`, and `*` / `?` in the last part match file names.
fn expand_include(pattern: &str, ssh_dir: &Path, home: &Path) -> Vec<PathBuf> {
    let path = if let Some(rest) = pattern.strip_prefix("~/") {
        home.join(rest)
    } else if Path::new(pattern).is_absolute() {
        PathBuf::from(pattern)
    } else {
        ssh_dir.join(pattern)
    };
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else { return Vec::new() };
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let Some(dir) = path.parent() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file() && wildcard_match(&name, &e.file_name().to_string_lossy()))
        .map(|e| e.path())
        .collect();
    files.sort();
    files
}

/// `*` (any run of characters) and `?` (one character) matching.
fn wildcard_match(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], t) || (!t.is_empty() && go(p, &t[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &t[1..]),
            _ => false,
        }
    }
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    go(&p, &t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_with_their_settings_and_includes() {
        let dir = std::env::temp_dir().join(format!("mrt-ssh-{}", uuid::Uuid::new_v4()));
        let ssh = dir.join(".ssh");
        std::fs::create_dir_all(ssh.join("config.d")).unwrap();
        std::fs::write(ssh.join("config.d").join("work.conf"), "Host build\n  HostName build.internal\n  User ci\n").unwrap();
        std::fs::write(ssh.join("config.d").join("notes.txt"), "Host not-included\n").unwrap();
        let text = "\
# personal
Host box pi
    HostName 192.168.1.20
    User me
Host box
    Port 2222
    User someone-else
Host *.example.com !secret
    User wild
Host=web
    Port=22
Include config.d/*.conf
Match host foo
    User matched
Host \"quoted\"
";
        let hosts = parse_ssh_config(text, &ssh, &dir);
        let names: Vec<&str> = hosts.iter().map(|h| h.alias.as_str()).collect();
        assert_eq!(names, ["box", "pi", "web", "build", "quoted"]);
        assert_eq!(hosts[0].target(), "me@192.168.1.20:2222", "the first User wins");
        assert_eq!(hosts[1].target(), "me@192.168.1.20");
        assert_eq!(hosts[2].target(), "web", "port 22 is not shown");
        assert_eq!(hosts[3].target(), "ci@build.internal");
        assert_eq!(hosts[4].user, None, "Match blocks are skipped");
        assert!(wildcard_match("*.conf", "work.conf"));
        assert!(!wildcard_match("*.conf", "notes.txt"));
        assert!(wildcard_match("h?st", "host"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
