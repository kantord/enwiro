# Install all currently-installed enwiro binaries from local repo
install-dev:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Building workspace in release mode..."
    cargo build --workspace --release
    # Stop the daemon so the binary isn't held open. Use systemctl when the
    # unit exists so the lifecycle stays in sync with how it normally runs;
    # fall back to pkill for users running `enw daemon` directly.
    daemon_was_active=0
    if systemctl --user is-active --quiet enwiro-daemon.service 2>/dev/null; then
        daemon_was_active=1
        systemctl --user stop enwiro-daemon.service
    else
        pkill -x enw && sleep 0.2 || true
    fi
    installed=$(cargo install --list | grep -E '^enwiro' | awk '{print $1}')
    for crate in $installed; do
        [ "$crate" = "enwiro-sdk" ] && continue
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
    # Bring the daemon back up if we stopped it. Skipped silently when the
    # unit isn't present (pkill path handled above doesn't auto-restart).
    if [ "$daemon_was_active" = "1" ]; then
        echo "Restarting enwiro-daemon..."
        systemctl --user start enwiro-daemon.service
    fi

# Install all currently-installed enwiro binaries from crates.io
install-release:
    #!/usr/bin/env bash
    set -euo pipefail
    installed=$(cargo install --list | grep -E '^enwiro' | awk '{print $1}')
    for crate in $installed; do
        [ "$crate" = "enwiro-sdk" ] && continue
        echo "Installing $crate from crates.io..."
        cargo install "$crate" --force
    done
