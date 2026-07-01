# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.10](https://github.com/mrjones2014/jj-gh/compare/jj-gh-v0.2.9...jj-gh-v0.2.10) - 2026-07-01

### Fixed

- *(lint)* Resolve lint warning and fix CI check
- *(pr-restack)* Add `is:open` filter to PR queries
- *(deps)* update cargo minor and patch

### Other

- Merge pull request #222 from mrjones2014/renovate/lock-file-maintenance

## [0.2.9](https://github.com/mrjones2014/jj-gh/compare/jj-gh-v0.2.8...jj-gh-v0.2.9) - 2026-06-23

### Fixed

- *(diffs)* make marker matching robust against misbehaving formatters
- *(deps)* update cargo minor and patch
- *(bookmarks)* Fix logic to use existing bookmark if one exists
- *(editor)* Fix editor option precedence

## [0.2.8](https://github.com/mrjones2014/jj-gh/compare/jj-gh-v0.2.7...jj-gh-v0.2.8) - 2026-06-15

### Fixed

- *(cli)* prevent shell-arg panic & precedence being wrong in some cases
- *(meta)* Fix package descriptions

## [0.2.7](https://github.com/mrjones2014/jj-gh/compare/jj-gh-v0.2.6...jj-gh-v0.2.7) - 2026-06-14

### Added

- *(pr)* Add `pr url` subcommand to print URL to a PR by number or rev
- *(pr-create)* Add `--no-edit` option
- *(pr-edit)* Show diffs in edit view
- *(pr-create)* add `--title-template` an `--pick-title` options

### Fixed

- *(editor)* Force editor to run interactively when part of a piped cmd
- *(auth)* Get GH token lazily
- *(log)* Fix logging format

### Other

- *(config)* Make config merging less verbose
- *(docs)* Update README.md
- *(api)* Refactor architecture to abstract repeated algorithms
- *(docs)* Update README.md
- *(docs)* Generate manpage for `jj-gh`

## [0.2.6](https://github.com/mrjones2014/jj-gh/compare/jj-gh-v0.2.5...jj-gh-v0.2.6) - 2026-06-13

### Added

- *(ci)* Upload binaries to GitHub Releases for `cargo-binstall`
- *(pr-retry-failed)* Add `--all` flag to retry for all local PRs

### Fixed

- *(pr-create)* Don't try to validate PR body
- *(cli)* Simplify logging format to avoid contrast readability issues

### Other

- *(dpes)* update GitHub GraphQL schema
- Merge pull request #184 from mrjones2014/renovate/lock-file-maintenance
- *(deps)* lock file maintenance
- *(deps)* update cargo minor and patch
- *(fmt)* Use turbofish over type ascription

## [0.2.5](https://github.com/mrjones2014/jj-gh/compare/jj-gh-v0.2.4...jj-gh-v0.2.5) - 2026-06-11

### Fixed

- *(cli)* Clean up config loader and macro, improve responsiveness
- *(cli)* Fix color regression by cleaning up & standardizing cmd runner
- *(jj)* Stream output for cmds where we don't need to parse its STDOUT
- *(log)* Improve logging readability in many areas
- *(remotes)* Detect default remote instead of relying on custom config

### Other

- *(cli)* Reorganize modules
- *(docs)* Clarify details about GitHub tokens
- Merge pull request #164 from mrjones2014/mrj/push-puqtnponxmuw
- *(docs)* update README.md with Usage and Tips and Tricks sections

## [0.2.4](https://github.com/mrjones2014/jj-gh/compare/jj-gh-v0.2.3...jj-gh-v0.2.4) - 2026-06-05

### Added

- *(pr)* Show upstream owner when creating/editing cross-fork PRs

### Fixed

- *(cli)* Clear spinner when an error is produced

### Other

- update GitHub GraphQL schema
- Merge pull request #155 from mrjones2014/renovate/lock-file-maintenance
- *(deps)* lock file maintenance
- *(deps)* update cargo minor and patch

## [0.2.3](https://github.com/mrjones2014/jj-gh/compare/jj-gh-v0.2.2...jj-gh-v0.2.3) - 2026-06-04

### Added

- *(pr-create)* Show diffs in editor and strip on submit
- *(pr)* Add `restack` subcommand to interactively update PR base refs

### Fixed

- *(deps)* `serde_yml` -> `noyalib`
- *(ci)* Don't bother running semver checks on CLI crate
- *(auto-merge)* Produce an error when merge queues are enabled on repo
- *(cli)* Use spinner for `pr log`

### Other

- *(config)* Refactor/simplify config/args layering with proc macro

## [0.2.2](https://github.com/mrjones2014/jj-gh/compare/jj-gh-v0.2.1...jj-gh-v0.2.2) - 2026-06-01

### Fixed

- *(pr)* Fix GraphQL query and vendor schema

## [0.2.1](https://github.com/mrjones2014/jj-gh/compare/jj-gh-v0.2.0...jj-gh-v0.2.1) - 2026-06-01

### Added

- *(pr)* Add `retry-failed` subcommand (with `rerun` alias)
- *(pr)* Add `edit` subcommand

