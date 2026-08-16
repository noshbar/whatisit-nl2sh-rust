use std::env;
use std::io::{self, IsTerminal, Write};
use std::process::{Command, Stdio};
use std::time::Instant;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use whatisit::{
    SYSTEM_PROMPT, Severity, check_safety, extract_command, find_llama_cli, find_model,
    looks_degenerate, strip_cli_chrome,
};

const HELP: &str = r#"whatisit — turn plain English into a shell command, locally

Usage:
  whatisit [OPTIONS] <request...>
  whatisit doctor

Options:
  -e, --execute       run the result after interactive confirmation
  -q, --quiet         print only the command (warnings still use stderr)
  -t, --timing        print generation time
  -n N, --num N       generate N candidates
      --threads N     llama.cpp worker threads (default: half the CPUs, max 4)
  -h, --help          show this help

Files are discovered beside the whatisit executable, regardless of the current folder.
Override them with WHATISIT_LLAMA_CLI and WHATISIT_MODEL.
"#;

#[derive(Debug)]
struct Options {
    execute: bool,
    quiet: bool,
    timing: bool,
    count: usize,
    threads: usize,
    words: Vec<String>,
}

fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(2, usize::from)
        .div_ceil(2)
        .clamp(1, 4)
}

fn parse_args(args: Vec<String>) -> Result<Options, String> {
    let mut out = Options {
        execute: false,
        quiet: false,
        timing: false,
        count: 1,
        threads: default_threads(),
        words: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        if !out.words.is_empty() {
            out.words.extend_from_slice(&args[i..]);
            break;
        }
        match args[i].as_str() {
            "-e" | "--execute" => out.execute = true,
            "-q" | "--quiet" => out.quiet = true,
            "-t" | "--timing" => out.timing = true,
            "--" => {
                out.words.extend_from_slice(&args[i + 1..]);
                break;
            }
            "-n" | "--num" | "--threads" => {
                let flag = args[i].clone();
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| format!("{flag} needs a number"))?
                    .parse::<usize>()
                    .map_err(|_| format!("{flag} needs a positive number"))?;
                if value == 0 {
                    return Err(format!("{flag} needs a positive number"));
                }
                if flag == "--threads" {
                    out.threads = value;
                } else {
                    out.count = value;
                }
            }
            value if value.starts_with("-n") && value[2..].chars().all(|c| c.is_ascii_digit()) => {
                out.count = value[2..]
                    .parse()
                    .map_err(|_| "-n needs a positive number")?;
                if out.count == 0 {
                    return Err("-n needs a positive number".into());
                }
            }
            _ => {
                out.words.extend_from_slice(&args[i..]);
                break;
            }
        }
        i += 1;
    }
    Ok(out)
}

