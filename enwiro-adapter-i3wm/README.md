# Enwiro i3 adapter

This adapter uses the current i3 workspace's name to derive the enwiro environment name.

It removes the workspace number and any ':' found in the i3 workspace's name.

You can rename your i3 workspaces by running :

```
i3-msg 'rename workspace 1:name'
```

By [using `workspace number` instead of `workspace [...]` in your i3 config](https://i3wm.org/docs/userguide.html#_changing_named_workspaces_moving_to_workspaces), you'll be able
to switch workspaces using your usual keyboard shortcuts.
