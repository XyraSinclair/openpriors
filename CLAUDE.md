# CLAUDE.md

This repo is in the fast direct-to-main collaboration set.

If the current checkout is clean before work:

```bash
./scripts/sync_main.sh
```

After a minimum good chunk of work:

```bash
git add <intentional-paths>
./scripts/push_main.sh "<clear message>"
```

Guardrails:

- stage intended paths only
- do not force-push `main`
- do not use `git add -A` unless the whole worktree is intentionally yours
- if the current checkout is dirty with unrelated work, use a separate worktree
  or clean checkout
- if a sync script aborts because of a rebase conflict, handle it deliberately
  in a clean worktree or branch

Read **`AGENTS.md`**. It's the source of truth. Keep it updated as you work.
@AGENTS.md
