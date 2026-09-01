# WinThemeSwitcher

Automatically swap between two Windows 11 **themes** at local sunrise and sunset — macOS's auto-theme behavior, on Windows 11. Full theme swap (wallpaper + colors + light/dark mode), not just a DWORD toggle.

> **Signed releases since v0.3.0.** All published binaries are Authenticode-signed with a self-signed publisher cert (`CN=WinThemeSwitcher Self-Signed`, thumbprint `40E0D1EB58DAC255EB37E9D64FF34448E3D33D12`). v0.4.0+ releases also carry an **RFC 3161 DigiCert timestamp countersignature** so the signature survives the cert's 2036 expiration. On first install you'll need to trust the cert — see [Verifying the signature](#verifying-the-signature). Architecture details and build/sign workflow are in [CLAUDE.md](CLAUDE.md).

## Features

- **Tiny binary** (~330 KB), zero CPU between transitions — event-driven, sleeps on a kernel timer until the next sunrise/sunset or a tray click.
- **Reliable theme apply** via the `IThemeManager2` COM interface — the same API the Settings UWP wraps internally. Atomic, in-process, ~200 ms latency, no Settings flash. Two-tier fallback if it ever errors.
- **Catches up after sleep / lock.** A scheduled sunrise that fires while you're suspended reconciles the moment you log back in.
- **Respects manual overrides** — changing theme in Settings (or via the tray's **Toggle Theme**) sticks until the next natural sunrise/sunset transition, surviving lock/unlock and sleep/resume. The app only steps in when a transition actually passed while you were away.
- **Recovers from failed applies** — a transition whose apply errors is retried up to 3 times a minute apart (instead of silently waiting for the next transition), and stands down if you change the theme yourself in the meantime.
- **Diagnostic log** at `events.log` next to the exe (rotated past 256 KB) — every transition recorded with cause, target, applied tier, and timing.

## Install

1. Download `win-theme-switcher-vX.Y.Z-windows-x64.zip` from the [latest non-prerelease release](../../releases) — currently [v0.4.0](../../releases/tag/v0.4.0), because v0.4.0 is marked `prerelease: true` (this changes in v0.5.0 — see the [Roadmap](#roadmap) v0.5.0 row, sub-bullet 1d). Extract to e.g. `C:\Tools\WinThemeSwitcher\`.
2. **Trust the publisher cert** (one-time, recommended — the zip includes `WinThemeSwitcher-publisher.cer`):
   ```powershell
   Import-Certificate -FilePath .\WinThemeSwitcher-publisher.cer -CertStoreLocation Cert:\CurrentUser\Root
   Import-Certificate -FilePath .\WinThemeSwitcher-publisher.cer -CertStoreLocation Cert:\CurrentUser\TrustedPublisher
   ```
3. Run `win-theme-switcher.exe`. A half-orange / half-dark-blue circle appears in your notification area.

On first launch the app reads your coordinates via Windows Location. If Location is off or denied, a dialog asks if you want to enable it (opens Settings) or fall back to manual entry (opens `config.json` in your editor). After editing config, right-click tray → **Refresh**.

## Configuration

`config.json` lives next to the exe (deliberately, so the app works from wherever you put it).

```json
{
  "latitude": 40.7128,
  "longitude": -74.0060,
  "auto_start": true,
  "theme_day": null,
  "theme_night": null
}
```

| Field | Default | Meaning |
|---|---|---|
| `latitude` / `longitude` | from Windows Location, else `0.0` | Decimal degrees. `0.0, 0.0` triggers the first-run location flow. |
| `auto_start` | `true` | When `true`, registers `HKCU\...\Run\WinThemeSwitcher`; when `false`, removes the entry. Applied on every launch and on Refresh. |
| `theme_day` | `null` → `%SystemRoot%\Resources\Themes\aero.theme` | Path to the `.theme` applied after sunrise. |
| `theme_night` | `null` → `%SystemRoot%\Resources\Themes\dark.theme` | Path to the `.theme` applied after sunset. |

Custom `.theme` paths must use double backslashes in JSON: `"C:\\Users\\you\\AppData\\Local\\Microsoft\\Windows\\Themes\\Custom.theme"`. They also need to be already registered with Windows (i.e. installed once via Settings → Personalization → Themes) for the primary apply path to find them.

After editing config, right-click tray → **Refresh**. No restart needed.

## Tray menu

- **Toggle Theme** — flips light/dark right now, as a manual override: it sticks (including across lock/unlock and sleep) until the next natural sunrise/sunset transition.
- **Open Config** — opens `config.json` in your default editor.
- **Refresh** — re-reads config, retries Windows Location if needed, force-applies the correct theme.
- **Quit** — exits. The auto-start entry persists; set `auto_start: false` and click Refresh (or relaunch) once to remove it.

If two copies are launched, the second shows a notice and exits (single-instance). Fatal startup errors show a dialog and are recorded in `events.log`, as are panics.

## Verifying the signature

```powershell
Get-AuthenticodeSignature .\win-theme-switcher.exe | Format-List Status, SignerCertificate
```

Should return `Status: Valid` and `Signer: CN=WinThemeSwitcher Self-Signed`. The signature lets you verify the file was signed by this project's publisher key and hasn't been tampered with since signing. Self-signed certs can't suppress SmartScreen — a CA-signed cert reduces prompts over time as reputation accrues (v0.6.0 row in [Roadmap](#roadmap)).

## Antivirus false positives

**Heads-up: only build with `scripts\build.ps1`.** Bare `cargo build --release` produces an unsigned fresh-hash PE that KSN flags on first execute — see [Building from source](#building-from-source) for the canonical flow. The rest of this section is for users running the **already-signed published binaries**, not for source-builders.

---

The v0.4.0 release ships Authenticode-signed binaries (RSA + SHA256, self-signed publisher cert `CN=WinThemeSwitcher Self-Signed`, thumbprint `40E0D1EB58DAC255EB37E9D64FF34448E3D33D12`). On first install, import the included `WinThemeSwitcher-publisher.cer` into `Cert:\CurrentUser\Root` (and optionally `Cert:\CurrentUser\TrustedPublisher` for SmartScreen) — see [Verifying the signature](#verifying-the-signature). After that, **most AVs accept the binary without any further action**.

**Kaspersky is the exception.** Its Behavior Detection scores cumulative signal — `HKCU\Run` writes, `HWND_BROADCAST` / `WM_SETTINGCHANGE` / `WM_THEMECHANGED` propagation, WinRT Geolocation, COM activation of `themeui.dll`. v0.4.0's tier-1 `IThemeManager2` apply removes the broadcast signals (the COM interface does its own broadcast from inside `themeui.dll`, where the heuristics trust it), but the persistence + Geolocation signals remain. For self-signed-cert releases (v0.4.0), Kaspersky still needs a **Trusted application** rule (Settings → Security → Threats and Exclusions → *Specify trusted applications* → tick all five checkboxes: Do not scan opened files, Do not monitor application activity, Do not inherit restrictions, Do not monitor child application activity, Allow interaction with Kaspersky interface). **The binary is not malicious** — full source is in this repo. A CA-signed cert (planned for v0.6.0, via SignPath Foundation's free OSS program) collapses this further and removes the need for the Trusted-app rule entirely.

If your AV quarantines the published signed binary anyway, restore it and add the rule before re-running.

## How it works

`apply_theme` is a three-tier fallback:

1. **`IThemeManager2`** (primary) — the undocumented-but-stable COM interface in `themeui.dll` that the Settings UWP wraps internally. Atomic, in-process apply. `SetCurrentTheme(idx)` does the `WM_THEMECHANGED` + `WM_SETTINGCHANGE` broadcasts itself.
2. **`ShellExecuteW(.theme)` + commit watcher** — legacy backup if the COM interface ever errors. A 5 s watcher polls the registry to detect silent failures and promotes to tier 3.
3. **Direct registry write** — last resort. Flips light/dark mode but not wallpaper.

Sunrise/sunset times come from the [`sun-times`](https://crates.io/crates/sun-times) crate (no network). The event loop blocks on `winit`'s `WaitUntil(next_transition)` between transitions — zero CPU. A worker thread catches `WTSRegisterSessionNotification` (unlock) and `PowerRegisterSuspendResumeNotification` (resume from sleep) so transitions reconcile after long suspends.

Full architecture, threading invariants, and the reasoning behind each tier are in [CLAUDE.md](CLAUDE.md).

## Building from source

Requirements: Rust `stable-x86_64-pc-windows-msvc` + Visual Studio Build Tools with the C++ workload + Windows SDK (for `signtool.exe`).

```powershell
winget install Rustlang.Rustup
winget install Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
git clone https://github.com/atefalshehri/WinThemeSwitcher.git
cd WinThemeSwitcher
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build.ps1            # build + sign + deploy to C:\Tools\
# or, for verification only (does not overwrite the installed binary):
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build.ps1 -SkipCopy
```

**Do not run `cargo build --release` directly.** It produces an unsigned fresh-hash PE that KSN flags as `VHO:Trojan.Win32.Convagent.gen` on this machine — that's the failure mode that delayed v0.4.0 by six weeks (see the v0.4.0 row in [Roadmap](#roadmap)). `scripts\build.ps1` signs the binary with the project's Authenticode cert + RFC 3161 DigiCert countersignature **before** anything executes it. The same caveat applies to `cargo test` — use `scripts\test.ps1`.

Release profile is tuned for size (`opt-level = "z"`, `lto = true`, `strip = true`, `panic = "abort"`). No `build.rs` — `windows-sys` and `windows` self-link. Output ~330 KB. Cert + signing details live in [CLAUDE.md → Sign every release build](CLAUDE.md#sign-every-release-build).

## Uninstall

1. Right-click tray → Quit.
2. Delete the install folder.
3. Remove auto-start:
   ```cmd
   reg delete "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" /v WinThemeSwitcher /f
   ```

No installer, no uninstaller — it's a single-exe tool by design.

## Roadmap

Ordered by priority. The Aug–Sep 2026 audit's confirmed correctness bugs shipped as fixes in v0.3.x–v0.4.0; release/distribution work is next.

### Release plan

Versioning before 1.0: a **patch** (0.x.y → 0.x.y+1) is pure bug fixes; a **minor** (0.x → 0.x+1) is anything that adds surface (menu items, config fields) or changes behavior. v1.0 is a gate, not a feature drop. Numbers are assigned at ship time — the 0.6/0.7 order can swap if the SignPath approval wait stalls, and regression patches (e.g. 0.4.1) slot in anywhere.

| Version | Type | Contents |
|---|---|---|
| **v0.3.2** | patch | **Shipped 2026-07-04.** Sunrise/sunset day-bracketing fix + scheduling tests + CI gate; `config.json` never overwritten on a parse error (error is logged + shown in a non-blocking dialog, empty file self-heals, autostart setting survives a broken file) |
| **v0.4.0** | minor | **Shipped 2026-09-01.** Preserve manual overrides across lock/unlock/resume (time-based reconciliation — overrides survive any-length sleeps; missed transitions still reconcile); "Toggle Theme" tray item; fail-loudly bundle (panic hook, fatal-error MessageBox, wake-listener logging + WTS registration retry, single-instance mutex, bounded apply retry with user-intervention stand-down); `.theme`-name resolution + tick-decision unit tests (40 total); **`scripts\test.ps1` + new `scripts\build.ps1`** (signed test binary; signed release exe with **RFC 3161 DigiCert countersignature** before first execution — closes the KSN `VHO:Trojan.Win32.Convagent.gen` first-seen flag vector); **CLAUDE.md agent-facing rule** to keep LLM coding agents on the signing wrappers (the root cause of this release slipping six weeks past v0.3.2 was bare `cargo build` producing unsigned exes that KSN locked on first execute) |
| **v0.4.1** | patch | **TBD — current state is clean (40/40 tests, 0 warnings). Slot reserved for the next audit.** If nothing surfaces, this row stays at "did not ship" — the table allows patch releases to slot in anywhere. |
| **v0.5.0** | minor | **Automate the release pipeline.** `scripts\release.ps1` wraps `build.ps1` + `gh release upload --clobber` + `gh release edit --prerelease=false`; `release.yml` either calls the wrapper on a self-hosted runner or stops generating assets; a verify step (re-check `Get-AuthenticodeSignature` + countersignature post-upload) gates the upload. **First non-prerelease release** since v0.2.0 — fixes `/releases/latest` and unblocks winget automation. Also: investigate the recurring `wake_listener_err stage=power_register code=87` and either fix it (different `DEVICE_NOTIFY_*` flag? explicit unregister-before-register?) or add a CLAUDE.md note that it's benign. Candidate point to launch the personal Scoop bucket (`persist` mechanism fits the exe-relative config model better than winget's symlink layout — see §Release & distribution §4). |
| **v0.6.0** | minor | SignPath Foundation CA signing in CI (ends the manual signed-asset swap); submit the first CA-signed binary to Microsoft Defender + Kaspersky for reputation seeding. **Removes the §Antivirus false positives section from README** — Kaspersky heuristics no longer fire on first sight for CA-signed binaries, so the user-facing allowlist flow becomes historical. Also unlocks the winget submission (§Release & distribution §3) — winget's validation pipeline runs its own AV scans, and a CA-signed + Defender-submitted binary is what gets through. |
| **v0.7.0** | minor | Live tray tooltip ("Dark until 06:12", "Location needed — click Refresh", or a degraded-apply warning); "Open Log" menu item; MessageBox when a user-initiated Refresh fails (scheduled ticks stay silent-to-log); first-run location retry without requiring Refresh; `offset_sunrise_min` / `offset_sunset_min` config fields (sun-anchored, so still compatible with "no custom times") |
| **v1.0.0** | gate | Cut when **all** of: winget accepts the package, the §Correctness fixes and §Foundation sections below are empty, a full release cycle has shipped CA-signed with no AV flags, **§Antivirus false positives is deletable from README**, the recurring `wake_listener_err power_register code=87` is either fixed or explicitly documented as benign in CLAUDE.md, and the README's §Contributing moves from "Alpha / personal-use tool first" to "Stable for personal + distribution". Net effect: a stranger can install WinThemeSwitcher without reading the Kaspersky section. |

### Correctness fixes

Bugs that shipped in a named release. A reader auditing a version can scan this section to see what the version fixed; an empty section means "no known correctness work since the last audit".

- **v0.4.0 — Override-fight regression.** Pre-v0.4.0, the event loop called `tick()` on every event — including the `WM_SETTINGCHANGE` broadcast that fires when the user changes theme in Settings. The override-aware `decide_tick` didn't exist yet; the old "screen-mismatches-target → apply" logic won and the user's manual selection was reverted on the next event. v0.4.0 scopes `tick()` to `Init` / `ResumeTimeReached` / `Refresh` / `Wake` only — Settings broadcasts don't fire on any of those. Coverage: `matching_current_skips_apply` + `wake_before_next_transition_preserves_override`.
- **v0.4.0 — Stale-baseline risk on apply failure.** A failed apply (e.g. COM error from a transient themeui.dll state) used to look identical to a user override on the next wake tick: the wake saw `current != target`, the previous `reconciled_next` was recorded as a successful apply, and `decide_tick` returned `SkipOverride` — stranding the screen on the wrong theme until the next natural transition. v0.4.0's fix: `note_apply_failed` deliberately leaves `reconciled_next` stale on `Err`, so wakes re-apply instead of misreading the failure as an override. The `retry_baseline` field gates stand-down: if the user intervened mid-episode, the screen has moved off the failure snapshot and the retry cancels. Coverage: `failed_apply_then_wake_must_reapply_not_preserve`, `user_intervention_during_retry_window_stands_down`, `retry_budget_is_bounded_and_resets_per_episode`.
- **v0.3.2 — Empty `config.json` crash-on-launch.** A crash mid-write (Rust's `fs::write` truncates before writing) leaves a 0-byte file; pre-v0.3.2, this errored on every launch and the user had to delete the file manually. v0.3.2 fix: empty/whitespace files self-heal to defaults (nothing to preserve, so first-run semantics apply). Coverage: `config_empty_file_is_healed_to_defaults`.

### Foundation

- **Remaining known gaps** (accepted for now, documented in CLAUDE.md): the bounded apply retry covers total apply failure only — a tier-2 `ShellExecute` silent-fail is still recovered by `commit_watcher`'s registry fallback, not the retry counter; and a Toggle within ~5 s of a tier-2 apply can be reverted by that apply's still-running commit watcher (unreachable while tier 1 is healthy).
- **`wake_listener_err stage=power_register code=87` fires on every launch.** `PowerRegisterSuspendResumeNotification` returns `ERROR_INVALID_PARAMETER` (87) on a fresh STA thread for reasons that aren't pinned down (likely an undocumented requirement around the hwnd being a true message-only window, or a NULL pointer where a previous registration is expected). The source treats it as benign — the WTS notification still registers, so unlock events still fire; only the suspend/resume hook is degraded. But it logs every launch, clutters `events.log` rotation, and confuses anyone reading the log trying to diagnose real wake-listener failures. **Investigate in v0.5.0**: try an explicit unregister-before-register sequence, or test with `DEVICE_NOTIFY_CALLBACK` instead of `DEVICE_NOTIFY_WINDOW_HANDLE`. If no clean fix surfaces, document as benign in CLAUDE.md and add the code to a known-benign set in the test suite.

### Release & distribution (dependency chain, in order)

The release-pipeline hardening items that used to live as a single bullet are now broken out so each maps cleanly to the release plan above.

1. **Harden the release process (in progress — split across v0.4.0 + v0.5.0):**
   - **1a. Done in v0.4.0**: timestamp countersignature on every signed build (`/tr http://timestamp.digicert.com /td SHA256` — without it, signatures die when the cert expires in 2036). `scripts\build.ps1` produces the signed exe and deploys to `C:\Tools\…`. See the v0.4.0 row above.
   - **1b. v0.5.0**: `scripts\release.ps1` wraps `build.ps1` + `gh release upload --clobber` + `gh release edit --prerelease=false`. A verify step re-checks `Get-AuthenticodeSignature` post-upload and gates the workflow. Replaces the manual `cargo build` → manual `gh release upload` ritual that let the v0.3.0 "shipped unsigned for months" failure recur.
   - **1c. v0.5.0**: investigate the recurring `wake_listener_err stage=power_register code=87` (see §Foundation). Either fix or document as benign in CLAUDE.md.
   - **1d. v0.5.0**: stop marking releases `prerelease` — the v0.4.0 release is marked prerelease because GitHub's `release.yml` defaulted to `prerelease: true`; from v0.5.0 onward, the release script sets `prerelease: false` for stable versions. Fixes `/releases/latest` and unblocks winget automation.
2. **CA-signed releases** via [SignPath Foundation's free OSS program](https://signpath.org) — signing moves into CI, which also permanently eliminates the manual signed-asset swap. Reduces SmartScreen prompts over time via cert reputation (no cert eliminates them outright). Azure Trusted Signing is not an option: individual validation is US/Canada-only.
3. **Submit the first CA-signed binary to Microsoft Defender** — this gates winget, whose validation pipeline runs AV scans — and to Kaspersky. Repeat only if a specific release gets flagged.
4. **winget package** (`InstallerType: portable`), only after steps 1–2 make asset hashes final at publish time. A personal Scoop bucket may come earlier (v0.5.0 candidate): Scoop's `persist` mechanism fits the exe-relative config model better than winget's symlink layout.

### UX polish

- **Live tray tooltip** — "Dark until 06:12", "Location needed — click Refresh", or a degraded-apply warning — plus an **"Open Log"** menu item and a MessageBox when a user-initiated Refresh fails (scheduled ticks stay silent-to-log).
- **First-run without the Refresh step** — retry Windows Location a few times right after the enable-Location dialog is dismissed, instead of a `Geolocator::StatusChanged` subscription. Bigger optional variant: re-query location on unlock/resume so a traveler's coordinates don't go stale (today they are never re-read once set).
- **Optional sunrise/sunset offsets** (`offset_sunrise_min` / `offset_sunset_min` in config) — still sun-anchored, so compatible with "no custom times".

### Maintenance notes (not scheduled)

- **winit `ApplicationHandler` migration** — only when bumping to winit 0.31 (the pinned 0.30 merely deprecates `EventLoop::run`; nothing forces this today). Worth evaluating at that point: dropping winit for a plain Win32 message loop — the pattern already exists in the wake listener.

**Not planned**: GUI configuration (`config.json` + Refresh is the UX), custom wake times (sunrise/sunset is the whole point; offsets from them are fine), cross-platform (Windows only — macOS already has this natively), in-app update check (it would re-add the autorun+beacon AV-heuristic surface the `IThemeManager2` migration removed — winget/Scoop handle upgrades), pause/snooze toggle (a manual override already pauses until the next transition), and ADM-style scripting/hotkeys/battery rules (out of scope for a ~330 KB tray tool).

## Contributing

**v0.4.0 status: stable for personal use.** The Tray/apply/scheduling core is mature (40 unit tests, 0 warnings, deployed binary verified daily for months). Working toward distribution polish — winget submission (gated on v0.6.0's CA-signed cert), Scoop bucket (candidate for v0.5.0), and the §Antivirus false positives section's eventual retirement (also v0.6.0). Issues and PRs welcome — please open an issue to discuss larger changes before sending a patch. Focus areas: see [Roadmap](#roadmap).

## License

MIT — see [LICENSE](LICENSE).

## Acknowledgments

- [`sun-times`](https://crates.io/crates/sun-times) — local sunrise/sunset math.
- [`chrono`](https://crates.io/crates/chrono) — date/time handling; [`serde`](https://crates.io/crates/serde) + [`serde_json`](https://crates.io/crates/serde_json) — config persistence.
- [`tray-icon`](https://crates.io/crates/tray-icon) + [`winit`](https://crates.io/crates/winit) — tray icon and event loop.
- [`windows-sys`](https://crates.io/crates/windows-sys) + [`windows`](https://crates.io/crates/windows) — official Microsoft Rust bindings.
