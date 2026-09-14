# iloader (rebelancap fork) — STATUS

Branch: `visionos-tunnel` · version **2.3.4** · last updated 2026-09-13

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

## Next steps

1. Full sign-in with the real password for `austin@archibalds.tv` (keychain
   service `iloader`), either in the app UI or
   `ILOADER_TEST_PASSWORD=… cargo test --test gsa_signin -- --ignored --nocapture`.
   Expect a 2FA prompt.
2. Resolve the Local Network permission state for the rebuilt app: on launch
   2.3.4 logged `local-network probe: REFUSED (No route to host (os error 65))`
   and discovered no headset in 20s, while `dns-sd -B _remotepairing._tcp` from
   the shell sees two devices. Allow/toggle iloader in Settings ▸ Privacy &
   Security ▸ Local Network and relaunch before any pairing test.
3. End-to-end Vision Pro round: pair, then sign + install the SideStore IPA a
   separate agent is producing, and confirm LiveContainer picks up the injected
   certificate with no user interaction.
4. Nothing is pushed and no release exists for 2.3.4 — Austin decides if/when.

## Open questions

- Does 2.3.4 get a GitHub release, or does the next OTA/dev build become
  2.3.4.1? Default if unanswered: no release, nothing pushed; the branch just
  sits here.
- Should the LiveContainer main-bundle cert injection be offered upstream as a
  PR against `apple-codesign-quick`? Default: keep it vendored.

## Live claims

None. No simulator lane held, no booted devices, no background agents, working
tree clean on `visionos-tunnel`. The test-launched 2.3.4 `.app` was quit; the
separately installed `/Applications/iloader.app` (older build) may still be
running from before this session.
