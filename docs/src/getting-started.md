# Getting started

This assumes `jj-gh` is installed and a GitHub token is available. If not, start with [Installation](./installation.md) and [Authentication](./authentication.md).

## 1. Set the `jj` alias (if needed)

Add this to your jj config so `jj-gh` is reachable as `jj pr`:

```toml
# ~/.config/jj/config.toml
[aliases]
pr = ["util", "exec", "--", "jj-gh", "pr"]
```

The Nix `home-manager` module can install this for you. See [Installation](./installation.md#set-up-a-jj-alias).

## 2. See PR metadata in your log

```sh
jj pr log
```

This is like `jj log`, with PR number, CI status, and merge status added inline for commits that have an open PR. Arguments after `--` are forwarded to `jj log`, for example `jj pr log -- -r 'mine()'`. See [Log](./workflows/log.md).

## 3. Create a PR from a revision

> [!TIP]
> `jj-gh` sets `JJ_GH="1"` in the editor process's environment; this can be
> used to detect the `jj-gh` flow and apply tweaks to your editor specifically
> for `jj-gh`, for example in Neovim, by checking `vim.env.JJ_GH ~= nil`.

```sh
jj pr create <rev>
```

Your editor opens on a Markdown PR body. The first block is YAML frontmatter where you can set the title, base, labels, reviewers, draft state, and auto-merge before submitting. The body is seeded from your configured jj template or the repo's `.github/PULL_REQUEST_TEMPLATE.md`. See [Configuration](./configuration.md) and [Templates](./templates.md).

## 4. Preview stack changes

```sh
jj pr stack --dry-run
```

`jj pr stack` shows the `jj log` it is working from and a plan for making GitHub match your local stack shape. `--dry-run` prints that plan and exits without touching GitHub. When you are ready to apply it, run `jj pr stack`; it prints the plan and asks for confirmation before making changes. See [Reconciling stacks](./stack-reconciliation.md) and [Stacked PRs](./stacked-prs.md).

## 5. Open the PR

```sh
jj pr url <rev>
```

This looks up the PR for the revision (or PR number) and prints its full URL, which is convenient in pipes such as `jj pr url <rev> | pbcopy`. See [Everyday workflows](./workflows/overview.md).
