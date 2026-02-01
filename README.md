# upi ⚡️

A simple, lightweight Rust daemon that monitors URLs and triggers local scripts when content changes.

## Features

- **Content Monitoring**: Fetches URLs and parses them using host-installed tools (like `jq`, `grep`, or `awk`).
- **Smart Triggers**: Only runs commands when the *parsed output* changes.
- **Persistent State**: Stores the last known result in JSON to survive restarts.
- **Concurrent Workers**: Each task runs on its own schedule using a Tokio-based event loop.

## Installation

```bash
# Build from source
cargo build --release

# Move to bin
mv target/release/upi /usr/local/bin/
```

## Configuration

Define your tasks in a `config.yml` file:

```yaml
# Optional: Global sync interval in seconds
global-update-every: 3600

tasks:
  - url: "https://api.github.com/repos/rust-lang/rust/releases/latest"
    parse: "jq -r .tag_name"
    command: "notify-send 'New Rust Release: $UPI_PARSED'"
    update-every: 60

  - url: "https://status.example.com/api/v1/health"
    parse: "jq .status"
    command: "/etc/scripts/alert-on-change.sh"
    update-every: 10
```

## Usage

```bash
# Run with config
upi --config config.yml

# Override global interval via CLI
upi --config config.yml --global-update-every 120
```

### Environment Variables

When a command is triggered, the following variable is available:
- `$UPI_PARSED`: The current output of the parse command.

## License

MIT
