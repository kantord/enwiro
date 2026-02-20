# Creating an Enwiro Cookbook

A cookbook is a standalone program that tells enwiro how to discover and set up
project environments. You can write one in any language — Python, Bash, Go,
Rust, JavaScript, etc. Enwiro communicates with cookbooks by running them as
subprocesses and reading their stdout.

## How Enwiro Finds Your Cookbook

Enwiro scans every directory in `$PATH` for executables whose name starts with
`enwiro-cookbook-`. The part after that prefix becomes the cookbook name.

| Binary name                  | Cookbook name |
|------------------------------|-------------|
| `enwiro-cookbook-docker`      | `docker`    |
| `enwiro-cookbook-npm`         | `npm`       |
| `enwiro-cookbook-my-tool`     | `my-tool`   |

The binary must have executable permissions. Non-executable files are silently
ignored.

**Tip:** During development, place your binary in `~/.local/bin/` (or anywhere
on `$PATH`). Enwiro also checks the directory containing its own executable, so
co-locating your cookbook binary there works too.

## Subcommands

Your cookbook binary must handle three subcommands passed as the first argument:

### `list-recipes`

```
enwiro-cookbook-yourname list-recipes
```

Print available recipes to stdout as **JSON lines** (one JSON object per line).
Each object must have a `name` field and may optionally include a `description`:

```json
{"name":"my-project"}
{"name":"another-project","description":"A short description of this project"}
```

- Each line must be a valid JSON object with at least a `"name"` field.
- The `"description"` field is optional. Omit it or set it to `null` if there
  is no description.
- Recipe names must not contain newlines or null bytes.
- Unknown fields are ignored, so you can add extra fields for your own use.
- Exit with code 0 on success.

**Sorting matters.** Enwiro preserves the order your cookbook returns. Print
recipes in the order that makes the most sense for your use case. For example,
if your cookbook discovers time-based resources, sort by most recently updated
first. If there are "primary" entries (like a main project directory) that
should always appear first, put them at the top.

Within a cookbook, a good default is: most relevant or most recently used items
first.

### `cook <recipe_name>`

```
enwiro-cookbook-yourname cook my-project
```

Prepare an environment for the given recipe and print its **filesystem path**
to stdout. This is the directory that enwiro will symlink into `~/.enwiro_envs/`.

The path should be absolute. Enwiro trims surrounding whitespace.

```
/home/user/projects/my-project
```

This subcommand is where your cookbook does its real work — cloning a repo,
creating a worktree, setting up a directory, etc. If the environment already
exists (e.g., a previously cloned repo), just print the existing path.

Exit with code 0 on success. On failure, exit non-zero and write an error
message to stderr.

### `metadata`

```
enwiro-cookbook-yourname metadata
```

Print a JSON object to stdout describing your cookbook. This subcommand is
**optional** — if your binary doesn't support it (exits non-zero or doesn't
recognize the command), enwiro uses sensible defaults.

Currently the only field is `defaultPriority`:

```json
{"defaultPriority": 40}
```

**Priority** controls the order your cookbook's recipes appear relative to other
cookbooks. Lower numbers appear first. If omitted, the default priority is 50.

The built-in cookbooks use:

| Cookbook  | Priority |
|----------|----------|
| git      | 10       |
| chezmoi  | 20       |
| github   | 30       |
| (default)| 50       |

When two cookbooks share the same priority, they are sorted alphabetically by
name.

Unknown fields in the JSON are ignored, so you can safely add your own
fields for forward compatibility.

## Output Encoding

All stdout output must be valid UTF-8. If your binary produces invalid UTF-8,
enwiro treats it as an error.

## Error Handling

- **Exit code 0** means success — stdout is parsed as results.
- **Non-zero exit code** means failure — stdout is discarded and stderr is
  shown to the user as the error message.
- If `list-recipes` fails, enwiro skips your cookbook and continues with the
  others. Your cookbook failing does not break the overall recipe list.

