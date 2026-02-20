# Install all currently-installed enwiro binaries from local repo
install-dev:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Building workspace in release mode..."
    cargo build --workspace --release
    # Kill the background daemon so the binary isn't held open
    pkill -x enwiro && sleep 0.2 || true
    installed=$(cargo install --list | grep -E '^enwiro' | awk '{print $1}')
    for crate in $installed; do
        [ "$crate" = "enwiro-logging" ] && continue
        bin="target/release/$crate"
        if [ ! -f "$bin" ]; then
            echo "WARNING: $crate binary not found, skipping"
            continue
        fi
        dest="${CARGO_HOME:-$HOME/.cargo}/bin/$crate"
        echo "Installing $crate..."
        rm -f "$dest"
        cp "$bin" "$dest"
    done

# Install all currently-installed enwiro binaries from crates.io
install-release:
    #!/usr/bin/env bash
    set -euo pipefail
    installed=$(cargo install --list | grep -E '^enwiro' | awk '{print $1}')
    for crate in $installed; do
        [ "$crate" = "enwiro-logging" ] && continue
        echo "Installing $crate from crates.io..."
        cargo install "$crate" --force
    done
