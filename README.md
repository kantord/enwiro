# enwiro

Enwiro's aims to make window management as useful and convenient as possible
with the goal of making you more productive.

The core feature of Enwiro is connecting your window manager's "workspace" (or
similar) feature with separate [environments](#environment) that allow you to
work with different projects or workflows.

Enwiro is window-manager-agnostic and relies on adapters to support different
types of window managers and operating systems. Even if your window manager is
not currently supported, it should be simple enough to create an adapter for it.

At their core, environments are simple working directories, and they might be
generated using different plugins called [cookbooks](#cookbook).

Enwiro is the successor to [i3-env](https://github.com/kantord/i3-env).

## Usage

### Integration with desktop environment

`enwiro` integrates with your desktop environment using adapters such as
`enwiro-adapter-i3wm`. Adapters implement a set of basic features which `enwiro`
can use in order to connect to your operating system's graphical environment.

The adapter will provide `enwiro` with an environment name (based on your
currently active desktop workspace). You can check your adapter's README to
know how the environment name is derived.

#### Currently available adapters:

- `enwiro-adapter-i3wm` supports i3

#### Configuring desktop environment integration

`enwiro` adapters have names prefixed with `enwiro-adapter-` and can be
installed using `cargo`. For example, to install an adapter for i3, you can run

`cargo install enwiro-adapter-i3wm`.

In your configuration file, set `adapter` to your desired adapter. For example,
to use `enwiro-adapter-i3wm`, set `adapter` to `i3wm`.

```toml
adapter = "i3wm"
```

#### Updating

The most convenient way to update all enwiro-related packages at once is:

```
cargo install-update -a
```

This requires [`cargo-update`](https://crates.io/crates/cargo-update), which
you can install with `cargo install cargo-update`.

## Concepts

### Environment

<img align="right" src="environment.png" width="400" />

An environment is a local folder or a symbolic link pointing to a folder. To define
an environment, create a folder or a symbolic link inside your `workspaces_directory`
(`$HOME/.enwiro_envs` by default). The name of the folder or symlink will be used
as the environment name.

An environment serves as a working directory for your applications, such as your
terminal or your code editor. To run a command inside an environment, switch to a
desktop workspace with a name matching the name of the environment you want to use
and run  `enwiro wrap <COMMAND> [-- [COMMAND_ARGS]...]`. If no matching environment
is found but a matching recipe exists, the environment will be created automatically.
If no environment or recipe is found, it will default to using your home directory.

You can also use `enwiro activate <NAME>` to switch to (or create) a workspace for
a given environment. This is the complement to `enwiro wrap`: while `wrap` runs a
command inside an environment, `activate` selects which environment is active in
your desktop.

An environment variable `ENWIRO_ENV` containing the `enwiro` environment name
will also be added before running commands with `enwiro wrap ...`.

An environment could be linked to:

- Any branch of a Git repository checked out on your local computer
- A folder on a remote computer
- Any folder on your computer

### Recipe

<img align="right" src="recipe.png" width="400" />

Recipes are automatically generated blueprints for environments.

While they do not exist as environments on your computer yet, you can interact
with them as if they were environments and when you do so, they will be created
on the fly for you.

Recipes can have a hierarchical nature. For instance, the recipe for a Git
repository might refer to the main working tree of the Git repository, and serve
as the "parent recipe" to recipes for creating new worktrees for the same Git
repository.

### Cookbook

<img align="right" src="cookbook.png" width="400" />

Cookbooks are plugins that contain recipes. You can add more recipes to your
enwiro by installing and configuring more cookbooks.

List of currently available cookbooks:

- `enwiro-cookbook-chezmoi`: Use your chezmoi source directory as an environment
- `enwiro-cookbook-git`: Generate environments using Git repositories
- `enwiro-cookbook-github`: Discover repositories from GitHub using the GraphQL API

### Bridge

Bridges provide close integration between enwiro and other applications. A bridge
translates between enwiro and an external tool's interface, acting as glue that
connects the two.

List of currently available bridges:

- `enwiro-bridge-rofi`: Browse and activate environments from [rofi](https://github.com/davatorium/rofi)

### Background Recipe Caching

When you run `enwiro list-all` (or use it via a bridge like rofi), a background
daemon is automatically spawned to keep recipe listings cached. This avoids
blocking the UI on slow cookbook plugins (e.g., GitHub API calls).

- The daemon starts automatically on first use — no manual setup needed
- A desktop notification is shown when the daemon starts for the first time
- Recipes are refreshed every 5 minutes in the background
- The daemon exits automatically after 1 hour of inactivity
- If the cache is unavailable, `list-all` falls back to fetching recipes synchronously

Runtime files are stored in `$XDG_RUNTIME_DIR/enwiro/` (or `$XDG_CACHE_HOME/enwiro/run/`
as fallback).

### Notifications

Enwiro sends desktop notifications for important events using the system's
notification service (via [notify-rust](https://crates.io/crates/notify-rust)):

- **Environment creation**: when a new environment is cooked from a recipe
- **Errors**: when workspace activation or environment setup fails

This is especially useful when enwiro is triggered from a keybinding or bridge
where there is no terminal to show output. If the notification service is
unavailable, error messages fall back to stderr.
