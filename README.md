# enwiro


Enwiro is the successor to `i3-env`.
 
 
## Concepts
 
### Environment

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
