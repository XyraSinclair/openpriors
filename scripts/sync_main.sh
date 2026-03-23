#!/usr/bin/env bash
set -euo pipefail

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "run this inside a git worktree" >&2
  exit 1
fi

if ! git diff --quiet || ! git diff --cached --quiet || [[ -n "$(git ls-files --others --exclude-standard)" ]]; then
  echo "refusing to sync: working tree is dirty" >&2
  echo "commit, stash, or use a separate worktree first" >&2
  git status --short
  exit 1
fi

current_branch="$(git symbolic-ref --quiet --short HEAD || true)"
if [[ "$current_branch" != "main" ]]; then
  git checkout main
fi

if ! git pull --rebase origin main; then
  git rebase --abort >/dev/null 2>&1 || true
  echo "sync_main.sh aborted: rebase conflict against origin/main" >&2
  echo "handle the conflict deliberately in a clean worktree or branch" >&2
  exit 1
fi
