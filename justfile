# Install all currently-installed enwiro binaries from local repo
install-dev:
    #!/usr/bin/env bash
    set -euo pipefail
    installed=$(cargo install --list | grep -E '^enwiro' | awk '{print $1}')
    for crate in $installed; do
        [ "$crate" = "enwiro-logging" ] && continue
        dir="$(pwd)/$crate"
        if [ ! -d "$dir" ]; then
            echo "WARNING: $crate not found in repo, skipping"
            continue
        fi
        echo "Installing $crate from local repo..."
        cargo install --path "$dir"
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
