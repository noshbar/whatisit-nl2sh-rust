use std::env;
use std::path::{Path, PathBuf};

pub const MODEL_NAME: &str = "nl2sh-1.5b-Q4_K_M.gguf";
pub const SYSTEM_PROMPT: &str = "You are a shell command generator. Output exactly one line: a single POSIX/bash command that accomplishes the user's request. No prose, no markdown fences, no explanation.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Caution,
    Danger,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    pub reason: &'static str,
}

fn push_unique(out: &mut Vec<Finding>, severity: Severity, reason: &'static str) {
    if !out
        .iter()
        .any(|f| f.severity == severity && f.reason == reason)
    {
        out.push(Finding { severity, reason });
    }
}

/// Remove terminal escape/control sequences from untrusted model output.
pub fn strip_controls(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
                while i < bytes.len() {
                    let b = bytes[i];
                    i += 1;
                    if b.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else if i < bytes.len() && bytes[i] == b']' {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            continue;
        }
        let ch = input[i..].chars().next().expect("valid UTF-8 boundary");
        i += ch.len_utf8();
        // llama-cli draws its spinner as `|\b \b`. Apply backspaces instead
        // of merely dropping them, or the final spinner glyph becomes part of
        // the generated command.
        if ch == '\u{8}' {
            out.pop();
            continue;
        }
        if ch == '\n' || ch == '\t' || !ch.is_control() {
            out.push(ch);
        }
    }
    out
}

