# CLAUDE.md

## Releases

Releases follow [docs/release.md](docs/release.md); never tag by hand.

Scope: this repo only (Viper Mixer). A bump or release request never covers the other Viper repos (such as vipermusic), even if the request says "both" or "all repos"; each gets its own request.

- Every push to `main` runs CI, and when it passes, the Release workflow checks whether a commit since the last tag has a `Changelog:` paragraph other than `none`. If one does, it puts the next version and its notes on the `release` branch and keeps the "Release vYY.M.N" pull request up to date. Merging that pull request releases it: the run after the merge tags it, builds the AppImage, and creates the GitHub release. So "push" never releases, and you never bump the version.
- "Release" (or "merge the release") means `gh pr merge release --merge`, only ever with a merge commit, never squash or rebase.
- Only "release now" (or "bump ver", "bump and release", "push and release") asks for a release that can't wait: commit and push the work, then run `scripts/release.sh bump` (or `scripts/release.sh YY.M.N` when the user names a version) in the foreground. It counts the version from the newest tag, runs Clippy, writes and checks the notes, sets the version, commits `Release vYY.M.N`, tags, and pushes, asking nothing. Include the notes it prints and the run's link in your reply, then end the turn.
- Versions are CalVer `YY.M.N`: the two-digit year, the month, and which release of that month (26.11.4 → 26.11.5, then 26.12.0 in a new month). Don't work a number out yourself: `scripts/release.sh next` prints it.
- Never watch GitHub Actions runs unless asked; `scripts/release.sh watch YY.M.N` follows one.
- Never move, reuse, or force-push an existing tag. If the tag already exists, stop and ask.

## Commits

- Commit subjects are short, sentence-case, and imperative, with no prefix, and describe the user-visible change.
- Every commit that changes the app needs a paragraph in its message starting with `Changelog:` and its group (Added, Changed, Fixed, Removed, or Security), written for users by the rules in `docs/changelog-style.md`, for example "Changelog: Fixed: A strip keeps its app after the app restarts, so its fader works again without reassigning it." A change users can't notice gets `Changelog: none`. The `commit-msg` hook (`git config core.hooksPath scripts/hooks`) enforces both; if it turns a commit down, fix the message rather than bypassing the hook. These paragraphs are the release's notes word for word, so a follow-up to a change in the same release gets `Changelog: none`.
- Keep internal CI, release, and tooling changes out of the changelog: commit them separately so they only touch `.github/`, `scripts/`, `docs/`, or `*.md`, which need no `Changelog:` paragraph. If one has to touch app files, write `Changelog: none`.
- Do not edit `CHANGELOG.md` by hand, except to fix notes already released: edit that release's section, commit it, and run `scripts/release.sh notes <version>`.

## Development

- Check with `cargo clippy --all-targets` and `cargo test`. CI runs both.
- Look at the window without touching the desktop: `scripts/screenshot.sh .scratch/shot.png SCENE` opens `--demo SCENE` in a headless KWin with its own D-Bus session and saves the window; the scenes are `SCENES` in `src/ui.rs` (`default`, `assign-menu`, `midi-menu`, `learning`, `update`, `disconnected`). Never run Spectacle against the real session. Write shots under `.scratch/`, which git ignores.
- The window is GPUI from crates.io. `Cargo.lock` holds `libc` at 0.2.189 because GPUI's `xattr` uses `ENOATTR`, which later `libc` dropped; keep it there when updating (`cargo update -p libc --precise 0.2.189`).
- Keep dependencies few. `futures`, `png` and `serde_json` are already in GPUI's tree. Add a crate only when the standard library or an existing dependency can't do the job.
- `assets/logo.png` and `assets/tray.png` are rendered from `assets/logo.svg`; the README says how.
