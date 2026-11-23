# Editing a PR

`jj pr edit <pr-num|rev>` (alias `jj pr e`) resolves a PR from a PR number or a revision (via its local bookmark), fetches its current state, and opens a markdown file in your editor.

## The editor buffer

The buffer is YAML frontmatter followed by the PR body:

```yaml
---
# yaml-language-server: $schema=https://github.com/mrjones2014/jj-gh/releases/download/jj-gh-v0.3.9/frontmatter.schema.json
title: "Add the feature"
base: "master"
labels: ["enhancement"]
reviewers: ["@octocat", "@org/team"]
draft: false
auto_merge: false
auto_merge_method: "merge"
---
PR body goes here.
```

| Field               | Meaning                                                                                      |
| ------------------- | -------------------------------------------------------------------------------------------- |
| `title`             | PR title; required and non-empty.                                                            |
| `base`              | Base branch; required. Prefilled with the resolved base (`owner:branch` for cross-fork PRs). |
| `labels`            | List of label names.                                                                         |
| `reviewers`         | Collaborator `@handle`s or team `@org/team` slugs.                                           |
| `draft`             | Whether the PR is a draft.                                                                   |
| `auto_merge`        | Scope: `false`, `infer`, `all`, or `below`. See [Auto-merge](./auto-merge-rerun.md).         |
| `auto_merge_method` | `merge`, `squash`, or `rebase`; omitted unless non-default.                                  |

If a diff preview is enabled, it is appended after a `# 8< jj-gh: below this line removed on submit >8` marker. Everything from that marker onward is stripped when the buffer is parsed, so the preview never lands in the PR body.

## Only changed metadata is applied

`jj-gh` compares the edited frontmatter and body with the state fetched before opening the editor. It sends only the fields you changed and applies label and reviewer additions and removals individually, rather than replacing their complete lists. This preserves labels added by CI bots or teammates while you were editing, even though they were not in the original buffer.

To change an existing auto-merge request or its merge method, use `jj pr auto-merge` instead: the frontmatter cannot express partial stack endpoints, while the command can move a request to the stack tip or cancel one. `jj pr edit` remains the path for the other metadata fields.

The PR URL is printed on success.

An empty body is refused by default to avoid clobbering a body that exists but failed to load; pass `-f` / `--force` to override. Title and base must remain non-empty.

## Diff preview and editor

- `--diffs` / `--no-diffs` toggle the read-only diff preview; the defaults are the `pr_edit_show_diffs` / `pr_create_show_diffs` config settings (both `true`).
- The preview is rendered from local commits when possible, falling back to GitHub's API.
- `-e` / `--editor` wins over `editor` in config, which wins over jj's `ui.editor` (jj resolves `$JJ_EDITOR`, `$VISUAL`, and `$EDITOR` itself). A common trick is `editor = ["env", "JJ_GH=1", "nvim", "+10"]` to detect this flow in your editor config.

## Editor schema

A JSON Schema for the frontmatter is published in two forms, both as GitHub release assets so every version stays accessible:

- **Version-pinned, for releases:** `https://github.com/mrjones2014/jj-gh/releases/download/jj-gh-v<version>/frontmatter.schema.json`, for example `https://github.com/mrjones2014/jj-gh/releases/download/jj-gh-v0.3.9/frontmatter.schema.json`. Each release gets the asset that matches its code.
- **Rolling, for development:** `https://github.com/mrjones2014/jj-gh/releases/download/nightly/frontmatter.schema.json`, attached to the `nightly` pre-release and updated on every push to `master`.

`jj pr create` and `jj pr edit` insert a `# yaml-language-server: $schema=...` directive as the first line of the frontmatter, pointing at the version-pinned release asset.

The editor buffer uses a `.jj-gh-pr-*.md` filename, so it can be targeted without affecting other Markdown files. For example, with the VS Code YAML extension:

```json
{
  "yaml.schemas": {
    "https://github.com/mrjones2014/jj-gh/releases/download/jj-gh-v0.3.9/frontmatter.schema.json": [
      "**/.jj-gh-pr-*.md"
    ]
  }
}
```

You can also generate the schema locally with `jj-gh config frontmatter-schema`.

See [Configuration](../configuration.md) for the editor and diff settings, and [Templates](../templates.md) for how the create flow seeds the body.
