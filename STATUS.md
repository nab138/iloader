# iloader (rebelancap fork) — STATUS

Branch: `visionos-tunnel` · version **2.3.5** · last updated 2026-09-14

## Current state

`visionos-tunnel` is rebased onto upstream `origin/main` (nab138/iloader
`348eefd`, 2026-09-10) and carries all 13 substantive Vision Pro commits plus the
re-done vendoring. Apple's GSA 503 block on `com.apple.dt.Xcode` client-info is
fixed: `src-tauri/vendor/isideload` is now an exact copy of the commit upstream
pins (`nab138/isideload@f6a4d5dba717d72fc2af63eaba26b27ba44116be`, branch
`apple-codesign-quick`, 0.3.17, containing a19f5f0's hardcoded
`<com.apple.AuthKit/1 (com.apple.akd/1.0)>`), overridden through
`[patch."https://github.com/nab138/isideload"]`. The only isideload patch we
still carry locally is the LiveContainer main-bundle certificate injection
(`ALTCertificate.p12` + `ALTCertificateID` + `ALTCertificatePassword`) — the
developer-error-35 "device already registered" tolerance is now upstream
verbatim. `vendor/idevice` is untouched at 0.1.65 (upstream's lock still resolves
0.1.65) and still carries the `awaitingUserConsent` real-pairing-code patch;
Cargo resolves a single idevice. Signed release build and DMG produced, all 5
unit tests pass, frontend typechecks. A real GSA sign-in reaches Apple and gets a
parsed plist error, not the HTML 503.

## Last round (2026-09-14, 2.3.5 release prep)

- `SIDESTORE_VP_URL` in `src-tauri/src/sideload.rs` repointed to
  `rebelancap/SideStore` release **`visionos-0.7.0`**; the doc comment now records
  the akd sign-in fix and the adi.pb reset requirement. LiveContainer URLs on the
  rolling `visionos` tag are unchanged.
- Version bumped 2.3.4 -> **2.3.5**. `bun run bump-patch` covers `package.json`,
  `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json` but runs with `--no-lock`,
  so `src-tauri/Cargo.lock`'s `iloader` entry was edited by hand — check it every
  bump.
- Signed release build succeeded (`bun run tauri build --target aarch64-apple-darwin
  --config src-tauri/ci.conf.json`); the bundle_dmg.sh hang did not recur. DMG
  copied to `build/iloader-visionOS-aarch64.dmg` (9.4 MB); `build/` is git-ignored.
- Release notes for the rolling `visionos` release drafted at
  `build/release-notes.md` (existing body plus a "What's new in 2.3.5" section).
- **BLOCKED: notarization.** `xcrun notarytool submit --keychain-profile iloader`
  fails with `Error: No Keychain password item found for profile: iloader`, and no
  `com.apple.gke.notary.tool` item is visible in the (unlocked, no-timeout) login
  keychain. `spctl -a -t open -vv build/iloader-visionOS-aarch64.dmg` therefore
  reports `rejected / source=Unnotarized Developer ID`. The DMG IS validly signed
  with `Developer ID Application: Austin Archibald (57G8J46Z2T)`. Austin must
  re-run `xcrun notarytool store-credentials iloader` (apple-id
  austin@archibalds.tv, team 57G8J46Z2T, app-specific password) before the DMG can
  be notarized, stapled and published. **Do not upload the DMG as-is.**
- Nothing pushed, nothing uploaded.

## Last round (2026-09-13, upstream sync)

- Committed the dirty Cargo.lock; safety copy at
  `backup/visionos-tunnel-pre-sync-2026-09-13` (old tip `d285365`).
- Rebased by cherry-picking the 13 substantive commits onto `origin/main`,
  dropping all 14 "Bump version to 2.2.x" commits. Only non-trivial conflict:
  `src-tauri/Cargo.toml` in the base VP commit (took upstream's git isideload
  dependency, kept our idevice 0.1.65 + `installation_proxy` feature, kept our
  vendor patch blocks). Every other conflict was the `iloader` version line in
  `src-tauri/Cargo.lock`.
