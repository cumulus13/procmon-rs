# procmon

Process Monitor & Auto-Restart — a Rust port of the original Python tool, built so that **memory usage stays flat** during long-running monitoring.

![CI](https://github.com/cumulus13/procmon/actions/workflows/ci.yml/badge.svg)
![License](https://img.shields.io/badge/license-MIT-blue)

<p align="center">
  <img src="https://raw.githubusercontent.com/cumulus13/refs/heads/master/procmon-rs/logo.png" width="80%" alt="Powered by">
</p>


## Features

- Monitors multiple processes and automatically restarts them when they stop
- INI configuration (compatible with the Python version)
- **Growl** (GNTP) and **desktop** (native) notifications
- Terminal dashboard: uptime, restart count, memory usage, per-process status
- Cross-platform: Windows, Linux (glibc and static musl), macOS
- No garbage collector; a single small binary with no runtime

## Why Rust?

The Python version's memory usage grew steadily over time. In the Rust version:

| Problem in Python | Solution in Rust |
|---|---|
| Rich objects (Layout/Panel/Table) recreated every cycle | Each frame is rendered into a single `String` and dropped |
| `psutil.process_iter` allocates many temporary objects | One `System` instance is reused across all cycles |
| Child processes never `wait()`ed, leaving zombies | `Child::try_wait()` is called every cycle |
| Manual cache + `gc.collect()` | Not needed; memory is freed deterministically |

## Installation

### From a release

Download the archive for your platform from the [Releases](https://github.com/cumulus13/procmon/releases) page and extract it.

| Platform | Archive suffix |
|---|---|
| Linux x86_64 (glibc) | `x86_64-unknown-linux-gnu.tar.gz` |
| Linux x86_64 (static, musl / Alpine) | `x86_64-unknown-linux-musl.tar.gz` |
| Windows x86_64 | `x86_64-pc-windows-msvc.zip` |
| macOS Apple Silicon | `aarch64-apple-darwin.tar.gz` |

### From source

```bash
git clone https://github.com/cumulus13/procmon.git
cd procmon
cargo build --release
# binary: target/release/procmon
```

Static Linux binary (musl, runs on Alpine and any other distro):

```bash
rustup target add x86_64-unknown-linux-musl
sudo apt-get install -y musl-tools   # Debian/Ubuntu
cargo build --release --target x86_64-unknown-linux-musl
# binary: target/x86_64-unknown-linux-musl/release/procmon
```

Without desktop notifications (smaller binary):

```bash
cargo build --release --no-default-features
```

## Usage

```bash
# Create a template config in the user config directory
procmon --init

# Or create one at a specific path
procmon --create-config procmon.ini

# Start monitoring (config is found automatically)
procmon

# Or use an explicit config file
procmon -c procmon.ini

# Custom check interval (seconds)
procmon -c procmon.ini -i 10

# Show process status and exit
procmon -c procmon.ini -l
```

| Option | Description |
|---|---|
| `-c, --config <FILE>` | Path to the INI config (default: automatic lookup, see below) |
| `-i, --interval <SEC>` | Check interval, overrides the value in the config |
| `-l, --list` | Print status and exit |
| `--create-config <FILE>` | Write a template config to the given path |
| `--init` | Write a template config to the user config directory |

Existing files are never overwritten by `--create-config` or `--init`.

Press `Ctrl+C` to stop. Processes that were started by the monitor are **not** killed when the monitor exits.

## Config file lookup

When `-c` is not given, `procmon` searches these locations in order and uses the first file it finds, so it works when called from any directory (for example from your `PATH`):

1. `$PROCMON_CONFIG` (environment variable)
2. `./procmon.ini` (current directory)
3. `procmon.ini` next to the executable
4. User config directory:
   - Windows: `%APPDATA%\procmon\procmon.ini`
   - Linux/macOS: `$XDG_CONFIG_HOME/procmon/procmon.ini` or `~/.config/procmon/procmon.ini`

Quick start for a global install:

```bash
procmon --init     # creates the template in the user config directory
procmon            # finds it automatically from any directory
```

## Configuration

See [`examples/procmon.ini.example`](examples/procmon.ini.example).

```ini
[Settings]
check_interval = 5

[Notifications]
use_growl = no
use_plyer = yes        ; desktop notifications
growl_host = localhost
growl_port = 23053

[MyApp]
exe_path = /usr/local/bin/myapp
args = --port 8080 --name "my app"
working_dir = /usr/local
enabled = yes
```

- Every section other than `Settings` and `Notifications` is treated as a monitored process.
- `exe_path` is required; `args`, `working_dir`, and `enabled` are optional.
- Booleans accept `yes/no`, `true/false`, `on/off`, `1/0`.

## Differences from the Python version

- Growl uses a minimal GNTP implementation **without password support** (`growl_password` is ignored).
- `plyer` is replaced by `notify-rust`.
- The `--gc-interval` option was removed.
- The UI uses plain ANSI output instead of Rich.

## License

MIT © Hadi Cahyadi

## 👤 Author
        
[Hadi Cahyadi](mailto:cumulus13@gmail.com)
    

[![Buy Me a Coffee](https://www.buymeacoffee.com/assets/img/custom_images/orange_img.png)](https://www.buymeacoffee.com/cumulus13)

[![Donate via Ko-fi](https://ko-fi.com/img/githubbutton_sm.svg)](https://ko-fi.com/cumulus13)
 
[Support me on Patreon](https://www.patreon.com/cumulus13)
