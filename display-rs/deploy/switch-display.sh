#!/bin/sh
# Switch the hifi-display service between the Rust and the Python display.
#   ~/spectrum-analyzer-rs/switch-display.sh rust
#   ~/spectrum-analyzer-rs/switch-display.sh python
set -e
dir=/etc/systemd/system/hifi-display.service.d
case "$1" in
  rust)
    sudo mkdir -p "$dir"
    sudo cp "$(dirname "$0")/rust.conf" "$dir/rust.conf"
    ;;
  python)
    sudo rm -f "$dir/rust.conf"
    ;;
  *)
    echo "usage: $0 rust|python" >&2
    exit 2
    ;;
esac
sudo systemctl daemon-reload
sudo systemctl restart hifi-display
systemctl --no-pager status hifi-display | head -3