## Example: A Minimal Cookbook in Bash

Here's a complete cookbook that discovers directories in `~/projects/`:

```bash
#!/usr/bin/env bash
set -euo pipefail

case "${1:-}" in
    list-recipes)
        # List project directories as JSON lines, sorted by modification time (newest first)
        for dir in $(ls -t ~/projects/); do
            if [ -d "$HOME/projects/$dir" ]; then
                printf '{"name":"%s"}\n' "$dir"
            fi
        done
        ;;
    cook)
        recipe="$2"
        path="$HOME/projects/$recipe"
        if [ -d "$path" ]; then
            echo "$path"
        else
            echo "Project not found: $recipe" >&2
            exit 1
        fi
        ;;
    metadata)
        echo '{"defaultPriority": 40}'
        ;;
    *)
        echo "Unknown subcommand: ${1:-}" >&2
        exit 1
        ;;
esac
```

Save this as `enwiro-cookbook-projects`, make it executable (`chmod +x`), and
place it anywhere on your `$PATH`. Enwiro will discover it as the `projects`
cookbook.

## Example: A Cookbook in Python

```python
#!/usr/bin/env python3
import json
import os
import sys

WORKSPACE_DIR = os.path.expanduser("~/workspace")

def list_recipes():
    if not os.path.isdir(WORKSPACE_DIR):
        return
    entries = sorted(
        os.listdir(WORKSPACE_DIR),
        key=lambda d: os.path.getmtime(os.path.join(WORKSPACE_DIR, d)),
        reverse=True,  # newest first
    )
    for name in entries:
        full = os.path.join(WORKSPACE_DIR, name)
        if os.path.isdir(full):
            print(json.dumps({"name": name}))

def cook(recipe_name):
    path = os.path.join(WORKSPACE_DIR, recipe_name)
    if not os.path.isdir(path):
        print(f"Not found: {recipe_name}", file=sys.stderr)
        sys.exit(1)
    print(path)

def metadata():
    print(json.dumps({"defaultPriority": 45}))

if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else ""
    if cmd == "list-recipes":
        list_recipes()
    elif cmd == "cook":
        cook(sys.argv[2])
    elif cmd == "metadata":
        metadata()
    else:
        print(f"Unknown subcommand: {cmd}", file=sys.stderr)
        sys.exit(1)
```

## How It All Fits Together

When a user runs `enwiro list-all`, here's what happens:

1. Enwiro discovers all `enwiro-cookbook-*` binaries on `$PATH`.
2. It calls `metadata` on each to learn their priority (falling back to 50).
3. Cookbooks are sorted by priority (lowest first), then alphabetically.
4. It calls `list-recipes` on each cookbook in order.
5. Results are combined into a single list, preserving cookbook order and each
   cookbook's internal ordering.
6. Recipes that match an already-existing environment are filtered out (they
   can't be cooked again since the environment already exists).

When a user activates a recipe:

1. Enwiro calls `cook <recipe_name>` on the appropriate cookbook.
2. The returned path is symlinked into `~/.enwiro_envs/<recipe_name>/`.
3. The window manager workspace is switched to the new environment.

## Tips

- **Keep `list-recipes` fast.** Enwiro caches recipe lists via a background
  daemon, but the first call (or a cache miss) runs synchronously. Avoid
  network calls in `list-recipes` if possible, or cache results yourself.
- **`cook` can be slow.** It's fine for `cook` to clone a repo, create a
  worktree, or do other setup work — the user expects to wait.
- **Idempotent cooking.** If `cook` is called for a recipe that was already
  cooked, just return the existing path. Don't fail or recreate.
- **Don't worry about metadata.** If you skip the `metadata` subcommand
  entirely, your cookbook will still work — it just gets the default priority of
  50.
- **Recipe names are identifiers.** Users type them (e.g., `enwiro activate
  my-project`), so keep them short and filesystem-friendly. Avoid spaces.
