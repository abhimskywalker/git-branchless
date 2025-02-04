# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

- Added: `git branchless init` now sets `advice.detachedHead false`, to reduce the incidence of scary messages.
- Added: aliasing `git` to `git-branchless wrap` improves which commands are grouped together for `git undo`, and possibly enables more features in the future.
- Added: Created `git move` command, which rebases entire subtrees at once.
- Added: The `git-branchless wrap` command can now take an explicit `--git-executable` parameter to indicate which program to run.
- Fixed: Visible commits in the smartlog sometimes showed the reason that they were hidden, even though they were visible.
- Fixed: The working copy was sometimes left dirty after a `git undo`, even if it was clean beforehand.
- Fixed: `git-branchless` now supports Git v2.31.
- Fixed: `git restack` now doesn't infinite-loop on certain rebase conflict scenarios.
- Fixed: `git smartlog` now doesn't crash for some cases of hidden merge commits.

## [0.3.1] - 2021-04-15

- Added: Hidden commits which appear in the smartlog now show the reason why they're hidden.
- Fixed: Historical commits displayed in `git undo` were sometimes rendered incorrectly, indicating that they were hidden/visible inappropriately. They now display the true historical visibility.

## [0.3.0] - 2021-04-08

- BREAKING: Events are now grouped into transactions. This improves the UX around `git undo`, since it can undo groups of related events. This breaks the on-disk database format.

## [0.2.0] - 2020-03-15

Ported to Rust. No new features.

- Performance for repeated calls to Git hooks is significantly improved. This can happen when rebasing large commit stacks.
- The `git undo` UI has been changed to use a Rust-specific TUI library (`cursive`).

## [0.1.0] - 2020-12-18

First beta release. Supports these commands:

- `git sl`/`git smartlog`.
- `git hide`/`git unhide`.
- `git prev`/`git next`.
- `git restack`.
- `git undo`.