### Fixed

- *(deps)* `nu_ansi_term` -> `anystyle` since already depend via clap
- *(cli)* Add colors/styles to help output
- *(ci)* Use Cachix for CI caching
- *(template)* Add a way to add static content to PR body template

### Other

- *(docs)* Add CONTRIBUTING.md
- *(auth)* Put auth args in global arguments

## [0.2.0](https://github.com/mrjones2014/jj-gh/compare/jj-gh-v0.1.3...jj-gh-v0.2.0) - 2026-05-29

### Added

- *(pr)* [**breaking**] Use jj templates for `pr fetch` and PR body

### Fixed

- *(nix)* Validate home-manager module opts against Rust `Config` struct

### Other

- *(jj)* Extract reusable template-alias config injection
- *(docs)* Add note about nerdfont support in pager

## [0.1.3](https://github.com/mrjones2014/jj-gh/compare/jj-gh-v0.1.2...jj-gh-v0.1.3) - 2026-05-28

### Fixed

- *(api)* Use GraphQL for finding open PR for revision
- *(docs)* Update permissions section
- *(changelog)* Make release-plz consider `hm-module.nix`
- *(auth)* Use test abstractions to avoid running real process in tests
- *(remotes)* Make remotes configurable
- *(auth)* Allow `gh auth token` to be used for authentication
- *(completions)* Split comments so completions have concise description
- *(auto-merge)* Fix auto-merge failing when merge queues are enabled

### Other

- *(deps)* Update serde_yml
- *(graphql)* Organize queries and make code more consistent

## [0.1.2](https://github.com/mrjones2014/jj-gh/compare/jj-gh-v0.1.1...jj-gh-v0.1.2) - 2026-05-27

### Added

- *(pr-create)* Add `reviewers` to frontmatter fields
- *(cli)* Add completions
- *(cli)* Add completions when invoked as a `jj` alias

### Fixed

- *(git)* Use `gix` crate to resolve git configs and such
- *(pr-log)* Fix missing PRs for locally diverged bookmarks
- *(pr)* Remove extraneous newline when opening editor

## [0.1.1](https://github.com/mrjones2014/jj-gh/compare/jj-gh-v0.1.0...jj-gh-v0.1.1) - 2026-05-26

### Fixed

- *(cli)* Support tokens from `GH_TOKEN` and `JJ_GH_TOKEN` env vars

## [0.1.0](https://github.com/mrjones2014/jj-gh/releases/tag/jj-gh-v0.1.0) - 2026-05-25

### Added

- *(ci)* Setup `release-plz` to publish to crates.io
- *(pr-log)* Show whether auto-merge is enabled
- *(pr-log)* Show merge status in `pr log` default template
- *(log)* Use nerdfont icons in default template
- *(cli)* Add `pr log` subcommand
- *(docs)* Auto generate docs
- *(cli)* Prettier log format
- *(nix)* Add home-manager module
- *(cli)* Add feature to fetch PRs across forks
- *(ci)* Use `cargo-udeps`
- *(ci)* Add nix cache action
- *(cli)* Use `trunk()` revset to detect default branch
- *(pr)* End-to-end `pr create` orchestrator
- *(gh)* GitHub API client and PR target resolution
- *(jj)* Read layer for revs, bookmarks, and remotes
- *(config)* Implement configuration and `gh-askpass` functionality
- *(cli)* Initial setup

### Fixed

- *(docs)* Fix `jj` alias in example docs
- *(cli)* Fix boolean argument parsing and config layering
- *(treefmt)* Format `*.gql` files with prettier
- *(graphql)* Use search query to pull PRs more precisely
- *(lints)* Show warnings normally in LSP but deny in flake checks
- *(alias)* Fix how `jj` alias is set up
- *(alias)* Fix how `jj` alias is set up
- *(docs)* Fix `jj` alias docs for manual install
- *(cli)* Fix boolean option handling
- *(cli)* Fix help text
- move graphql-validate script out of nix/ directory
- *(docs)* Fix home-manager module
- *(docs)* Fix home-manager module
- *(nix)* Fix home-manager module definition
- *(docs)* Fix home-manager module docs
- *(template)* Add additional newline
- *(pr)* Fix open PR resolution w.r.t forks vs. not fork
- *(config)* Use `jj` to resolve canonical config files
- *(config)* Use `[jj-gh]` key for config
- *(ci)* Allow dispatching manually
- *(ci)* Add correct permissions for caching
- *(ci)* Run on correct branch

### Other

- *(pr)* Add utility to lookup PR from number or revision
- *(cli)* Improve `pr` module organization
- *(cli)* Simplify argument handling by using `figment` more
- *(ci)* Automated flake.lock updates
- *(tools)* Set up treefmt.nix
- *(docs)* Document required GitHub token permissions
- *(cli)* Consistently use `tokio::process:Command`
- *(docs)* Document using nix to build
- *(cli)* Use dispatcher pattern for `pr` subcommand
- *(docs)* Add README.md
- *(cli)* Drop dependency on `git` on `$PATH`
- *(ci)* Add GitHub Actions
