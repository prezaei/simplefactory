---
name: sync-main
description: Safely sync the local branch to match origin/main. Use when the user wants to sync to main, pull main, reset to main, update from main, get latest main, or sync their local branch to the latest remote state.
---

# Sync to Main

Safely synchronize the local branch to match `origin/main`. Handles dirty working trees and gives the user control over how to resolve local changes.

## Workflow

1. Run `git fetch origin` to get the latest remote state.

2. Check for uncommitted changes with `git status --short`.

3. **If the working tree is dirty**, ask the user:
   - **Hard reset** — discard all local changes and match `origin/main` exactly (`git reset --hard origin/main`)
   - **Stash and reset** — stash changes, reset to `origin/main`, then remind user they can `git stash pop` later
   - **Abort** — do nothing

4. **If the working tree is clean**, run `git reset --hard origin/main`.

5. Confirm final state:
   ```bash
   git log --oneline -5
   git status --short
   ```

6. Report the result: which commit HEAD points to and whether the tree is clean.

## Notes

- Default to hard reset when the user says "sync to main" without qualification.
- Never use `git merge` unless the user explicitly asks for a merge.
- If on a feature branch, warn the user that this will move their branch pointer to main. Suggest `git checkout main` first.
