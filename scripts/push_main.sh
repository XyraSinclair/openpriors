#!/usr/bin/env bash
set -euo pipefail

message="${1:-}"
if [[ -z "$message" ]]; then
  echo "usage: $0 \"clear commit message\"" >&2
  exit 1
fi

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "run this inside a git worktree" >&2
  exit 1
fi

current_branch="$(git symbolic-ref --quiet --short HEAD || true)"
if [[ "$current_branch" != "main" ]]; then
  echo "refusing to push: current branch is ${current_branch:-detached}, not main" >&2
  exit 1
fi

if ! git diff --quiet || [[ -n "$(git ls-files --others --exclude-standard)" ]]; then
  echo "refusing to push: unstaged or untracked changes are present" >&2
  echo "stage intended paths explicitly or use a separate worktree" >&2
  git status --short
  exit 1
fi

if ! git diff --cached --quiet; then
  git commit -m "$message"
else
  echo "no staged changes; syncing and pushing main as-is" >&2
fi

if ! git pull --rebase origin main; then
  git rebase --abort >/dev/null 2>&1 || true
  echo "push_main.sh aborted: rebase conflict against origin/main" >&2
  echo "resolve it deliberately in a clean worktree or branch, then push" >&2
  exit 1
fi

git push origin HEAD:main
