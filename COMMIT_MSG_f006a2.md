fix(ci): use tauri-action for NSIS+updater build so latest.json is generated

The hand-rolled `pnpm tauri build` + manual zip/SHA256SUMS/attach flow
produced the updater signature (-setup.exe.sig) but never generated
latest.json (softprops glob found no latest.json; tauri bundler run
locally reproduced: sig yes, latest.json no). Tauri updater artifacts
are produced by tauri-bundler via bundle_project; the latest.json
manifest generation path proved unreliable via bare CLI runs in this
environment, while the community-verified pattern is the pinned
tauri-action (action-v0.6.2) with updaterJsonPreferNsis.

Release v0.1.0-beta.1 currently published without latest.json -> updater
feed incomplete. Rebuild via action, re-attach, re-verify.

