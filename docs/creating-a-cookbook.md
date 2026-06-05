# Creating an Enwiro Cookbook

A cookbook is a standalone program that tells enwiro how to discover and set up
project environments. You can write one in any language - Python, Bash, Go,
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

Your cookbook binary handles subcommands passed as the first argument.
`list-recipes` and `cook` are required; `equivalents`, `metadata`, and `listen`
are optional.

### `list-recipes`

```
enwiro-cookbook-yourname list-recipes
```

Print available recipes to stdout as **JSON lines** (one JSON object per line).
Each object must have a `name` field and may optionally include `description`
and `sort_order`:

```json
{"name":"my-project","sort_order":0}
{"name":"another-project","description":"A short description","sort_order":50}
```

- Each line must be a valid JSON object with at least a `"name"` field.
- The `"description"` field is optional. Omit it or set it to `null` if there
  is no description.
- The `"sort_order"` field is optional (defaults to 0). It is a number from 0
  to 100 that controls how this recipe ranks globally against recipes from other
  cookbooks. Lower values appear first. See **Global sort order** below.
- Recipe names must not contain newlines or null bytes.
- Unknown fields are ignored, so you can add extra fields for your own use.
- Exit with code 0 on success.

**Ordering within your cookbook:** a good default is to list the most relevant
or most recently used items first. How that ranking is then combined with other
cookbooks' recipes is what `sort_order` controls - see below.

#### Global sort order

