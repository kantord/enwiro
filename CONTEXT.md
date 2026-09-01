# Enwiro

Enwiro manages project environments: it materializes them from recipes and
launches applications inside them, integrated with the user's window manager
or multiplexer through adapters.

## Language

**Environment**:
A named directory holding a project's working files, in which applications
can be launched. Its name is derived from the workspace name.
_Avoid_: enw (colloquial), workspace (that is the WM-side concept)

**Workspace**:
The window-manager or multiplexer container (i3 workspace, tmux session)
that an environment is presented in. Managed through the adapter.
_Avoid_: environment

**Recipe**:
A description of how to create an environment, offered by a cookbook.

**Cookbook**:
A plugin that lists recipes and can cook them.

**Cooking**:
Materializing a recipe into an environment. Synchronous work owned by
activation; a cook is either finished or not, there is no observable
in-progress state.
_Avoid_: provisioning, preparing (UI copy may say "preparing")

**Activation**:
Switching to (creating if needed) the workspace for an environment and
cooking the environment if it does not exist yet. The sole owner of cooking
in interactive flows.

**Adapter**:
The integration that talks to the window manager or multiplexer: it resolves
the current workspace's environment name and activates workspaces.

**Wrapping**:
Launching a command inside an environment (its directory and identity
applied to the process). Degrades to a bare launch when no environment can
be resolved.

**Shell launch** (`enw shell`):
Wrapping the user's shell in the current environment, waiting for a pending
cook to finish before starting it. Falls back to a plain, unwrapped shell
when no environment exists or nothing enwiro-related is reachable.

**Project**:
A named, configurable codebase-identity that environments resolve to. It is
the home of per-codebase policy (isolation first, other rules later), carried
by the project-level `.enwiro.toml`. Distinct from an environment, which is
per-workspace; the same project can be present as many environments (branches,
worktrees).
_Avoid_: policy scope (same thing, coined before "project" was settled)

**Isolation**:
A project policy deciding how an environment's applications are wrapped when
launched: run on the host, inside a container, or inside a microVM. Owned by
the wrap layer, not the recipe or cookbook.
_Avoid_: sandboxing (conflates with the loop layer; enwiro only owns the
isolation substrate, not the agentic loop that runs on it)
