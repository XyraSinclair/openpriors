# CLAUDE.md

This repo is in the fast direct-to-main collaboration set.

If the current checkout is clean before work:

```bash
git checkout main && git pull --rebase origin main
```

After a minimum good chunk of work:

```bash
git add <intentional-paths>
git diff --cached --quiet || git commit -m "<clear message>"
git pull --rebase origin main
git push origin HEAD:main
```

Guardrails:

- stage intended paths only
- do not force-push `main`
- do not use `git add -A` unless the whole worktree is intentionally yours
- if the current checkout is dirty with unrelated work, use a separate worktree
  or clean checkout
- if a rebase conflict is not trivial, stop and leave a clear note

Read **`AGENTS.md`**. It's the source of truth. Keep it updated as you work.
@AGENTS.md
