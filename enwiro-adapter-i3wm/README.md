# Enwiro i3 adapter

This adapter uses the current i3 workspace's name to derive the enwiro environment name.

It removes the workspace number and any ':' found in the i3 workspace's name.

You can rename your i3 workspaces by running:

```
i3-msg 'rename workspace to "1: my-project"'
```

By [using `workspace number` instead of `workspace [...]` in your i3 config](https://i3wm.org/docs/userguide.html#_changing_named_workspaces_moving_to_workspaces), you'll be able
to switch workspaces using your usual keyboard shortcuts.

## Activating environments

When you run `enwiro activate <NAME>`, the adapter will switch to the
workspace that has the matching environment name. If no such workspace
exists, a new one is created using the lowest available number
(e.g. `3: my-project`).

## Using enwiro from i3 keybindings

i3 keybindings don't go through a login shell, so `~/.cargo/bin` may not be
on `PATH`. Use the full path to enwiro in your i3 config:

```
bindsym $mod+Return exec ~/.cargo/bin/enwiro wrap i3-sensible-terminal
```

Enwiro will automatically discover its plugins (adapters and cookbooks)
installed in the same directory, regardless of `PATH`.