fn generate(prompt: &str, threads: usize, sample: bool) -> Result<String, String> {
    let model = find_model().ok_or_else(|| {
        format!(
            "model not found; put {} beside this executable or set WHATISIT_MODEL",
            whatisit::MODEL_NAME
        )
    })?;
    let llama = find_llama_cli().ok_or_else(|| {
        "llama-cli not found; put it beside this executable or set WHATISIT_LLAMA_CLI".to_string()
    })?;
    let temperature = if sample { "0.6" } else { "0" };
    let mut child = Command::new(&llama);
    child
        .args([
            "-m",
            model.to_string_lossy().as_ref(),
            "-sys",
            SYSTEM_PROMPT,
            "-p",
            prompt,
            "-st",
            "--no-display-prompt",
            "--no-warmup",
            "--device",
            "none",
            "--temp",
            temperature,
            "--repeat-penalty",
            "1.08",
            "-n",
            "64",
            "-t",
            &threads.to_string(),
        ])
        .stdin(Stdio::null());

    // llama-cli opens /dev/tty directly when it has a controlling terminal,
    // bypassing Command::output's stdout/stderr pipes. A new session has no
    // controlling terminal, making llama-cli use the captured stdout path it
    // already uses in non-interactive environments.
    #[cfg(unix)]
    unsafe {
        child.pre_exec(|| {
            unsafe extern "C" {
                fn setsid() -> i32;
            }
            if setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let output = child
        .output()
        .map_err(|e| format!("could not start {}: {e}", llama.display()))?;
    if env::var_os("WHATISIT_DEBUG").is_some() {
        eprintln!("whatisit debug: llama-cli={}", llama.display());
        eprintln!("whatisit debug: model={}", model.display());
        eprintln!("whatisit debug: status={}", output.status);
        eprintln!(
            "whatisit debug: stdout={} bytes {:?}",
            output.stdout.len(),
            String::from_utf8_lossy(&output.stdout)
        );
        eprintln!(
            "whatisit debug: stderr={} bytes {:?}",
            output.stderr.len(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "llama-cli exited with {}: {}",
            output.status,
            error
                .lines()
                .rev()
                .take(8)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let command = extract_command(&strip_cli_chrome(&stdout));
    if command.is_empty() {
        return Err("the model returned no usable command; try rephrasing".into());
    }
    if looks_degenerate(&command) {
        return Err("the model produced a repetitive, incomplete command; try rephrasing".into());
    }
    Ok(command)
}

fn print_findings(command: &str) -> bool {
    let findings = check_safety(command);
    let danger = findings.iter().any(|f| f.severity == Severity::Danger);
    for finding in findings {
        let label = match finding.severity {
            Severity::Danger => "!! DANGER",
            Severity::Caution => "!  caution",
        };
        eprintln!("  {label}  {}", finding.reason);
    }
    danger
}

fn confirm(prompt: &str) -> io::Result<bool> {
    eprint!("{prompt} [y/N] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn doctor() -> i32 {
    let model = find_model();
    let llama = find_llama_cli();
    println!("whatisit doctor");
    println!(
        "  {} model      {}",
        if model.is_some() { "ok  " } else { "FAIL" },
        model
            .as_ref()
            .map_or_else(|| "not found".into(), |p| p.display().to_string())
    );
    println!(
        "  {} llama-cli  {}",
        if llama.is_some() { "ok  " } else { "FAIL" },
        llama
            .as_ref()
            .map_or_else(|| "not found".into(), |p| p.display().to_string())
    );
    if model.is_some() && llama.is_some() {
        0
    } else {
        1
    }
}

fn run() -> i32 {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || matches!(args.first().map(String::as_str), Some("-h" | "--help")) {
        print!("{HELP}");
        return 0;
    }
    if args.first().is_some_and(|a| a == "doctor") {
        return doctor();
    }
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("whatisit: {error}");
            return 2;
        }
    };
    if options.words.is_empty() {
        eprintln!("whatisit: give me a request in plain English");
        return 2;
    }
    let prompt = options.words.join(" ");
    let started = Instant::now();
    let mut commands = Vec::new();
    for index in 0..options.count {
        match generate(&prompt, options.threads, index > 0) {
            Ok(command) if !commands.contains(&command) => commands.push(command),
            Ok(_) => {}
            Err(error) if commands.is_empty() => {
                eprintln!("whatisit: {error}");
                return 4;
            }
            Err(_) => break,
        }
    }
    if commands.is_empty() {
        eprintln!("whatisit: the model returned no distinct command");
        return 5;
    }

    if options.quiet {
        if print_findings(&commands[0]) {
            eprintln!("whatisit: refusing to emit a command flagged DANGER");
            return 6;
        }
        println!("{}", commands[0]);
        return 0;
    }
    let mut any_danger = false;
    for (index, command) in commands.iter().enumerate() {
        if commands.len() > 1 {
            println!("{}. {command}", index + 1);
        } else {
            println!("{command}");
        }
        io::stdout().flush().ok();
        any_danger |= print_findings(command);
    }
    if options.timing {
        eprintln!("  [{:.2}s, one-shot mode]", started.elapsed().as_secs_f64());
    }
    if !options.execute {
        return 0;
    }
    if commands.len() > 1 {
        eprintln!("whatisit: refusing to guess which candidate to execute; rerun without -n");
        return 6;
    }
    if any_danger {
        eprintln!("whatisit: refusing to auto-run a command flagged DANGER");
        return 6;
    }
    if !io::stdin().is_terminal() {
        eprintln!("whatisit: refusing to execute without an interactive confirmation");
        return 6;
    }
    if !confirm("Run this?").unwrap_or(false) {
        eprintln!("whatisit: not running.");
        return 0;
    }
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    eprintln!("$ {}", commands[0]);
    Command::new(shell)
        .args(["-c", &commands[0]])
        .status()
        .map_or_else(
            |error| {
                eprintln!("whatisit: could not run shell: {error}");
                7
            },
            |status| status.code().unwrap_or(1),
        )
}

fn main() {
    std::process::exit(run());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_only_count_before_request() {
        let args = parse_args(vec![
            "-q".into(),
            "list".into(),
            "files".into(),
            "-e".into(),
        ])
        .unwrap();
        assert!(args.quiet);
        assert!(!args.execute);
        assert_eq!(args.words, ["list", "files", "-e"]);
    }
}
