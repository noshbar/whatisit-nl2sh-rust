# whatisit-nl2sh Rust port

Ask for a shell command in plain English. The request is handled entirely on
your machine by `llama-cli` and the included GGUF model. The application itself
is now a native Rust binary: it does not install or import Python packages.

```console
$ whatisit find files bigger than 100MB in this folder
find . -size +100M

$ whatisit compress the logs directory into a tarball
tar -czf logs.tar.gz logs/
```

Ported by Codex from https://github.com/ThorOdinson246/whatisit-nl2sh (August 14 2026), because I prefer native executables over virtual environments.
Tested on macOS using the local model only and Works for Me [tm]!

## Build

Rust 1.85 or newer is required. There are no third-party Rust dependencies.

```bash
make release
./whatisit doctor
./whatisit list files changed in the last week
```

`make release` builds with Cargo and copies the optimized executable to
`./whatisit`, beside the local runtime and model. The program always looks for
these files beside its own executable, regardless of your current directory:

- `llama-cli`
- `nl2sh-1.5b-Q4_K_M.gguf`

Paths can also be set explicitly:

```bash
export WHATISIT_LLAMA_CLI=/path/to/llama-cli
export WHATISIT_MODEL=/path/to/nl2sh-1.5b-Q4_K_M.gguf
whatisit doctor
```

`llama-cli` is launched with GPU offload disabled, keeping the application
portable and CPU-only. Generation uses half the available CPUs, capped at four;
override that per invocation with `--threads N`.

To run it from any folder, add this repository to your `PATH`:

```bash
export PATH="/path/to/whatisit-nl2sh:$PATH"
cd /some/other/project
whatisit show the ten largest files
```

## Use

Options go before the request text:

| option | behavior |
|---|---|
| `-e`, `--execute` | run the command after interactive confirmation |
| `-n N`, `--num N` | generate up to N distinct candidates |
| `-q`, `--quiet` | print only the command on stdout |
| `-t`, `--timing` | report generation latency |
| `--threads N` | choose the CPU thread count |

Nothing is executed unless `--execute` is supplied and confirmed. Commands
identified as high risk are never auto-run, and `--quiet` refuses to emit them.
This static checker is a safety net rather than a shell sandbox, so always read
generated commands before running them.

```bash
# Use a result in another command.
cd "$(whatisit -q the directory holding the largest log file)"

# Review and optionally execute.
whatisit -e count lines in every Rust source file
```

## Development

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

## Model

The bundled filename is published as `nl2sh-1.5b-Q4_K_M.gguf` from
`ThorOdinson246/nl2sh-1.5b-Q4_K_M`. It is based on
Qwen2.5-Coder-1.5B-Instruct and quantized to Q4_K_M.

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
