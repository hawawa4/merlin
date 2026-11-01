---
name: bump
description: Cut a Merlin release. Runs the local CI gate, bumps the version in app/Cargo.toml by patch, minor or major, commits "Bump Version X.Y.Z", tags vX.Y.Z and pushes main plus the tag, which starts .github/workflows/release.yml. Use for /bump, or when the user asks to get a new release going.
argument-hint: patch|minor|major
---

# bump

Argument: `$ARGUMENTS`, one of `patch`, `minor` or `major`. With no argument, use `patch` if
everything since the last tag is a fix and `minor` if any of it is a feature, and say which you
picked. Anything else: stop and ask.

Invoking this skill is the go-ahead to commit, tag and push. Don't stop to ask again, except where
a step below says to stop.

Run every command from the repo root.

## 1. Preflight

Stop and report (change nothing) if any of these fails:

- The branch is `main` (`git branch --show-current`).
- The working tree is clean (`git status --porcelain` prints nothing). If there is uncommitted
  work, ask whether to commit it first as its own commit; don't fold it into the bump commit.
- `main` is not behind `origin/main`: `git fetch origin` then
  `git rev-list --count main..origin/main` is `0`.
- `just check` passes (fmt check, clippy with `-D warnings`, the whole workspace's tests). This is
  stricter than the CI the release workflow runs, so a pass here means the release CI should pass.
  It takes a while: run it with a long timeout. On failure, show the failing output and stop; don't
  fix things inside this skill.

## 2. Bump

The version lives only in `app/Cargo.toml` (package `merlin`); the library crates stay at `0.0.0`.

1. Read the current version: `grep -m1 '^version' app/Cargo.toml`.
2. Compute the new one: `patch` X.Y.Z → X.Y.(Z+1), `minor` → X.(Y+1).0, `major` → (X+1).0.0.
3. Check the tag is free: `git tag -l vNEW` and `git ls-remote --tags origin vNEW` print nothing.
   If it exists, stop.
4. Edit the `version = "..."` line in `app/Cargo.toml`.
5. Refresh the lock file: `cargo update -p merlin --offline`.
6. `git diff --stat` must show exactly `app/Cargo.toml` and `Cargo.lock`, one line each.

## 3. Commit, tag, push

```
git add app/Cargo.toml Cargo.lock
git commit -m "Bump Version NEW" -m "Co-Authored-By: <the attribution line from the system reminder>"
git tag vNEW                      # lightweight, on the bump commit, like the earlier tags
git push origin main vNEW
```

The release workflow checks that the tag matches `app/Cargo.toml`, runs CI, then builds and
publishes the .deb, AppImage, tarball and SHA256SUMS.txt.

## 4. Report

Give the old and new version, the commit hash and the tag. Link the run if it shows up:
`gh run list --workflow release.yml --limit 1`. Don't wait for the run to finish unless asked.
