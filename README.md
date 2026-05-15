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
git clone https://github.com/chklauser/llmtmplog.git
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

### Example output

After `cargo clean`, running `cargo check` through `llmtmplog`:

```
$ llmtmplog --head --tail cargo check
llmtmplog: running `cargo`
llmtmplog: stdout & stderr redirected to /home/you/.cache/llmtmplog/20260511-172309-737498212-91313.log
   Compiling libc v0.2.186
    Checking option-ext v0.2.0
    Checking dirs-sys v0.5.0
    Checking dirs v6.0.0
    Checking llmtmplog v0.1.0 (/home/you/devel/llmtmplog)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.69s
   Compiling libc v0.2.186
    Checking option-ext v0.2.0
    Checking dirs-sys v0.5.0
    Checking dirs v6.0.0
    Checking llmtmplog v0.1.0 (/home/you/devel/llmtmplog)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.69s
llmtmplog: EXIT_CODE=0
```

The `llmtmplog:` lines go to stderr; everything else (the head preview, then
the tail preview) goes to stdout. The output here only had six lines, so head
and tail overlap completely — on a larger build you'd see the first 50 lines,
then the last 50.

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

## Garbage collection

On each run, `llmtmplog` kicks off a background sweep of the cache directory
and deletes its own log files older than 24 hours. The sweep only touches
files matching the wrapper's own naming pattern, errors are silently ignored,
and the wrapper never waits for the sweep to finish — if it's still running
when the wrapped command exits, it just stops.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
