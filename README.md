# enwiro

**Everything that moves together in your head should move together on your computer** - your git branch, terminal session, IDE, and web browser, all in lockstep when you switch what you're working on.

Enwiro's task-switching is *workflow-shaped, not app-shaped*: it makes your apps work within your workflow instead of making you bend your workflow to fit how each app happens to work.

> Enwiro started out in 2019 as [i3-env](https://github.com/kantord/i3-env); generalized over time to work with multiple window managers, multiplexers, and agents you already use. Multi-platform, Rust, plugin-based, no per-project configuration required.

---

## Who is this for?

**You run several AI agents in parallel** (Claude Code, Cursor, Aider, Codex)
Never lose track of what your agents are doing: enwiro supports isolation strategies such as `git` worktrees in an agent-agnostic (and in general, application-agnostic) way. Enwiro's metadata-collection and environment-initialization features give your agents a headstart before you even start prompting them.

Enwiro's environments represent self-contained units of work, which is a helpful organization strategy when switching between several multi-agent workflows regularly, especially when mixing different agent binaries. The metadata collection system is a powerful resource for keeping track of your workflows, and it allows for the creation of tools such as `enw kanban`, which you can use to keep track of what agentic workflows you are running currently.

**You use a tiling window manager**
Most tiling window managers miss a killer feature that tmux has: the working directory. Enwiro brings this feature to tiling window managers: it brings the concept of working directories to your window manager's workspaces, not just in your terminal, but across most GUI applications. This means all applications - whether they were launched automatically by enwiro, or manually started by you - will automatically open the folder enwiro associates with your workspaces.

As a tiling window manager user you are probably used to switching between your in-progress work for two Jira tickets with a single keystroke, but enwiro also removes the need to manually open different folders in different apps. Furthermore, enwiro takes care of automating the preparation of directories, files and application windows associated with a certain workflow or task you are working on.

**You live in a terminal multiplexer such as tmux**
The choice is yours: use a tmux-like workflow where enwiro binds different sessions to your window manager's workspaces, or simply let enwiro bind your tmux sessions to enwiro's environments.
Enwiro's general philosophy is composition with other tools: it's not designed to force you to abandon your current tools, it's designed to orchestrate your existing tools to help you use them more efficiently.

**You just juggle a lot at once**
A feature, a paper, a client, an experiment, a vault — anything you switch between that has its own folder and its own context. enwiro keeps each one self-contained, so switching is one gesture instead of five.

**You are easily distracted or want to thrive in a fast-paced environment**
Having to reorganize your terminals, editors, agents, browser windows and stash your work acts as a silent multiplier to the mental and productivity cost of context switching.

Enwiro's self-contained environments let you switch with single gesture - and its metadata collection system provides you with mnemonics that make it less painful to reconstruct your previous mental workflow.

It can't remove the inherent difficulty of switching between units of work — but it can eliminate the bureaucracy around it. I originally designed enwiro - and its predecessor, i3-env - to do just that: switch to a new task and back as quickly as you can think of it.

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

<img align="right" src="environment.png" width="250" />

An environment is a local folder or a symbolic link pointing to a folder. To define
an environment, create a folder or a symbolic link inside your `workspaces_directory`
(`$HOME/.enwiro_envs` by default). The name of the folder or symlink will be used
as the environment name.

An environment serves as a working directory for your applications, such as your
terminal or your code editor. To run a command inside an environment, switch to a
desktop workspace with a name matching the name of the environment you want to use
and run  `enw wrap <COMMAND> [-- [COMMAND_ARGS]...]`. If no matching environment
is found but a matching recipe exists, the environment will be created automatically.
If no environment or recipe is found, it will default to using your home directory.

You can also use `enw activate <NAME>` to switch to (or create) a workspace for
a given environment. This is the complement to `enw wrap`: while `wrap` runs a
command inside an environment, `activate` selects which environment is active in
your desktop.

An environment variable `ENWIRO_ENV` containing the `enwiro` environment name
will also be added before running commands with `enw wrap ...`.

An environment could be linked to:

- Any branch of a Git repository checked out on your local computer
- A folder on a remote computer
- Any folder on your computer

### Recipe

<img align="right" src="recipe.png" width="250" />

Recipes are automatically generated blueprints for environments.

While they do not exist as environments on your computer yet, you can interact
with them as if they were environments and when you do so, they will be created
on the fly for you.

Recipes can have a hierarchical nature. For instance, the recipe for a Git
repository might refer to the main working tree of the Git repository, and serve
as the "parent recipe" to recipes for creating new worktrees for the same Git
repository.

### Cookbook

<img align="right" src="cookbook.png" width="250" />

Cookbooks are plugins that contain recipes. You can add more recipes to your
enwiro by installing and configuring more cookbooks.

List of currently available cookbooks:

- `enwiro-cookbook-chezmoi`: Use your chezmoi source directory as an environment
- `enwiro-cookbook-git`: Generate environments using Git repositories
- `enwiro-cookbook-github`: Discover repositories from GitHub using the GraphQL API
- `enwiro-cookbook-obsidian`: Discover Obsidian vaults and auto-open Obsidian/Zotero on activation

### Bridge

Bridges provide close integration between enwiro and other applications. A bridge
translates between enwiro and an external tool's interface, acting as glue that
connects the two.

List of currently available bridges:

- `enwiro-bridge-rofi`: Browse and activate environments from [rofi](https://github.com/davatorium/rofi)

### Daemon

`enw activate` and `enw ls` need a background daemon. Some cookbooks
fetch recipes over the network (the GitHub cookbook, for example), so enwiro
does that work in a daemon to keep the foreground commands fast. Both
commands will fail if the daemon is not running.

Start it manually:

```
enwiro-daemon
```

Or check the systemd user service:

```
systemctl --user status enwiro-daemon.service
```

- Recipes are refreshed periodically while the user is active
- SIGTERM, SIGINT, and SIGHUP all shut it down cleanly
- Runtime files live in `$XDG_RUNTIME_DIR/enwiro/` (or
  `$XDG_CACHE_HOME/enwiro/run/` as fallback)

### Notifications

Enwiro sends desktop notifications for important events using the system's
notification service (via [notify-rust](https://crates.io/crates/notify-rust)):

- **Environment creation**: when a new environment is cooked from a recipe
- **Errors**: when workspace activation or environment setup fails

This is especially useful when enwiro is triggered from a keybinding or bridge
where there is no terminal to show output. If the notification service is
unavailable, error messages fall back to stderr.
