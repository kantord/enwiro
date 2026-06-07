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

## Installation

Install enwiro and an adapter for your window manager or multiplexer with `cargo`:

```sh
cargo install enwiro enwiro-daemon
cargo install enwiro-adapter-i3wm   # or another adapter
```

With a single adapter installed, enwiro selects it automatically. If you install more than one, choose which to use in your config file:

```toml
adapter = "i3wm"
```

Update everything at once with `cargo install-update -a` (requires [`cargo-update`](https://crates.io/crates/cargo-update)).

## Documentation

Full documentation — environments, adapters, recipes, cookbooks, bridges, and the daemon — lives at **[enwi.ro](https://enwi.ro)**.