The `sort_order` field (0-100) lets enwiro merge recipes from different
cookbooks into a single relevance-ranked list. Without it, all recipes from a
higher-priority cookbook would appear before any recipe from a lower-priority
one, regardless of individual relevance. Ties within the same `sort_order` are
broken by cookbook priority (see [`metadata`](#metadata)) and then
alphabetically by name.

**Convention:** After sorting your recipes internally, assign `sort_order`
linearly based on position. For a list of `total` recipes, the recipe at
position `index` (starting from 0) gets:

```
if total <= 1:
    sort_order = 0
else:
    sort_order = (index * 100) / (total - 1)
```

This maps the first recipe to 0 and the last to 100, with intermediate values
spread evenly. The built-in cookbooks all use this convention.

**Example:** A git cookbook with 5 recipes sorted newest-first would output
`sort_order` values of 0, 25, 50, 75, 100. A GitHub cookbook with 3 items would
output 0, 50, 100. When combined, the globally sorted list interleaves them by
relevance rather than grouping by cookbook.

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

This subcommand is where your cookbook does its real work - cloning a repo,
creating a worktree, setting up a directory, etc. If the environment already
exists (e.g., a previously cloned repo), just print the existing path.

Exit with code 0 on success. On failure, exit non-zero and write an error
message to stderr.

### `equivalents <env_path>...`

```
enwiro-cookbook-yourname equivalents /path/to/env-a /path/to/env-b
```

**Optional.** Given the working-directory paths of existing environments, print
the recipe/environment **names** your cookbook considers each of them equivalent
to — names that would cook the *same* environment. Print one name per line on
stdout; print nothing for environments you don't recognise.

This is how enwiro hides a recipe that has effectively already been cooked,
*even when a different cookbook cooked it under a different name*. For example,
the git cookbook implements `equivalents` by opening each path as a git working
tree and printing `repo@<branch>`:

```
$ enwiro-cookbook-git equivalents ~/.enwiro_envs/repo#42
repo@pr-42
```

So once a GitHub pull-request environment named `repo#42` exists (its worktree
is checked out at branch `pr-42`), the git cookbook's own `repo@pr-42` branch
recipe is recognised as already cooked and dropped from `enw ls`.

How enwiro uses it:

- enwiro hands **every** existing environment's path to **every** cookbook and
  unions all the names it gets back with the environment names themselves. A
  recipe whose name is in that set is hidden. enwiro itself contains no
  cookbook-specific logic — the equivalence data comes entirely from cookbooks.
- The data is derived live on each `enw ls`, so it works regardless of when an
  environment was cooked or whether the recipe that originally cooked it still
  exists. There is no per-environment bookkeeping to keep in sync.
- Names are plain recipe/environment names — the same flat, globally-unique
  namespace `name` lives in. No cookbook prefix.
- This only hides recipes whose equivalent **already exists** as an environment.
  Recipes for things not yet cooked stay listed, so the user still chooses what
  to cook; enwiro only removes what's already done.
- Best-effort: a cookbook that doesn't implement `equivalents` (exits non-zero
  or doesn't recognise the command) simply contributes no equivalences.

The payload is still piped on stdin as for other subcommands; ignore it if you
don't need config.

### `metadata`

```
enwiro-cookbook-yourname metadata
```

Print a JSON object to stdout describing your cookbook. This subcommand is
**optional** - if your binary doesn't support it (exits non-zero or doesn't
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

### `listen`

```
enwiro-cookbook-yourname listen
```

**Optional.** A long-running subcommand: instead of exiting, your binary stays
running and the daemon reads newline-delimited JSON from its stdout. Use it to
keep enwiro up to date when things change in the background - a repo gets a new
branch, a pull request is merged, and so on. Print an update line whenever
something changes, then go back to sleep. Each line is one of two kinds:

- **Updated recipe list:**
  `{"type":"recipes","data":[ ...recipes... ]}` - the full current set of
  recipes (the same objects [`list-recipes`](#list-recipes) prints), replacing
  whatever was sent before.
- **Status update:**
  `{"type":"status_changed","recipe":"<name>","status":{ ... }}` - marks one
  environment as finished or always-on, so its state shows up in `enw ls`
  without the user running `enw mark` by hand. The two statuses a cookbook may
  send are `{"type":"done"}` (the work is finished, e.g. the branch was merged)
  and `{"type":"evergreen"}` (an environment that is never "finished", like a
  dotfiles or notes directory).

**Cookbooks only ever set `done` or `evergreen`.** The `active` and `waiting`
statuses are the user's - they set those by hand with `enw mark`, and a cookbook
must never send them. If you're not sure an environment is done, send nothing.

#### A note for Rust cookbooks

If you write your cookbook in Rust, you don't have to implement this loop or the
JSON formatting yourself. The `enwiro_sdk` crate provides two helpers in its
`listen` module, `serve` and `serve_updates`, that run the loop, format each
line correctly, and skip sending an update when nothing actually changed. Wrap
your "what are the current recipes and statuses?" logic in one of them and the
SDK handles the rest.

## Output Encoding

All stdout output must be valid UTF-8. If your binary produces invalid UTF-8,
enwiro treats it as an error.

## Error Handling

- **Exit code 0** means success - stdout is parsed as results.
- **Non-zero exit code** means failure - stdout is discarded and stderr is
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

When a user runs `enw ls`, here's what happens:

1. Enwiro discovers all `enwiro-cookbook-*` binaries on `$PATH`.
2. It calls `metadata` on each to learn their priority (falling back to 50).
3. It calls `list-recipes` on each cookbook.
4. All recipes are collected into a single list and sorted globally by
   `(sort_order, cookbook priority, name)`. This means a highly relevant recipe
   from a low-priority cookbook can appear above a less relevant recipe from a
   high-priority cookbook.
5. Recipes that match an already-existing environment are filtered out (they
   can't be cooked again since the environment already exists).

When a user activates a recipe:

1. Enwiro calls `cook <recipe_name>` on the appropriate cookbook.
2. The returned path is symlinked into `~/.enwiro_envs/<recipe_name>/`.
3. The window manager workspace is switched to the new environment.

## Advanced: building on another cookbook

Most cookbooks are self-contained, so you can skip this section unless you need
it. Occasionally one cookbook wants to reuse an operation another already
implements.

The general-purpose way to do this, **which works no matter what language each
cookbook is written in**, is subprocess delegation: the daemon exposes a
`cookbook.invoke` call over its socket so one cookbook can ask another to run an
operation and hand back the result. Because it only relies on running a binary
and reading its output, it works across any language boundary.

If both cookbooks happen to be written in the same language, you can instead
depend on the other as a normal library and call its code directly - simpler
when it applies, but subprocess delegation is the option that always works.

## Tips

- **Keep `list-recipes` fast.** Enwiro caches recipe lists via a background
  daemon, but the first call (or a cache miss) runs synchronously. Avoid
  network calls in `list-recipes` if possible, or cache results yourself.
- **`cook` can be slow.** It's fine for `cook` to clone a repo, create a
  worktree, or do other setup work - the user expects to wait.
- **Idempotent cooking.** If `cook` is called for a recipe that was already
  cooked, just return the existing path. Don't fail or recreate.
- **Don't worry about metadata.** If you skip the `metadata` subcommand
  entirely, your cookbook will still work - it just gets the default priority of
  50.
- **Recipe names are identifiers.** Users type them (e.g., `enw activate
  my-project`), so keep them short and filesystem-friendly. Avoid spaces.