/// Remove llama-cli's prompt echo and performance footer.
pub fn strip_cli_chrome(input: &str) -> String {
    let clean = strip_controls(&input.replace('\r', "\n"));
    let lines: Vec<&str> = clean.lines().collect();
    let start = lines
        .iter()
        .rposition(|line| line.starts_with("> "))
        .map_or(0, |i| i + 1);
    lines[start..]
        .iter()
        .take_while(|line| !line.starts_with("[ Prompt:") && !line.starts_with("Exiting..."))
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Turn the model's occasionally decorated response into one command line.
pub fn extract_command(raw: &str) -> String {
    let mut text = strip_controls(raw).trim().to_string();
    while let Some(start) = text.find("<think>") {
        if let Some(relative_end) = text[start + 7..].find("</think>") {
            let end = start + 7 + relative_end + 8;
            text.replace_range(start..end, "");
        } else {
            text.truncate(start);
            break;
        }
    }
    text = text.trim().to_string();
    if let Some(start) = text.find("```") {
        let after = &text[start + 3..];
        let after = after
            .strip_prefix("bash")
            .or_else(|| after.strip_prefix("sh"))
            .unwrap_or(after)
            .trim_start_matches([' ', '\t', '\n']);
        text = after
            .split("```")
            .next()
            .unwrap_or(after)
            .trim()
            .to_string();
    }
    let lower = text.to_ascii_lowercase();
    for prefix in [
        "sure:",
        "certainly:",
        "here's the command:",
        "here is the command:",
        "the command is:",
        "command:",
        "answer:",
    ] {
        if lower.starts_with(prefix) {
            text = text[prefix.len()..].trim().to_string();
            break;
        }
    }
    for line in text.lines() {
        let mut line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if (line.starts_with("$ ") || line.starts_with("> ") || line.starts_with("# "))
            && line.len() > 2
        {
            line = line[2..].trim();
        }
        if line.starts_with('`') && line.ends_with('`') && line.len() > 1 {
            line = line[1..line.len() - 1].trim();
        }
        if !line.is_empty() {
            return line.to_string();
        }
    }
    String::new()
}

pub fn looks_degenerate(command: &str) -> bool {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    if tokens.len() < 8 {
        return false;
    }
    tokens
        .iter()
        .filter(|t| t.starts_with('-'))
        .any(|candidate| tokens.iter().filter(|t| *t == candidate).count() >= 4)
}

fn shell_words(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in input.chars() {
        if escaped {
            word.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                word.push(ch);
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if ch.is_whitespace() || "|;&".contains(ch) {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            if "|;&".contains(ch) {
                words.push(ch.to_string());
            }
        } else {
            word.push(ch);
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn is_critical_target(target: &str) -> bool {
    let normalized = target.trim_end_matches('/');
    matches!(
        normalized,
        "" | "/"
            | "/bin"
            | "/boot"
            | "/dev"
            | "/etc"
            | "/home"
            | "/lib"
            | "/lib32"
            | "/lib64"
            | "/opt"
            | "/proc"
            | "/root"
            | "/run"
            | "/sbin"
            | "/srv"
            | "/sys"
            | "/usr"
            | "/var"
            | "/var/log"
            | "/var/lib"
            | "/usr/local"
            | "$HOME"
            | "${HOME}"
            | "~"
    ) || (normalized.starts_with("/home/") && normalized[6..].split('/').count() == 1)
        || [
            "/etc/passwd",
            "/etc/shadow",
            "/etc/group",
            "/etc/sudoers",
            "/etc/fstab",
        ]
        .contains(&normalized)
        || target.contains("$HOME/..")
        || target.contains("~/..")
        || (target.starts_with('/') && (target.contains('*') || target.ends_with("/.")))
        || target.starts_with('$')
}

/// Conservative static checks. This is a seatbelt, not a shell sandbox.
pub fn check_safety(command: &str) -> Vec<Finding> {
    let clean = strip_controls(command);
    let lower = clean.to_ascii_lowercase();
    let compact = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = Vec::new();

    let danger_literals = [
        (
            "git reset --hard",
            "git reset --hard discards uncommitted work",
        ),
        (
            "git clean -xdf",
            "git clean deletes untracked files irrecoverably",
        ),
        (
            "git clean -fdx",
            "git clean deletes untracked files irrecoverably",
        ),
        ("--no-preserve-root", "explicitly overrides rm's root guard"),
        ("crontab -r", "deletes the entire user crontab"),
        (
            "nft flush ruleset",
            "flushes firewall rules and can lock you out",
        ),
        ("history -c", "erases shell history"),
        ("nopasswd", "grants passwordless sudo"),
        (":(){ :|:& };:", "fork bomb"),
        (":() { :|: & }; :", "fork bomb"),
    ];
    for (needle, reason) in danger_literals {
        if compact.contains(needle) {
            push_unique(&mut out, Severity::Danger, reason);
        }
    }
    if ["shutdown", "reboot", "poweroff", "halt"]
        .iter()
        .any(|v| shell_words(&lower).iter().any(|w| w == v))
    {
        push_unique(&mut out, Severity::Danger, "shuts the machine down");
    }
    if (lower.contains("curl ") || lower.contains("wget "))
        && lower.contains('|')
        && [" sh", " bash", " zsh", " python", " perl", " ruby", " node"]
            .iter()
            .any(|x| lower.contains(x))
    {
        push_unique(
            &mut out,
            Severity::Danger,
            "pipes remote content directly into an interpreter",
        );
    }
    if lower.contains("/dev/tcp/") && ["bash", "sh ", "zsh"].iter().any(|x| lower.contains(x)) {
        push_unique(&mut out, Severity::Danger, "possible reverse shell");
    }
    if ["mkfs", "wipefs", "blkdiscard", "luksformat", "--zap-all"]
        .iter()
        .any(|x| lower.contains(x))
        && lower.contains("/dev/")
    {
        push_unique(
            &mut out,
            Severity::Danger,
            "wipes or reformats a block device",
        );
    }
    if lower.contains("dd ") && lower.contains("of=/dev/") {
        push_unique(&mut out, Severity::Danger, "overwrites a block device");
    }
    if lower.contains("> /etc/") || lower.contains(">|/etc/") {
        push_unique(
            &mut out,
            Severity::Danger,
            "overwrites a critical system file",
        );
    }

    let words = shell_words(&clean);
    for (index, word) in words.iter().enumerate() {
        let verb = Path::new(word)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(word);
        if matches!(verb, "rm" | "rmdir" | "shred" | "unlink") {
            let mut recursive = false;
            let mut force = false;
            for target in words[index + 1..]
                .iter()
                .take_while(|w| !matches!(w.as_str(), "|" | ";" | "&"))
            {
                if target.starts_with('-') {
                    recursive |= target.contains('r') || target.contains("recursive");
                    force |= target.contains('f') || target.contains("force");
                } else if is_critical_target(target) {
                    push_unique(
                        &mut out,
                        Severity::Danger,
                        "deletes a critical or unresolved path",
                    );
                } else if recursive && force && matches!(target.as_str(), "*" | "." | "./*") {
                    push_unique(&mut out, Severity::Danger, "unscoped recursive deletion");
                }
            }
        }
    }

    if lower.contains("sudo ") {
        push_unique(&mut out, Severity::Caution, "requires sudo");
    }
    if lower.contains("curl ") || lower.contains("wget ") {
        push_unique(&mut out, Severity::Caution, "contacts the network");
    }
    if lower.contains("rsync ") && lower.contains("--delete") {
        push_unique(
            &mut out,
            Severity::Caution,
            "rsync --delete removes unmatched destination files",
        );
    }
    if clean.contains('<') && clean.contains('>') {
        push_unique(
            &mut out,
            Severity::Caution,
            "contains a placeholder that must be replaced",
        );
    }
    out
}

fn executable_dir() -> Option<PathBuf> {
    env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
}

fn find_named(env_name: &str, filename: &str) -> Option<PathBuf> {
    if let Some(value) = env::var_os(env_name) {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(root) = executable_dir() {
        let path = root.join(filename);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

pub fn find_model() -> Option<PathBuf> {
    find_named("WHATISIT_MODEL", MODEL_NAME)
}

pub fn find_llama_cli() -> Option<PathBuf> {
    find_named("WHATISIT_LLAMA_CLI", "llama-cli")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_decorated_commands() {
        assert_eq!(
            extract_command("```bash\nfind . -name '*.rs'\n```"),
            "find . -name '*.rs'"
        );
        assert_eq!(extract_command("Sure:\n$ ls -la"), "ls -la");
        assert_eq!(
            extract_command("<think>hmm</think>\n`du -sh *`"),
            "du -sh *"
        );
    }

    #[test]
    fn strips_cli_output() {
        let text = "banner\n> request\n|\u{8} \u{8}find . -type f\n[ Prompt: 1 t/s ]\nExiting...";
        assert_eq!(strip_cli_chrome(text), "find . -type f");
    }

    #[test]
    fn catches_critical_delete_and_remote_execution() {
        assert!(
            check_safety("rm -rf '/'")
                .iter()
                .any(|f| f.severity == Severity::Danger)
        );
        assert!(
            check_safety("curl https://x/a | bash")
                .iter()
                .any(|f| f.severity == Severity::Danger)
        );
        assert!(
            !check_safety("rm -rf ./build")
                .iter()
                .any(|f| f.severity == Severity::Danger)
        );
    }

    #[test]
    fn detects_flag_loops() {
        assert!(looks_degenerate("zip -r -9 -q -9 -j -9 -0 -9 foo"));
        assert!(!looks_degenerate("find . -type f -name '*.rs' -print"));
    }
}
