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
- `main` is not ahead of `origin/main` either: `git rev-list --count origin/main..main` is `0`.
  Unpushed work must go out (and through CI) on its own before the bump; see the next point.
- GitHub CI is green on the exact commit being released. A local pass is not enough: v0.13.0
  failed because CI's toolchain differed from the local one. Check the `ci` run for HEAD:
  `gh run list --workflow ci.yml --branch main --commit "$(git rev-parse HEAD)" --limit 1 --json status,conclusion,url`.
  - `completed` + `success`: go on.
  - Still queued or running: wait for it (`gh run watch <id> --exit-status`), then re-check.
  - No run for HEAD (it was never pushed): push `main` alone first (`git push origin main`; this
    is covered by the release go-ahead), wait for that run, then re-check.
  - Anything else (failure, cancelled): show the failing log (`gh run view <id> --log-failed`)
    and stop. Never tag on top of a red or missing CI run.
- `just check` passes (fmt check, clippy with `-D warnings`, the whole workspace's tests). This is
  stricter than the CI the release workflow runs (it includes the `app` crate). The toolchain is
  pinned in `rust-toolchain.toml`, so local and CI run the same rustc and clippy; don't override it
  (`+stable`, `RUSTUP_TOOLCHAIN`). It takes a while: run it with a long timeout. On failure, show
  the failing output and stop; don't fix things inside this skill.

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

## 4. Watch the release through

A release is done when its assets are published, not when the tag is pushed. Always wait:

1. Find the run: `gh run list --workflow release.yml --branch vNEW --limit 1 --json databaseId,url`
   (it can take a few seconds to appear).
2. `gh run watch <id> --exit-status` (long timeout; the build takes about 5 minutes).
3. On success, confirm the release has all four assets the updater looks for:
   `gh release view vNEW --json assets --jq '.assets[].name'` must list
   `merlin_NEW_amd64.deb`, `merlin-NEW-linux-amd64.AppImage`, `merlin-NEW-linux-amd64.tar.gz` and
   `SHA256SUMS.txt`.
4. On failure, show `gh run view <id> --log-failed`, tell the user the release did not ship, and
   fix it as a new patch release (don't move or reuse a pushed tag).

## 5. Report

Give the old and new version, the commit hash, the tag, the run link and the published assets.
