# enwiro

Simplify your workflow with dedicated project environments for each workspace in your window manager.

Enwiro connects your window manager's workspaces with separate working directories, allowing you to work with different projects or workflows seamlessly.

## Installation

```
cargo install enwiro
```

## Usage

```
enwiro wrap <COMMAND> [-- [COMMAND_ARGS]...]   # Run a command inside an environment
enwiro show-path [ENVIRONMENT_NAME]            # Show the path of an environment
enwiro list-environments                       # List all existing environments
enwiro list-all                                # List environments and available recipes
```

## Configuration

Configuration is stored in `~/.config/enwiro/enwiro.toml` (managed by [confy](https://crates.io/crates/confy)).

```toml
workspaces_directory = "/home/user/.enwiro_envs"
adapter = "i3wm"
```

See the [repository README](https://github.com/kantord/enwiro) for full documentation.
