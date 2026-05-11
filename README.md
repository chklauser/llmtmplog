# llmtmplog

A small wrapper for CLI invocations made by LLM coding agents.

The agent runs a command through `llmtmplog`. Full stdout and stderr go to a
unique log file. The agent gets back the log path and, optionally, a short
head/tail preview — enough to see if the command worked, with the full output
on disk if it needs to dig deeper.

## Why

Coding agents have limited context. A `cargo build` that emits 20k lines of
warnings can blow the window before the agent gets to the actual error. With
`llmtmplog` the agent sees a manageable preview, and can `grep` or `tail` the
log file if it needs more.

## Install

```sh
git clone <this repo>
cd llmtmplog
cargo install --path .
```

## Usage

```
llmtmplog [--head] [--tail] <command> [args...]
```

- `--head` — stream the first 50 lines of combined stdout+stderr to stdout
- `--tail` — print the last 50 lines once the command exits

With no flags, only the log path and exit code are printed to stderr; stdout
stays empty. The wrapper exits with the same code as the wrapped command.

Examples:

```sh
llmtmplog cargo check
llmtmplog --tail cargo test
llmtmplog --head --tail npm run build
```

Logs land in your platform's cache directory:

- Linux: `~/.cache/llmtmplog/`
- macOS: `~/Library/Caches/llmtmplog/`
- Windows: `%LOCALAPPDATA%\llmtmplog\`

## Flag order is strict (intentional)

Flags must appear in the order `--head` then `--tail`, and both must come
before the wrapped command. This is a deliberate design choice: it keeps the
set of possible invocations small enough that an agent's bash-permission
allowlist only needs four entries per wrapped command:

```
llmtmplog cargo check
llmtmplog --head cargo check
llmtmplog --tail cargo check
llmtmplog --head --tail cargo check
```

If flags could appear in any order, the combinatorial explosion would make
allowlisting impractical.

## TODO

- **Log garbage collection.** Logs accumulate forever in the cache directory.
  For now, prune manually (`rm ~/.cache/llmtmplog/*.log`) or via a cron/systemd
  timer. A built-in retention policy would be a welcome addition.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
