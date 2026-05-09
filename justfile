# Install all currently-installed enwiro binaries from local repo
install-dev:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Building workspace in release mode..."
    cargo build --workspace --release
    # Kill the background daemon so the binary isn't held open
    pkill -x enw && sleep 0.2 || true
    installed=$(cargo install --list | grep -E '^enwiro' | awk '{print $1}')
    for crate in $installed; do
        [ "$crate" = "enwiro-logging" ] && continue
        # The `enwiro` crate produces a binary named `enw`; all other crates
        # produce a binary matching their crate name.
        if [ "$crate" = "enwiro" ]; then
            bin_name="enw"
        else
            bin_name="$crate"
        fi
        bin="target/release/$bin_name"
        if [ ! -f "$bin" ]; then
            echo "WARNING: $bin_name binary not found, skipping"
            continue
        fi
        dest="${CARGO_HOME:-$HOME/.cargo}/bin/$bin_name"
        echo "Installing $bin_name..."
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
