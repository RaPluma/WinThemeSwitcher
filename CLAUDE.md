# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Windows tray app (Rust) that swaps the full Windows **theme** (wallpaper + colors + light/dark mode) at local sunrise/sunset — macOS's auto-theme behavior, on Win11. Primary apply path is the `IThemeManager2` COM interface (the same one the Settings UWP wraps internally) for atomic, in-process theme apply; a two-tier fallback (legacy `ShellExecute(.theme)` → registry-only DWORD toggle) handles the case where the COM interface errors. ~330 KB single-exe, signed Authenticode, no installer.

Roadmap, per-version release plan, and the patch-vs-minor versioning rules live in README.md → Roadmap. Next up is v0.4.0 (manual-override preservation across wake, "Toggle theme" tray item, fail-loudly bundle).

## Source tree vs deployed binary — read first

The source tree (`C:\Users\atef\Documents\Projects\WinThemeSwitcher\`) is kept for future tweaks. **The actually-running binary lives elsewhere**:

```
C:\Tools\WinThemeSwitcher\
├── win-theme-switcher.exe   ← auto-starts at login
└── config.json              ← user's Riyadh coords, auto_start: true
```

`HKCU\Software\Microsoft\Windows\CurrentVersion\Run\WinThemeSwitcher` points at `"C:\Tools\WinThemeSwitcher\win-theme-switcher.exe"`. After any rebuild, copy the fresh binary over the `C:\Tools\` one — otherwise the next login still launches the old build:

```powershell
Get-Process -Name win-theme-switcher -EA SilentlyContinue | Stop-Process -Force
Copy-Item `
  "C:\Users\atef\Documents\Projects\WinThemeSwitcher\target\release\win-theme-switcher.exe" `
  "C:\Tools\WinThemeSwitcher\win-theme-switcher.exe" -Force
Start-Process "C:\Tools\WinThemeSwitcher\win-theme-switcher.exe"
```

(The pre-Cargo Manus prototype exe that used to sit at the repo root has been deleted; `/win-theme-switcher.exe` stays in `.gitignore` so it can't be re-committed.)

## Kaspersky false positive — critical context

**Resolved as of the IThemeManager2 + code-signing migration** — both root causes of the AV friction were addressed simultaneously. The signed binary (`CN=WinThemeSwitcher Self-Signed` cert trusted via `Cert:\CurrentUser\Root`) collapses the Authenticode-trust signal, and tier-1 theme apply via `IThemeManager2::SetCurrentTheme` removes the `HWND_BROADCAST WM_SETTINGCHANGE` + direct `WM_THEMECHANGED` signals that previously tripped behavior heuristics. **Sign every release build** (see Build section below) — unsigned builds will resurrect the issue. Everything below is preserved as historical context for unsigned-build scenarios; the current signed build should not need any of it.

**Dev/test builds (2026-08-03):** KSN flagged fresh unsigned *test* binaries in `target\debug\deps\` (`VHO:Trojan.Win32.Convagent.gen`) — the trust rules above are path-based and don't cover them, and KSN's scanner locks each fresh exe faster than a post-build signtool can run. Two-layer fix now in place: (1) a Kaspersky **exclusion on the whole `target\` folder** (Settings → Security settings → Threats and Exclusions → Manage exclusions), added by the user; (2) `scripts\test.ps1` builds the test binary, signs it from the cert store, and only then executes it — **run tests via this script, not bare `cargo test`**, so the first execution KSN ever sees carries a valid signature.

### Historical: pre-signing trust setup

The unsigned binary tripped `VHO:Trojan.Win32.Agent.gen` (Rust exe with no Authenticode signature + `HKCU\Run` writes + `HWND_BROADCAST` of `WM_SETTINGCHANGE` + direct `WM_THEMECHANGED` to `Shell_TrayWnd` + WinRT Geolocation = every AV heuristic signal). Plain **path-based exclusions were insufficient** — Kaspersky's Behavior Detection quarantined regardless. The pre-signing workaround was a **Trusted Applications rule** (Kaspersky Settings → Security → Threats and Exclusions → Specify trusted applications) with all checkboxes ticked: Do not scan opened files, Do not monitor application activity, Do not inherit restrictions, Do not monitor child application activity, Allow interaction with Kaspersky interface.

Rules are **path-based**, so two currently exist:
1. `C:\Tools\WinThemeSwitcher\win-theme-switcher.exe` (the deployed binary — stable).
2. `C:\Users\atef\Documents\Projects\WinThemeSwitcher\target\release\win-theme-switcher.exe` (the build output — rewritten by each `cargo build`).

If a future rebuild gets quarantined anyway (the hash changes and Kaspersky occasionally re-evaluates): drop a 0-byte placeholder at the path first (`Set-Content -Path ... -Value "" -Encoding Byte -Force`), re-add the trust rule while the placeholder exists, then rebuild. Same trick works for new deployment paths.

### When the trust rule is not enough (KSN cloud verdict)

Trusted Applications rules cover File Anti-Virus + Behavior Detection but **not Kaspersky Security Network (KSN) cloud reputation**. KSN can independently flag a fresh hash and pop a hostile two-button modal — *"Disinfect and restart"* / *"Try to disinfect without computer restart"* — with no Skip / Esc / X dismiss option. Both buttons quarantine. Adding a Threats and Exclusions entry mid-modal does **not** clear the in-progress verdict — Kaspersky finishes quarantining anyway, and even subsequent rebuilds can be flagged by `svchost.exe` (the indexer-style scanner running as `NT AUTHORITY\NETWORK SERVICE`) before the popup re-appears.

The reliable workaround for a deploy session is to **right-click the tray K → Pause protection → 15 minutes**, then immediately copy + launch within that window. Once the process is loaded into memory it survives even after protection resumes (Windows holds the file handle; on-disk re-detection won't kill the running PID). The autostart re-creation in `set_auto_start(true)` runs each launch, so even if Kaspersky deletes the `HKCU\Run` value during a quarantine event, the next launch restores it.

**Both long-term fixes are now in place** — see the resolution note at the top of this section. Code-signing landed via `New-SelfSignedCertificate` + signtool (cert in My + Root; chain trust comes from Root — see Build section). Tier-1 theme apply landed via the `IThemeManager2` COM interface (CLSID `{9324da94-50ec-4a14-a770-e90ca03e7c8f}`). The legacy paths and the trust-rule + KSN documentation in this section are kept for the contingency where the cert expires, the signtool step is skipped, or someone strips the signature.

## Build

**Always build via `scripts\build.ps1`** — it produces, Authenticode-signs (with an RFC 3161 DigiCert timestamp), and deploys the release exe in one step. Bare `cargo build --release` produces a fresh-hash unsigned Windows PE; KSN flags first-seen unsigned exes with `VHO:Trojan.Win32.Convagent.gen` on this machine, and the only fix is signing before the binary ever executes. The same caveat applies to `cargo test` — use `scripts\test.ps1` (see Test section below).

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File "C:\Users\atef\Documents\Projects\WinThemeSwitcher\scripts\build.ps1"            # build + sign + deploy
powershell -NoProfile -ExecutionPolicy Bypass -File "C:\Users\atef\Documents\Projects\WinThemeSwitcher\scripts\build.ps1" -SkipCopy   # build + sign only, leave C:\Tools\ untouched
```

For raw cargo (debugging a compile error only — never produces a deployable exe):

```powershell
& "$env:USERPROFILE\.cargo\bin\cargo.exe" build --release `
    --manifest-path "C:\Users\atef\Documents\Projects\WinThemeSwitcher\Cargo.toml"
```

Default toolchain is `stable-x86_64-pc-windows-msvc` (MSVC Build Tools required; the GNU toolchain's bundled linker/dlltool was broken on this machine). Release profile: `opt-level = "z"`, `lto = true`, `codegen-units = 1`, `panic = "abort"`, `strip = true`. Output ~330 KB. No `build.rs` — `windows-sys` and `windows` self-link.

> **For Claude Code / Fable 5 / any LLM coding agent working in this repo**: do not invoke `cargo build` or `cargo test` directly. Always call `scripts\build.ps1` (for a release) or `scripts\test.ps1` (for tests). The wrappers exist *because* agents forget to sign.

### Test and lint

**On this machine, run the suite via the signing wrapper** (bare `cargo test` produces an unsigned fresh-hash exe that Kaspersky/KSN may lock or quarantine — see the Kaspersky section):

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File "C:\Users\atef\Documents\Projects\WinThemeSwitcher\scripts\test.ps1"           # full suite
powershell -NoProfile -ExecutionPolicy Bypass -File "C:\Users\atef\Documents\Projects\WinThemeSwitcher\scripts\test.ps1" riyadh    # name filter
```

Lint (advisory in CI):

```powershell
$cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
$manifest = "C:\Users\atef\Documents\Projects\WinThemeSwitcher\Cargo.toml"
& $cargo fmt --manifest-path $manifest --check
& $cargo clippy --release --manifest-path $manifest -- -W clippy::all
```

### Sign every release build

Done automatically by `scripts\build.ps1` (see Build section above). Manual sign — for rebuilding a tagged release's assets or troubleshooting — uses the cert from the store:

```powershell
& "C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\signtool.exe" sign `
    /n "WinThemeSwitcher Self-Signed" /fd SHA256 `
    /tr http://timestamp.digicert.com /td SHA256 `
    "C:\Users\atef\Documents\Projects\WinThemeSwitcher\target\release\win-theme-switcher.exe"
```

(Backup: the original pfx export still exists, but MSIX virtualization redirected it to the Claude desktop app's sandbox — `C:\Users\atef\AppData\Local\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Local\WinThemeSwitcher\signing\winthemeswitcher-signing.pfx`, password `wts-local-signing`. The literal `%LOCALAPPDATA%\WinThemeSwitcher\` path never existed outside the sandbox.)

This collapses the Kaspersky heuristic signal — signed builds pass without tripping the AV-pause dance documented above (KSN subsection). If the cert ever needs regenerating: `New-SelfSignedCertificate -Type CodeSigning -Subject "CN=WinThemeSwitcher Self-Signed" -KeyAlgorithm RSA -KeyLength 2048 -HashAlgorithm SHA256 -CertStoreLocation Cert:\CurrentUser\My -KeyExportPolicy Exportable -NotAfter (Get-Date).AddYears(10)` and re-add it to the Root store. The `/tr` timestamp countersignature (added 2026-07-04, free DigiCert TSA, needs network at sign time) keeps signatures valid after the cert expires. The v0.3.0 and v0.3.1 published assets were retro-timestamped in place the same day (`signtool timestamp /tr ... /td SHA256` on the already-signed exes, then re-uploaded and verified by fresh download); v0.1.0/v0.2.0 predate signing entirely, so there is nothing there to timestamp.

### CI and releases (`.github/workflows/`)

- `ci.yml` — on push/PR to main: `cargo build --release`, then `cargo test` (**hard gate**), then `cargo fmt --check` and `cargo clippy --release -- -W clippy::all`, the last two `continue-on-error` (advisory, not gates). Run them locally via the Test and lint section above. Tests live in `mod tests` at the bottom of `main.rs`: scheduling math with fixture locations (Apia/UTC+13 regression, Riyadh baseline, Reykjavik midnight-sunset, Tromsø polar), config loading (parse-error preservation, first-run defaults, empty-file self-heal, unknown-field tolerance), and a solar-altitude sanity check. All machine-independent — instants are constructed in UTC and config tests use per-test temp files — so they pass anywhere.
- `release.yml` — on tag push `v*` (or manual dispatch): builds on GitHub runners and attaches a zip (exe + README + LICENSE + publisher `.cer`) plus the bare exe and `WinThemeSwitcher-publisher.cer` to a **prerelease**. The `.cer` is committed at the repo root (public cert only — byte-identical to the store cert's export). **CI binaries are unsigned** — the signing key exists only on this machine, so every tagged release needs a manual post-tag step: build locally from the tag, sign (section above), zip (exe + the tag's README + LICENSE + `.cer`), then replace the workflow's assets with `gh release upload <tag> <files> --clobber`. Don't skip it: v0.3.0 originally shipped unsigned CI builds because this step was missed; the assets were replaced with signed builds on 2026-07-04, so all current v0.3.0 assets verify `Valid`.

## Architecture — `src/main.rs`

Single file, ~2250 lines (incl. `mod tests`), event-driven, no polling. Logs every state transition to `events.log` next to the exe (rotated to `events.log.old` past 256 KB).

### 1. Theme apply — three-tier fallback in `apply_theme`

Tiered worst-case-degradation: each tier is more invasive but less reliable than the one above. `apply_theme` walks them top-down, returning a `&'static str` tag for the tier that succeeded (logged in the `applied=` field of the `cause=...` line).

#### Tier 1: `IThemeManager2` (preferred — `applied=theme-manager2`)

Undocumented-but-stable COM interface in `themeui.dll` that the Settings UWP itself wraps. CLSID `{9324da94-50ec-4a14-a770-e90ca03e7c8f}`, IID `{c1e8c83e-845d-4d95-81db-e283fdffc000}`. Vtable layout in the `IThemeManager2Vtbl` struct at the top of `main.rs`.

Flow (`apply_via_theme_manager2`):
1. Resolve the target `.theme` file's `[Theme]\nDisplayName=...` value. System themes use SHLoadIndirectString-style refs (`@%SystemRoot%\System32\themeui.dll,-2060`); literal strings work too. `resolve_theme_display_name` parses the INI, `resolve_indirect_string` calls `SHLoadIndirectString`. For `dark.theme` → `"Windows (dark)"`; for `aero.theme` → `"Windows (light)"`.
2. `CoCreateInstance(CLSID_THEME_MANAGER2)` + `Init(0)`.
3. Enumerate via `GetThemeCount` + `GetTheme(i)` + `ITheme::GetDisplayName(&BSTR)` until a name match. Free each BSTR with `SysFreeString`. **Don't cache the index across launches** — enumeration order is not stable.
4. `SetCurrentTheme(NULL, idx, apply_now=1, apply_flags=NO_HOURGLASS, pack_flags=0)`. This is the only tier-1 call that applies; it does the WM_THEMECHANGED + WM_SETTINGCHANGE broadcasts internally.

Why this is the primary path: ShellExecuteW(`.theme`) silently fails when the user isn't actively interactive (post-WTS_SESSION_UNLOCK, ResumeTimeReached while away, scheduled while no foreground UI). The UWP activation pipeline swallows the apply request — Settings flashes briefly but never commits. `IThemeManager2` is in-process, has no UI dependency, and is what every serious tool uses (AutoDarkMode, wtheme, etc.). Apply latency is ~200 ms vs. the ~5 s poll-then-fail of the legacy path.

**STA threading is mandatory** for this interface ("Shell crap is always STA" per AutoDarkMode source). The main thread already calls `CoInitializeEx(None, COINIT_APARTMENTTHREADED)` at startup; tier-1 apply runs from winit event handlers on that same thread, which is correct. **Never call from a worker thread** without CoInitializeEx(STA) on it first — you'll get RPC_E_WRONG_THREAD or silent corruption.

#### Tier 2: `ShellExecuteW(.theme)` + `commit_watcher` (legacy — `applied=theme-file`)

Fires only if tier 1 errors out (logged as `theme_manager2_err target=… msg="…"`). Same as the original implementation: `ShellExecuteW("open", <.theme path>, ..., SW_HIDE)` to launch the Themes UWP, plus `start_settings_closer` thread to `PostMessage(WM_CLOSE)` the Settings window once it appears, plus a 300 ms sleep + `poke_shell` (taskbar repaint).

**`commit_watcher` is the safety net for tier 2's silent-fail mode**: spawns a thread that polls `current_theme()` every 200 ms for 5 s. If the registry never matches the target → logs `commit_timeout target=…` and **falls through to tier 3 from inside the watcher thread** — writes the registry directly, broadcasts, pokes shell, polls again to confirm, logs `fallback_registry target=… confirmed=true after_ms=…`. Without this, tier 2's silent-fail leaves the user stuck (e.g. sunset fires, ShellExecute reports success, registry stays light, no recovery).

If tier 1 is healthy this path is rarely entered. It exists as backup in case future Windows builds break the COM interface.

#### Tier 3: registry-only (last resort — `applied=registry`)

`write_theme_registry` writes `AppsUseLightTheme` + `SystemUsesLightTheme`, broadcasts `WM_SETTINGCHANGE("ImmersiveColorSet")` to `HWND_BROADCAST`, calls `poke_shell`. **Flips light/dark mode but not wallpaper.** Hit when the `.theme` file is missing entirely, or when reached as the commit_watcher fallback.

**`poke_shell`** sends `WM_THEMECHANGED` + targeted `WM_SETTINGCHANGE("ImmersiveColorSet")` to `Shell_TrayWnd` and `Shell_SecondaryTrayWnd`, then `DwmFlush()`. Required for tiers 2 and 3 — `IThemeManager2::SetCurrentTheme` does the broadcast internally so tier 1 doesn't need it. If future Win versions add new taskbar window classes, extend the list.

**Theme file resolution** (`resolve_theme_file`): if `config.theme_day` / `theme_night` is a valid path, use it; otherwise fall back to system defaults at `%SystemRoot%\Resources\Themes\aero.theme` (light) / `dark.theme` (dark). Custom user themes work with tier 1 only if they're already registered with Windows (i.e. installed via Settings → Themes). Otherwise tier 1 errors with `no installed theme matches DisplayName "…"` and tier 2 takes over.

### 2. Event loop — only tick on specific events

The run closure must **not** call `tick()` on every event. An earlier version did, and the app fought the user's manual theme changes: they'd set Dark in Settings → Windows broadcasts `WM_SETTINGCHANGE` → winit delivers an event → our closure called `tick()` → saw `current != target`, flipped back to Light → user saw "Settings won't stay on Dark". Current behavior only ticks on:

- `Event::NewEvents(StartCause::Init)` — first event after launch.
- `Event::NewEvents(StartCause::ResumeTimeReached { .. })` — scheduled sunrise/sunset fired.
- `Event::UserEvent(AppEvent::Menu(refresh_id))` — user clicked Refresh.
- `Event::UserEvent(AppEvent::Wake(_))` — session unlock / power resume (see section 5; safe because these never fire on a Settings theme change).

Everything else is `_ => {}` (the Menu arm also handles Open Config and Quit, which don't tick). This matches macOS behavior: manual overrides persist until the next natural transition. `ControlFlow::WaitUntil(deadline)` is set once per tick and sticks across unrelated events (no need to re-set on WaitCancelled).

**State-aware apply**: `tick` decides via the pure `decide_tick(kind, current, target, now, &TickState)` → `Apply` / `SkipInSync` / `SkipOverride` / `CancelRetry`, called with the **pre-tick** state; outcomes are recorded after the apply result is known (`note_reconciled` on any non-Err outcome, `note_apply_failed` on Err). **Refresh always applies** (config edits take effect immediately) and resets the retry budget first. **Bounded apply retry (v0.4.0)**: a failed apply reschedules the WaitUntil to `min(next_transition, now + 60 s)` for up to 3 consecutive attempts (log field ` retry=N`, then ` retry=exhausted`; budget resets per episode). The retry arrives as a normal ResumeTimeReached tick; `retry_baseline` (the theme observed at failure) gates it — if the screen moved off the baseline, the user intervened and the retry stands down (`applied=skip-user-intervened`). Note the retry only covers *total* apply failure (all three tiers, realistically a failed registry write); a tier-2 ShellExecute silent-fail still recovers via commit_watcher's registry fallback, not the retry counter.

Scheduling math: `schedule(now_utc) -> (Theme, next_utc)` is the single source of truth (rewritten 2026-07-04; unit-tested in `mod tests`). It collects sunrise/sunset instants for the **UTC** dates D−1..D+1 via the `sun-times` crate, sorts them as instants, and picks state-after-last-event ≤ now / first-event > now. **Never pass a local date to `sun_times`** — it takes a UTC date and keys events to the solar day; the old code did exactly that, which made UTC+13/+14 locales permanently dark and skipped post-midnight sunsets (Reykjavik in June). When the ±1-day window is empty (polar day/night), current state comes from `solar_altitude_deg` (local implementation — the crate's `altitude` has math bugs) vs. the −0.833° civil threshold, and the next transition from a ≤200-day forward scan (covers the poles' ~6-month seasons). Everything is pure math on UTC instants; `tick` converts to `Local` only for logging.

### 3. Location (WinRT Geolocation)

`try_get_windows_location()` uses the `windows` crate's `Geolocator::RequestAccessAsync().get()` → `GetGeopositionAsync().get()`. Blocking, but <1 s with a cached location. Called **before** `event_loop.run`, so the tray icon doesn't appear until location is known.

On failure (service off / permission denied): `ask_enable_location()` MessageBox (Yes/No). Yes → `ShellExecute("ms-settings:privacy-location")` + info MessageBox telling user to enable and click Refresh. No → `show_manual_setup_prompt` opens `config.json` in Notepad. Refresh handler silently retries WinRT if location is still empty, so enabling Location Services and clicking Refresh unblocks without a restart.

**COM init matters**: `ensure_com_initialized` → `CoInitializeEx(None, COINIT_APARTMENTTHREADED)` runs first in `main`. WinRT silently fails on an uninitialized thread.

### 4. Config

Exe-relative path (`current_exe().parent().join("config.json")`, never CWD). `#[serde(default)]` at struct level makes missing/unknown fields safe.

**Broken files are never overwritten** (v0.3.2; unit-tested). `load_config_at(path) -> Result<Config, String>`: missing file → first-run, defaults written via `create_new` (so a file appearing in a delete-then-rename save window wins); empty/whitespace file → self-heals to defaults (crash-mid-write leftover, nothing to preserve); any other read/parse failure → `Err`, file untouched. Errors go through `report_config_error`: a `config_error` log line (inner quotes swapped to `'` to keep the `msg="…"` field parseable) plus a MessageBox on a **detached thread** (an `AtomicBool` prevents stacking) — never block startup or the event loop on it. Broken-config session policy: startup runs read-only against the file (in-memory defaults + best-effort WinRT coords, no `save_config`, **no `set_auto_start`** — the fallback `auto_start: true` must not override a broken file's `false`); Refresh keeps the last-known-good config and reports. `save_config` must only ever be called with a config successfully loaded from disk this session.

```rust
struct Config {
    latitude: f64,
    longitude: f64,
    auto_start: bool,
    theme_day:   Option<String>,   // .theme path, or None → aero.theme
    theme_night: Option<String>,   // .theme path, or None → dark.theme
}
```

`has_location()` returns false when both coords are `0.0` (null-island sentinel used for first-run detection).

`set_auto_start(true)` writes `HKCU\...\Run\WinThemeSwitcher` with the current exe path (quoted). `set_auto_start(false)` calls `RegDeleteValueW` — both directions work. `main()` (on a successful load only — see broken-file policy above) and the Refresh handler (always, using the last-known-good config when the reload fails — Refresh re-asserting the Run value is the documented recovery when an AV quarantine deletes it) call `set_auto_start(cfg.auto_start)`, so flipping the flag takes effect at the next launch or Refresh (before 2026-07-04 only the `true` direction was wired up and a `false` flag left the Run entry in place).

### 5. Wake on session unlock / power resume

`ControlFlow::WaitUntil` uses `Instant`, which is monotonic and pauses across system suspend. Before this listener existed, a sunrise transition scheduled at, say, 6 AM would never fire if the machine was asleep through it: after wake at 8 AM, the runtime still saw the deadline as ~22 hours away (24 − sleep duration). The user had to click Refresh to recover.

`start_wake_listener` spawns a worker thread that creates a hidden message-only window (`HWND_MESSAGE`) and registers two notifications against it:

- `WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION)` → delivers `WM_WTSSESSION_CHANGE`. We act on `WTS_SESSION_UNLOCK` (the user re-authenticated after Win+L or wake-from-sleep with a lock screen).
- `PowerRegisterSuspendResumeNotification(DEVICE_NOTIFY_WINDOW_HANDLE, hwnd, ...)` → delivers `WM_POWERBROADCAST`. We act on `PBT_APMRESUMEAUTOMATIC` (resume from sleep without a lock screen — covers machines that don't require re-auth on resume).

Both routes call `proxy.send_event(AppEvent::Wake(WakeKind::Unlock | WakeKind::Power))` via a process-wide `OnceLock<EventLoopProxy<AppEvent>>` (the WindowProc is `extern "system"` and can't capture). The main event loop handles both kinds exactly like a scheduled transition — calls `tick()` (logged as `cause=wake-unlock` / `cause=wake-power`), which is state-aware (no-op if `current == target`). Idempotent across both events firing in sequence.

`WTS_SESSION_UNLOCK` (0x8), `PBT_APMRESUMEAUTOMATIC` (0x12), and `DEVICE_NOTIFY_WINDOW_HANDLE` (0x0) are defined as local consts. windows-sys 0.59 does export all three — but under `Win32::UI::WindowsAndMessaging` rather than the RemoteDesktop/Power modules you'd look in, and the locals carry `WPARAM`/`u32` typing for direct comparison in the WindowProc. Values are stable Win32 ABI; safe to inline.

**Why this doesn't resurrect the manual-override-fight bug**: neither event fires when the user changes theme in Settings — `WM_WTSSESSION_CHANGE` is session lifecycle only, `WM_POWERBROADCAST` is power state only. So ticking on these is safe.

**Manual-override preservation (v0.4.0)**: a manual override (Settings or the tray's Toggle Theme) *survives* lock/unlock and wake-from-sleep. The rule is time-based, not theme-based: `TickState.reconciled_next` records the next-transition instant from the last tick that ended in sync; a Wake tick skips the re-apply only when `now < reconciled_next` (no transition passed while away → the divergence is an override, log `applied=skip-override`). Any-length sleeps reconcile correctly — a theme-parity comparison would wrongly preserve an override across an overnight lock spanning sunset *and* sunrise (design-review catch). A FAILED apply deliberately leaves `reconciled_next` stale so subsequent wakes re-apply instead of misreading the failure as an override (the wake becomes a free retry). Decision logic is the pure `decide_tick` + `note_reconciled`/`note_apply_failed` helpers — unit-tested, including event-sequence tests. Overrides still reset at the next natural transition (documented macOS-like behavior), and do not survive a process restart.

### 6. Tray + menu

Menu: Toggle Theme, Open Config, Refresh, separator, Quit. Menu events flow through `MenuEvent::set_event_handler` → `EventLoopProxy::send_event(AppEvent::Menu(id))` so clicks wake the event loop even when it's on a 12-hour WaitUntil.

**Toggle Theme** applies `toggle_target(current_theme())` (opposite of what's on screen; unreadable → Dark) via `apply_theme` directly — it does **not** call `tick()`, does not touch `TickState`, and does not disturb the pending WaitUntil. That's what makes it a manual override: the preservation rule in §5 keeps it across lock/unlock, and the next natural transition resets it. Known accepted race: a toggle within ~5 s of a *tier-2* apply can be reverted by that apply's still-running commit_watcher (unreachable while tier 1 is healthy).

Tray icon is generated in `make_tray_icon`: 32×32 RGBA, half orange (sun) + half dark-blue (moon). Procedural because `tray-icon`'s default placeholder is near-invisible on both taskbar modes; `with_icon` is required for the icon to actually show.

### 7. Fail-loudly plumbing (v0.4.0)

`main()` is now a thin wrapper: `install_panic_hook()` (writes `panic at=file:line:col msg="…"` to events.log — with `panic = "abort"` + windowed subsystem an unhooked panic is zero-trace death; hook body uses `payload_as_str`, no unwraps) → `claim_single_instance()` (`CreateMutexW("Local\\WinThemeSwitcher.single-instance")`; on `ERROR_ALREADY_EXISTS` → log `duplicate_instance`, info MessageBox, exit 0; handle intentionally leaked; mutex-creation *failure* logs and continues) → `run()` (the old main body). Any `Err` from `run()` — tray creation racing the taskbar at login, event-loop build/death — logs `fatal_error msg="…"` and shows a blocking MessageBox before exit 1 (previously: silent death). Wake-listener registration failures are logged per stage (`wake_listener_err stage=… code=…`); `WTSRegisterSessionNotification` gets 3 attempts 2 s apart (terminal-services machinery may not be up when we auto-start at logon). All `msg="…"` fields flow through `sanitize_log_msg` (quotes→apostrophes, newlines→spaces; unit-tested).

## Dependencies (`Cargo.toml`)

- `chrono`, `sun-times` — sunrise/sunset math.
- `serde` + `serde_json` — config persistence.
- `tray-icon`, `winit` — tray + event loop. Menu types come from `muda` (re-exported under `tray_icon::menu`).
- `windows-sys` (features: `Win32_Foundation`, `Win32_Security`, `Win32_System_Com`, `Win32_System_LibraryLoader`, `Win32_System_Power`, `Win32_System_RemoteDesktop`, `Win32_System_Registry`, `Win32_System_Threading`, `Win32_UI_WindowsAndMessaging`, `Win32_UI_Shell`, `Win32_Graphics_Dwm`) — raw Win32 FFI. `Win32_System_Com` is for `CoCreateInstance` + `CLSCTX_INPROC_SERVER` (IThemeManager2). `SysFreeString` lives in `Win32_Foundation` in windows-sys 0.59 (not `Win32_System_Ole` as you might expect). `CreateMutexW` (single-instance mutex) needs BOTH `Win32_System_Threading` *and* `Win32_Security` — the function is additionally cfg-gated on the latter because its first parameter is `*const SECURITY_ATTRIBUTES`.
- `windows` (features: `Devices_Geolocation`, `Foundation`, `Win32_System_Com`) — WinRT Geolocator + `CoInitializeEx` for the main thread's STA. Kept separate from `windows-sys` because the `windows` crate's typed bindings make Geolocator usable; raw `windows-sys` is fine for everything else.

## Invariants — don't break these

- **`tick()` scope**: only Init / ResumeTimeReached / Refresh / `AppEvent::Wake` (session unlock + power resume). Adding a callsite for any *other* trigger — especially anything that fires on `WM_SETTINGCHANGE` — resurrects the manual-override-fight bug. The wake events are safe specifically because they don't fire when the user changes the theme in Settings.
- **STA thread for IThemeManager2**: `ensure_com_initialized` runs `CoInitializeEx(None, COINIT_APARTMENTTHREADED)` first in `main`. All theme apply runs on that thread. Don't spawn worker threads to call `IThemeManager2` methods — they need their own `CoInitializeEx(STA)` and proper marshaling.
- **`poke_shell` after tier-2 / tier-3 apply only**: tier 1 (`IThemeManager2::SetCurrentTheme`) does the broadcast internally — calling `poke_shell` after it is wasted work and re-introduces the AV-tripping `HWND_BROADCAST WM_SETTINGCHANGE` signal that tier 1 was supposed to eliminate. Keep `poke_shell` for the legacy paths only; don't add it to tier 1.
- **Refresh forces apply** (bypasses state check); scheduled transitions respect it (no-op if already matching). Don't invert.
- **`decide_tick` reads the PRE-tick state**; `note_reconciled`/`note_apply_failed` run after the apply outcome is known. A failed apply must leave `reconciled_next` stale — updating it on Err makes the wake rule misread the failure as a user override and strands the wrong theme (the exact blocker the v0.4.0 design review caught). Never "simplify" by assigning state before/regardless of the outcome.
- **Toggle Theme never ticks and never touches `TickState`** — it's a manual override by construction. Routing it through `tick()` or recording it in state breaks override preservation.
- **Capture `GetLastError()` into a local immediately** after the failing Win32 call — `Local::now()`, `log_event`, and `format!` all make Win32 calls that clobber the thread's last error. (`PowerRegisterSuspendResumeNotification` is the exception: its error code IS the return value.)
- **Run tests via `scripts\test.ps1` on this machine**, not bare `cargo test` — the wrapper signs the test binary before first execution (Kaspersky section).
- **Free BSTRs from `ITheme::GetDisplayName` with `SysFreeString`** — not `CoTaskMemFree`, and definitely don't leak. The wtheme reference treats this strictly.
- **Vtable order in `IThemeManager2Vtbl`**: every method's slot index must match the COM ABI. Wrong order = calling the wrong method (silently catastrophic). The struct declares every slot up through `set_current_theme` — uncalled interior slots are `_`-prefixed placeholders that are **mandatory padding, never removable**; only trailing slots after the last called method may be omitted, and nothing may ever be reordered. Reference: namazso C# gist + wtheme C header (linked in main.rs comments).
- **`ensure_com_initialized` before any WinRT call**: otherwise Geolocator returns errors silently.
- **UTF-16 + NUL**: all Win32 wide strings go through `wide()` which appends the null terminator. Never pass a bare `&str` to a `*W` API.
- **HWND null check**: `(hwnd as usize) == 0` — robust to `windows-sys` flipping between `*mut c_void` and `isize`.
- **Windowed subsystem** (`#![windows_subsystem = "windows"]`): no console, `println!` goes nowhere. For diagnostics, write to a file or `OutputDebugStringW`.
