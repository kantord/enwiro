# enwiro


Enwiro is the successor to `i3-env`.

## Usage

### Integration with desktop environment

`enwiro` integrates with your desktop environment using adapters such as
`enwiro-adapter-i3wm`. Adapters implement a set of basic features
which `enwiro` can use in order to connect to your operating system's
graphical environment.

#### Currently available adapters:

* `enwiro-adapter-i3wm` supports i3

#### Configuring desktop environment integration

`enwiro` adapters have names prefixed with `enwiro-adapter-` and can be installed
using `cargo`. For example, to install an adapter for i3, you can run

`cargo install enwiro-adapter-i3wm`.

In your configuration file, set `adapter` to your desired adapter. For example, to
use `enwiro-adapter-i3wm`, set `adapter` to `i3wm`.


```toml
adapter = "i3wm"
```
 
 
## Concepts
 
### Environment

<p align="center">
 <img src="environments.png" width="400" />
</p>

An `enwiro` is a local folder or a symbolic link pointing to a folder.

An environment serves as a working directory for your applications,
such as your terminal or your code editor.

An environment could be linked to:

* Any branch of a Git repository checked out on your local computer
* A folder on a remote computer
* Any folder on your computer


### Recipe

<p align="center">
 <img src="recipes.png" width="400" />
</p>

Recipes are automatically generated blueprints for environments.

While they do not exist as environments on your computer yet, you can
interact with them as if they were environments and when you do so,
they will be created on the fly for you.

Recipes can have a hierarchical nature. For instance, the recipe for a
Git repository might refer to the main working tree of the Git repository,
and serve as the "parent recipe" to recipes for creating new worktrees for
the same Git repository.


### Lens

A lens is an activate mode in an environment. Some apps and window managers
might use this to provide a different experience, layout or different features
for a different part of workflow.

It could be said that lenses are the `enwiro`'s interpretation of modal editing.

Use cases might include:
- use different layouts for each step of test driven development
- different layout for rebasing, code review, editing, etc.
- different layout based on what monitor is being used
