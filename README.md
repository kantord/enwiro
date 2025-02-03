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

## Concepts

### Environment

<p align="center">
 <img src="environments.png" width="400" />
</p>

An `enwiro` is a local folder or a symbolic link pointing to a folder. To define
an environment, create a folder or a symbolic link inside your `workspaces_directory`
(`$HOME/.enwiro_envs` by default). The name of the folder or symlink will be used
as the environment name.

An environment serves as a working directory for your applications, such as your
terminal or your code editor. To run a command inside an environment, switch to a
desktop workspace with a name matching the name of the environment you want to use
and run  `enwiro wrap <COMMAND> [-- [COMMAND_ARGS]...]`. If no matching environment
is found, it will default to using your home direcory.

An environment variable `ENWIRO_ENV` containing the `enwiro` environment name
will also be added before runnning commands with `enwiro wrap ...`.

An environment could be linked to:

- Any branch of a Git repository checked out on your local computer
- A folder on a remote computer
- Any folder on your computer

### Recipe

<p align="center">
 <img src="recipes.png" width="400" />
</p>

Recipes are automatically generated blueprints for environments.

While they do not exist as environments on your computer yet, you can interact
with them as if they were environments and when you do so, they will be created
on the fly for you.

Recipes can have a hierarchical nature. For instance, the recipe for a Git
repository might refer to the main working tree of the Git repository, and serve
as the "parent recipe" to recipes for creating new worktrees for the same Git
repository.

### Cookbook

Cookbooks are plugins that contain recipes. You can add more recipes to your
enwiro by installing and configuring more cookbooks.

List of currently available cookbooks:

- `enwiro-cookbook-git`: Generate environments using Git repositories