- Re-vendored isideload 0.3.17 and re-applied the LiveContainer patch, adapted to
  0.3.17's synchronous `isideload_vfs::fs` file API.
- One API drift fix in our code: `sign_app` gained a `progress_callback`
  parameter (`src-tauri/src/sideload.rs`, pass `None`). Nothing else broke.
- Version 2.3.4 (one patch above upstream's 2.3.3; ours had been 2.2.21) in
  `package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`.
- Built `iloader_2.3.4_aarch64.dmg` signed with the Developer ID identity
  (not notarized — no release this round).
- Verified sign-in through a new `#[ignore]`d probe,
  `src-tauri/tests/gsa_signin.rs`: anisette provisioning succeeded, "Login step 1
  completed", then Apple's own `AuthWithMessage(-22406, "Enter the correct
  password for this Apple Account.")` using a deliberate placeholder password.
  Independently confirmed with curl that GSA `POST /grandslam/GsService2` returns
  503 for the Xcode client-info string and does not for the akd one.

## Last round (addendum, 2026-09-13 evening)

Fork 2.3.4 signed in to GSA for real (`Successfully logged in to Apple ID`, `Successfully retrieved app token`, no 503) and sideloaded ~/dev/sidestore/build/SideStore-visionOS.ipa (0.7.0) onto the Vision Pro over the RP tunnel: error-35 tolerance fired, InstallComplete, pairing file placed via Manage Pairing File. Sonnet review of the re-applied patches: no bugs.

## Next steps

1. **Restore the notarytool credential**: `xcrun notarytool store-credentials
   iloader` (Apple ID austin@archibalds.tv, team 57G8J46Z2T, app-specific
   password). Then, from `~/dev/iloader`:
   `xcrun notarytool submit build/iloader-visionOS-aarch64.dmg --keychain-profile
   iloader --wait` → `xcrun stapler staple build/iloader-visionOS-aarch64.dmg` →
   `spctl -a -t open -vv build/iloader-visionOS-aarch64.dmg` (expect
   `source=Notarized Developer ID`). The DMG needs no rebuild.
2. Create the `rebelancap/SideStore` release **`visionos-0.7.0`** with
   `~/dev/sidestore/build/SideStore-visionOS.ipa` — iloader 2.3.5 already points at
   that URL, so it 404s until the release exists.
3. Update the rolling `rebelancap/iloader` release `visionos`: body from
   `build/release-notes.md`, asset `iloader-visionOS-aarch64.dmg` (`--clobber`),
   only once notarized and stapled.
4. Update the `rebelancap/LiveContainer` `visionos` release with
   `~/dev/LiveContainer/build/*.ipa`; push the sidestore submodule branches
   (SideSign fb1a307, minimuxer eb67fe9) to the rebelancap forks.
5. Push `visionos-tunnel`. Austin decides whether 2.3.5 gets a GitHub release.

## Open questions

- Does 2.3.4 get a GitHub release, or does the next OTA/dev build become
  2.3.4.1? Default if unanswered: no release, nothing pushed; the branch just
  sits here.
- Should the LiveContainer main-bundle cert injection be offered upstream as a
  PR against `apple-codesign-quick`? Default: keep it vendored.

## Live claims
- Fork build **2.3.4** still running on the Mac from
  `src-tauri/target/aarch64-apple-darwin/release/bundle/macos/iloader.app` (another
  session is driving it); the on-disk bundle there is now the 2.3.5 rebuild, so the
  next launch of that path is 2.3.5.
- No simulators booted, no background agents, working tree clean.
- Waiting on Austin: notarytool credential (blocker above), and the on-headset
  SideStore 0.7.0 sign-in check (reset adi.pb with all boxes unchecked first).
