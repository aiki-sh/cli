# Optimizing Rust Builds on macOS (Apple Silicon)

## Step 1: Disable XProtect for Terminal (Biggest Win)

macOS scans every new binary for malware via XProtect. This adds seconds per
build script and can make test suites 2-3x slower.

**Fix:** Add your terminal app as a Developer Tool.

```bash
open "x-apple.systempreferences:com.apple.preference.security?Privacy_DevTools"
```

Toggle on your terminal app (Terminal, iTerm, etc.). Restart the terminal.

Reference: https://nnethercote.github.io/2025/09/04/faster-rust-builds-on-mac.html

## Step 2: Use lld for Debug Builds

The default Apple linker is slower than LLVM's `lld` for incremental debug
builds. Expected improvement: 20-50% faster linking.

```bash
brew install llvm
```

Add to your project's `.cargo/config.toml`:

```toml
[target.aarch64-apple-darwin]
rustflags = ["-C", "link-arg=-fuse-ld=/opt/homebrew/opt/llvm/bin/ld64.lld"]
```

## Step 3: Enable sccache

Caches compiled crates across builds. Unchanged crates are skipped entirely on
subsequent builds.

```bash
cargo install sccache
```

Add to `~/.cargo/config.toml` (global):

```toml
[build]
rustc-wrapper = "sccache"
```

## Step 4: Disable Spotlight Indexing for Project Directories

Spotlight indexes build artifacts in `target/`, causing unnecessary I/O
contention during builds.

```bash
mdutil -i off ~/path/to/project
```

Or: System Settings > Siri & Spotlight > Spotlight Privacy > add project folders.

## Step 5: Kill Claude Code Zombie Processes

Claude Code has a known idle CPU bug where each instance pins ~100% of one CPU
core even when idle. On a MacBook Air M4 with only 4 performance cores, stale
sessions quickly starve builds.

```bash
pkill -f claude
```

Alias for convenience:

```bash
echo 'alias cc-kill="pkill -f claude"' >> ~/.zshrc
source ~/.zshrc
```

Run `cc-kill` between sessions to reclaim cores.

## Step 6: Keep Rust Up to Date

The Rust compiler receives regular performance improvements. As of Rust 1.90+,
LLD is the default linker on Linux (macOS still requires manual setup per
Step 2).

```bash
rustup update stable
```

## Expected Impact

| Optimization           | Impact                            |
|------------------------|-----------------------------------|
| XProtect fix           | Up to 2-3x faster test execution  |
| lld linker             | 20-50% faster incremental linking |
| sccache                | Skip unchanged crate compilation  |
| Spotlight off           | Less I/O contention during builds |
| Kill zombie processes  | Reclaim CPU cores for builds      |
