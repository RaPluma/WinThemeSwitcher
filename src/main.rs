#![windows_subsystem = "windows"]

use std::error::Error;
use std::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use sun_times::sun_times;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    TrayIconBuilder,
};
use windows::Devices::Geolocation::{GeolocationAccessStatus, Geolocator};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows_sys::core::{GUID, HRESULT};
use windows_sys::Win32::Foundation::{
    GetLastError, SysFreeString, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM,
};
use windows_sys::Win32::Graphics::Dwm::DwmFlush;
use windows_sys::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Power::PowerRegisterSuspendResumeNotification;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD, REG_SZ,
};
use windows_sys::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Shell::{SHLoadIndirectString, ShellExecuteW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowW, GetMessageW, MessageBoxW,
    PostMessageW, RegisterClassW, SendMessageTimeoutW, TranslateMessage, HWND_BROADCAST,
    HWND_MESSAGE, IDYES, MB_ICONINFORMATION, MB_ICONQUESTION, MB_ICONWARNING, MB_OK, MB_YESNO, MSG,
    SMTO_ABORTIFHUNG, SW_HIDE, SW_SHOWNORMAL, WM_CLOSE, WM_POWERBROADCAST, WM_SETTINGCHANGE,
    WM_THEMECHANGED, WM_WTSSESSION_CHANGE, WNDCLASSW,
};
use winit::event::{Event, StartCause};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};

const PBT_APMRESUMEAUTOMATIC: WPARAM = 0x12;
const WTS_SESSION_UNLOCK: WPARAM = 0x8;
const DEVICE_NOTIFY_WINDOW_HANDLE: u32 = 0x0;

const THEME_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const APP_NAME: &str = "WinThemeSwitcher";

// === IThemeManager2 (themeui.dll, undocumented but stable since Win10 1809) ===
//
// The Settings UWP wraps this same interface. AutoDarkMode and similar tools use
// it as the canonical theme-apply path. Going through this instead of
// ShellExecuteW(.theme) avoids the silent-fail problem we hit with the UWP
// activation pipeline (post-unlock / scheduled-while-away contexts), AND removes
// most of the heuristic AV signals (no HWND_BROADCAST WM_SETTINGCHANGE, no
// direct WM_THEMECHANGED to Shell_TrayWnd — SetCurrentTheme does the broadcast
// itself from inside themeui.dll, where it's expected by AV behavior models).
//
// References:
//   - https://gist.github.com/namazso/0fde102c2fc56049c7c37f7fdf9ac3cd (C#)
//   - https://github.com/HenriquedoVal/wtheme/blob/main/ThemeManager2.h (C)
//   - https://github.com/AutoDarkMode/Windows-Auto-Night-Mode/blob/master/AutoDarkModeSvc/Handlers/IThemeManager2/Tm2Handler.cs

const CLSID_THEME_MANAGER2: GUID = GUID {
    data1: 0x9324da94,
    data2: 0x50ec,
    data3: 0x4a14,
    data4: [0xa7, 0x70, 0xe9, 0x0c, 0xa0, 0x3e, 0x7c, 0x8f],
};

const IID_THEME_MANAGER2: GUID = GUID {
    data1: 0xc1e8c83e,
    data2: 0x845d,
    data3: 0x4d95,
    data4: [0x81, 0xdb, 0xe2, 0x83, 0xfd, 0xff, 0xc0, 0x00],
};

const THEME_INIT_NO_FLAGS: i32 = 0;
// THEME_APPLY_FLAGS bitmask. 0 = apply everything (matches Settings UWP). NO_HOURGLASS
// suppresses the wait cursor for unattended apply.
const THEME_APPLY_FLAG_NO_HOURGLASS: i32 = 1 << 8;

#[repr(C)]
struct IThemeManager2Vtbl {
    // IUnknown
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    // IThemeManager2 — only the slots we actually call. ORDER MATTERS — must
    // match vtable layout exactly. Reference: namazso C# gist + wtheme C header.
    init: unsafe extern "system" fn(*mut c_void, i32) -> HRESULT,
    _init_async: unsafe extern "system" fn(*mut c_void, HWND, i32) -> HRESULT,
    _refresh: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    _refresh_async: unsafe extern "system" fn(*mut c_void, HWND, i32) -> HRESULT,
    _refresh_complete: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    get_theme_count: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    get_theme: unsafe extern "system" fn(*mut c_void, i32, *mut *mut c_void) -> HRESULT,
    _is_theme_disabled: unsafe extern "system" fn(*mut c_void, i32, *mut i32) -> HRESULT,
    _get_current_theme: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    set_current_theme: unsafe extern "system" fn(*mut c_void, HWND, i32, i32, i32, i32) -> HRESULT,
    // Remaining slots (GetCustomTheme, GetDefaultTheme, CreateThemePack, ...) omitted —
    // not called from this app. The struct only needs to expose what we call;
    // unused trailing slots don't affect ABI.
}

#[repr(C)]
struct IThemeVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    // GetDisplayName returns a BSTR (allocated with SysAllocString — release with SysFreeString).
    get_display_name: unsafe extern "system" fn(*mut c_void, *mut *mut u16) -> HRESULT,
    // PutDisplayName + later methods omitted — wtheme header notes they vary across
    // Windows versions and aren't safe to call.
}

/// RAII wrapper around an IThemeManager2 COM pointer. Calls Release on drop.
struct ThemeMgr {
    ptr: *mut c_void,
}

impl ThemeMgr {
    /// CoCreateInstance + Init. Caller must already be on an STA thread
    /// (we are — main thread does CoInitializeEx(APARTMENTTHREADED) at startup).
    unsafe fn create() -> Result<Self, HRESULT> {
        let mut ptr: *mut c_void = ptr::null_mut();
        let hr = CoCreateInstance(
            &CLSID_THEME_MANAGER2,
            ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_THEME_MANAGER2,
            &mut ptr,
        );
        if hr < 0 || ptr.is_null() {
            return Err(hr);
        }
        let vtbl = Self::vtbl_of(ptr);
        let hr = (vtbl.init)(ptr, THEME_INIT_NO_FLAGS);
        if hr < 0 {
            (vtbl.release)(ptr);
            return Err(hr);
        }
        Ok(Self { ptr })
    }

    unsafe fn vtbl_of(ptr: *mut c_void) -> &'static IThemeManager2Vtbl {
        &**(ptr as *const *const IThemeManager2Vtbl)
    }

    unsafe fn vtbl(&self) -> &IThemeManager2Vtbl {
        Self::vtbl_of(self.ptr)
    }

    unsafe fn count(&self) -> Result<i32, HRESULT> {
        let mut n = 0i32;
        let hr = (self.vtbl().get_theme_count)(self.ptr, &mut n);
        if hr < 0 {
            return Err(hr);
        }
        Ok(n)
    }

    /// Returns the display name of the theme at `index`, or an HRESULT error.
    /// Note: enumeration order is not stable across launches — re-enumerate every
    /// apply rather than caching indices.
    unsafe fn theme_display_name(&self, index: i32) -> Result<String, HRESULT> {
        let mut theme_ptr: *mut c_void = ptr::null_mut();
        let hr = (self.vtbl().get_theme)(self.ptr, index, &mut theme_ptr);
        if hr < 0 || theme_ptr.is_null() {
            return Err(hr);
        }
        let theme_vtbl = &**(theme_ptr as *const *const IThemeVtbl);
        let mut bstr: *mut u16 = ptr::null_mut();
        let hr = (theme_vtbl.get_display_name)(theme_ptr, &mut bstr);
        if hr < 0 || bstr.is_null() {
            (theme_vtbl.release)(theme_ptr);
            return Err(hr);
        }
        let name = read_wide_string(bstr);
        SysFreeString(bstr);
        (theme_vtbl.release)(theme_ptr);
        Ok(name)
    }

    /// Apply the theme at `index`. `apply_now=1` makes it take effect immediately
    /// (registry write + WM_THEMECHANGED + WM_SETTINGCHANGE broadcast all happen
    /// inside SetCurrentTheme). `pack_flags=0` matches Settings UWP defaults.
    unsafe fn set_current(&self, index: i32, apply_flags: i32) -> Result<(), HRESULT> {
        let hr =
            (self.vtbl().set_current_theme)(self.ptr, ptr::null_mut(), index, 1, apply_flags, 0);
        if hr < 0 {
            return Err(hr);
        }
        Ok(())
    }
}

impl Drop for ThemeMgr {
    fn drop(&mut self) {
        unsafe { (self.vtbl().release)(self.ptr) };
    }
}

unsafe fn read_wide_string(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *p.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
struct Config {
    latitude: f64,
    longitude: f64,
    auto_start: bool,
    theme_day: Option<String>,
    theme_night: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            latitude: 0.0,
            longitude: 0.0,
            auto_start: true,
            theme_day: None,
            theme_night: None,
        }
    }
}

impl Config {
    fn has_location(&self) -> bool {
        !(self.latitude == 0.0 && self.longitude == 0.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Theme {
    Light,
    Dark,
}

impl Theme {
    fn opposite(self) -> Theme {
        match self {
            Theme::Light => Theme::Dark,
            Theme::Dark => Theme::Light,
        }
    }
}

/// Why a tick is running — decides whether an apply is forced, state-aware, or
/// override-preserving (see `should_apply`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TickKind {
    /// First tick after launch.
    Init,
    /// ResumeTimeReached — a scheduled sunrise/sunset (or an apply retry).
    Scheduled,
    /// Session unlock / power resume.
    Wake,
    /// User clicked Refresh.
    Refresh,
}

/// Per-session tick state, owned by the event-loop closure.
struct TickState {
    /// Next scheduled transition (UTC) recorded by the last tick that ended
    /// reconciled — applied successfully, found the screen already matching,
    /// or deliberately preserved an override. `now >= reconciled_next` on a
    /// later tick means at least one transition has passed since we were
    /// last in sync, regardless of how many were missed (a same-THEME parity
    /// comparison would wrongly preserve an override across an ordinary
    /// overnight lock that spans sunset AND sunrise). A FAILED apply leaves
    /// this stale on purpose: every subsequent wake then sees the transition
    /// as still-unreconciled and re-applies, instead of misreading the
    /// failure as a user override.
    reconciled_next: Option<DateTime<Utc>>,
    /// Consecutive failed applies in the current failure episode.
    retry_count: u32,
    /// current_theme() observed when the last apply failed. If the screen no
    /// longer matches this, the user intervened during the retry window and
    /// the retry must stand down rather than clobber their choice.
    retry_baseline: Option<Theme>,
    /// The next-transition instant computed at the tick whose apply failed —
    /// the failure episode's own window. An intervention only cancels the
    /// retry while `now < episode_next`; past it, a transition has passed
    /// and reconciling to the schedule outranks the stand-down (otherwise an
    /// intervention right before a suspend that spans a transition would be
    /// promoted to a day-long override).
    episode_next: Option<DateTime<Utc>>,
}

impl TickState {
    fn new() -> Self {
        Self {
            reconciled_next: None,
            retry_count: 0,
            retry_baseline: None,
            episode_next: None,
        }
    }
}

/// What a tick decided to do — see `decide_tick`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TickAction {
    /// Call apply_theme.
    Apply,
    /// Screen already matches the schedule.
    SkipInSync,
    /// Wake tick, screen diverges, but no transition passed since the last
    /// reconciled tick: a manual override is being preserved.
    SkipOverride,
    /// The user changed the theme during a pending retry window — cancel the
    /// retry episode and let their choice stand until the next transition.
    CancelRetry,
}

/// The tick decision, minus all I/O. Pure — unit-tested, including across
/// sequences of ticks mutating one TickState via note_reconciled /
/// note_apply_failed. IMPORTANT: called with the PRE-tick state; the state
/// notes are recorded after the apply outcome is known.
fn decide_tick(
    kind: TickKind,
    current: Option<Theme>,
    target: Theme,
    now: DateTime<Utc>,
    state: &TickState,
) -> TickAction {
    // Refresh is fresh user intent: always force-apply.
    if kind == TickKind::Refresh {
        return TickAction::Apply;
    }
    // Pending-retry gate: the screen moved away from the failure snapshot,
    // so the user intervened mid-episode. Only an OBSERVED move counts — an
    // unreadable reading (None) on EITHER side proves nothing and must not
    // cancel. And the stand-down only applies within the failure episode's
    // own window (now < episode_next): once a transition has passed,
    // reconciling to the schedule outranks it, exactly like any override
    // ending at the next natural transition.
    if state.retry_count > 0
        && state.retry_baseline.is_some()
        && current.is_some()
        && current != state.retry_baseline
        && state.episode_next.is_some_and(|n| now < n)
    {
        return TickAction::CancelRetry;
    }
    if current == Some(target) {
        return TickAction::SkipInSync;
    }
    let transition_passed = state.reconciled_next.is_none_or(|n| now >= n);
    // Override preservation yields to a pending retry: a wake during an
    // active failure episode is a free retry opportunity (the divergence is
    // the FAILURE, not an override), including episodes started mid-window
    // by a failed Refresh where reconciled_next is still in the future.
    if kind == TickKind::Wake && !transition_passed && state.retry_count == 0 {
        return TickAction::SkipOverride;
    }
    TickAction::Apply
}

/// Record a tick that ended in sync with the schedule (any non-Err outcome).
fn note_reconciled(state: &mut TickState, next: DateTime<Utc>) {
    state.reconciled_next = Some(next);
    state.retry_count = 0;
    state.retry_baseline = None;
    state.episode_next = None;
}

/// Record a failed apply. Returns true when a quick retry should be
/// scheduled; false when the budget is exhausted (the episode resets so the
/// NEXT transition window gets a fresh budget, and reconciled_next stays
/// stale so wake events remain free retry opportunities).
fn note_apply_failed(
    state: &mut TickState,
    observed: Option<Theme>,
    next_utc: DateTime<Utc>,
) -> bool {
    state.retry_count += 1;
    state.retry_baseline = observed;
    if state.retry_count > MAX_APPLY_RETRIES {
        state.retry_count = 0;
        state.retry_baseline = None;
        state.episode_next = None;
        false
    } else {
        state.episode_next = Some(next_utc);
        true
    }
}

/// What the Toggle menu item should apply. Pure — unit-tested. An unreadable
/// current theme (registry read failure) defaults the base to Light, so the
/// first toggle lands on Dark.
fn toggle_target(current: Option<Theme>) -> Theme {
    current.unwrap_or(Theme::Light).opposite()
}

/// Keep a message single-line and parseable inside a `msg="..."` log field
/// (inner quotes swapped to apostrophes, newlines flattened). Shared by the
/// panic hook, fatal-error reporting, config-error reporting, and tick
/// apply-error lines.
fn sanitize_log_msg(s: &str) -> String {
    s.replace('"', "'").replace(['\n', '\r'], " ")
}

/// How long after a failed apply the bounded retry fires.
const APPLY_RETRY_DELAY_SECS: i64 = 60;
/// Consecutive failures after which we stop retrying until the next
/// scheduled transition.
const MAX_APPLY_RETRIES: u32 = 3;

/// Deadline for the next tick after a failed apply: retry soon, but never
/// past the scheduled transition itself. Pure — unit-tested.
fn retry_deadline(now: DateTime<Local>, next: DateTime<Local>) -> DateTime<Local> {
    std::cmp::min(
        next,
        now + chrono::Duration::seconds(APPLY_RETRY_DELAY_SECS),
    )
}

#[derive(Debug, Clone)]
enum AppEvent {
    Menu(MenuId),
    Wake(WakeKind),
}

#[derive(Debug, Clone, Copy)]
enum WakeKind {
    Unlock,
    Power,
}

static EVENT_PROXY: OnceLock<EventLoopProxy<AppEvent>> = OnceLock::new();

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn config_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("config.json")
}

fn log_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("events.log")
}

fn log_event(line: &str) {
    use std::io::Write;
    let path = log_path();
    if let Ok(meta) = fs::metadata(&path) {
        if meta.len() > 256 * 1024 {
            let _ = fs::rename(&path, path.with_extension("log.old"));
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{}", line);
    }
}

fn theme_str(t: Option<Theme>) -> &'static str {
    match t {
        Some(Theme::Light) => "light",
        Some(Theme::Dark) => "dark",
        None => "unknown",
    }
}

/// Load the config from `path`. A missing file is first-run: defaults are
/// written and returned. `Err` means the file EXISTS but could not be read or
/// parsed — it is left untouched on disk so a hand-edit typo can be fixed
/// instead of silently wiping the user's coordinates and theme paths.
fn load_config_at(path: &Path) -> Result<Config, String> {
    match fs::read_to_string(path) {
        Ok(content) if content.trim().is_empty() => {
            // A crash mid-write (fs::write truncates before writing) leaves
            // a 0-byte file. Nothing in it to preserve — self-heal like
            // first run instead of erroring on every launch.
            let cfg = Config::default();
            if let Ok(json) = serde_json::to_string_pretty(&cfg) {
                let _ = fs::write(path, json);
            }
            Ok(cfg)
        }
        Ok(content) => serde_json::from_str::<Config>(&content)
            .map_err(|e| format!("config.json is not valid JSON: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let cfg = Config::default();
            if let Ok(json) = serde_json::to_string_pretty(&cfg) {
                // create_new, not fs::write: if the file appears between our
                // read and this write (an editor saving via delete-then-
                // rename), the user's file wins and the defaults are dropped.
                use std::io::Write;
                if let Ok(mut f) = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                {
                    let _ = f.write_all(json.as_bytes());
                }
            }
            Ok(cfg)
        }
        Err(e) => Err(format!("config.json could not be read: {e}")),
    }
}

/// Serialize `cfg` over config.json unconditionally. Only call with a Config
/// that was successfully loaded from disk this session — persisting a
/// default/fallback Config here is exactly the settings-wipe bug fixed in
/// v0.3.2 (broken files must stay on disk for the user to repair).
fn save_config(cfg: &Config) -> Result<(), Box<dyn Error>> {
    let json = serde_json::to_string_pretty(cfg)?;
    fs::write(config_path(), json)?;
    Ok(())
}

fn ensure_com_initialized() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

fn try_get_windows_location() -> Option<(f64, f64)> {
    let access = Geolocator::RequestAccessAsync().ok()?.get().ok()?;
    if access != GeolocationAccessStatus::Allowed {
        return None;
    }
    let geo = Geolocator::new().ok()?;
    let pos = geo.GetGeopositionAsync().ok()?.get().ok()?;
    let coord = pos.Coordinate().ok()?;
    let point = coord.Point().ok()?;
    let p = point.Position().ok()?;
    Some((p.Latitude, p.Longitude))
}

fn open_config_in_editor() {
    let path = config_path();
    let path_w = wide(&path.to_string_lossy());
    let verb = wide("open");
    unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            verb.as_ptr(),
            path_w.as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn open_location_settings() {
    let verb = wide("open");
    let uri = wide("ms-settings:privacy-location");
    unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            verb.as_ptr(),
            uri.as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn show_message_box(title: &str, body: &str, flags: u32) -> i32 {
    let title_w = wide(title);
    let body_w = wide(body);
    unsafe { MessageBoxW(ptr::null_mut(), body_w.as_ptr(), title_w.as_ptr(), flags) }
}

fn ask_enable_location() -> bool {
    show_message_box(
        "WinThemeSwitcher — Location",
        "Windows Location is off or not allowed for desktop apps.\n\n\
         Enable it so sunrise and sunset can be computed automatically?\n\n\
         Yes opens Windows Settings. No lets you enter coordinates manually in config.json.",
        MB_YESNO | MB_ICONQUESTION,
    ) == IDYES
}

fn show_enable_pending_message() {
    show_message_box(
        "WinThemeSwitcher",
        "Turn on \"Location services\" in the Settings window that just opened. \
         Then right-click the WinThemeSwitcher tray icon and choose Refresh.",
        MB_OK | MB_ICONINFORMATION,
    );
}

fn show_manual_setup_prompt() {
    show_message_box(
        "WinThemeSwitcher — Setup",
        "Please set latitude and longitude in config.json (opening now), \
         then right-click the tray icon and choose Refresh.",
        MB_OK | MB_ICONINFORMATION,
    );
    open_config_in_editor();
}

/// A config-error box is already on screen (repeated Refresh clicks with a
/// still-broken file must not stack duplicates).
static CONFIG_ERROR_BOX_OPEN: AtomicBool = AtomicBool::new(false);

/// Log + tell the user their hand-edited config is broken and was left
/// untouched. Called from startup and from a user-initiated Refresh. The
/// MessageBox runs on a detached thread — a modal here would otherwise park
/// startup before the tray exists, or stall the event loop (scheduled
/// transitions, wake events) until dismissed. Plain MessageBoxW has no STA
/// requirement, so a worker thread is fine.
fn report_config_error(err: &str) {
    log_event(&format!(
        "{} config_error msg=\"{}\"",
        Local::now().to_rfc3339(),
        // serde_json errors quote the offending token; keep the log's
        // quoted-field convention parseable.
        sanitize_log_msg(err),
    ));
    if CONFIG_ERROR_BOX_OPEN.swap(true, Ordering::SeqCst) {
        return;
    }
    let body = format!(
        "{err}\n\nThe file was left unchanged — your settings are still in it. \
         Fix the error (tray menu → Open Config), then choose Refresh.",
    );
    std::thread::spawn(move || {
        show_message_box(
            "WinThemeSwitcher — Config error",
            &body,
            MB_OK | MB_ICONWARNING,
        );
        CONFIG_ERROR_BOX_OPEN.store(false, Ordering::SeqCst);
    });
}

fn acquire_location(cfg: &mut Config) {
    if let Some((lat, lon)) = try_get_windows_location() {
        cfg.latitude = lat;
        cfg.longitude = lon;
        let _ = save_config(cfg);
        return;
    }
    if ask_enable_location() {
        open_location_settings();
        show_enable_pending_message();
    } else {
        show_manual_setup_prompt();
    }
}

fn current_theme() -> Option<Theme> {
    let subkey = wide(THEME_KEY);
    let value = wide("SystemUsesLightTheme");
    unsafe {
        let mut hkey: HKEY = ptr::null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut hkey,
        ) != 0
        {
            return None;
        }
        let mut data: u32 = 0;
        let mut size: u32 = 4;
        let mut kind: u32 = 0;
        let r = RegQueryValueExW(
            hkey,
            value.as_ptr(),
            ptr::null_mut(),
            &mut kind,
            &mut data as *mut u32 as *mut u8,
            &mut size,
        );
        RegCloseKey(hkey);
        if r != 0 {
            return None;
        }
        Some(if data == 0 { Theme::Dark } else { Theme::Light })
    }
}

fn write_theme_registry(theme: Theme) -> Result<(), Box<dyn Error>> {
    let value: u32 = if theme == Theme::Light { 1 } else { 0 };
    let subkey = wide(THEME_KEY);
    let apps = wide("AppsUseLightTheme");
    let sys = wide("SystemUsesLightTheme");
    unsafe {
        let mut hkey: HKEY = ptr::null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        ) != 0
        {
            return Err("RegOpenKeyExW failed for Personalize".into());
        }
        RegSetValueExW(
            hkey,
            apps.as_ptr(),
            0,
            REG_DWORD,
            &value as *const u32 as *const u8,
            4,
        );
        RegSetValueExW(
            hkey,
            sys.as_ptr(),
            0,
            REG_DWORD,
            &value as *const u32 as *const u8,
            4,
        );
        RegCloseKey(hkey);
    }
    Ok(())
}

fn broadcast_setting_change() {
    let param = wide("ImmersiveColorSet");
    let mut result: usize = 0;
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            param.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            500,
            &mut result,
        );
    }
}

fn poke_shell() {
    let param = wide("ImmersiveColorSet");
    for class in ["Shell_TrayWnd", "Shell_SecondaryTrayWnd"] {
        let cls = wide(class);
        unsafe {
            let hwnd = FindWindowW(cls.as_ptr(), ptr::null());
            if (hwnd as usize) == 0 {
                continue;
            }
            let mut result: usize = 0;
            SendMessageTimeoutW(
                hwnd,
                WM_THEMECHANGED,
                0,
                0,
                SMTO_ABORTIFHUNG,
                500,
                &mut result,
            );
            SendMessageTimeoutW(
                hwnd,
                WM_SETTINGCHANGE,
                0,
                param.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                500,
                &mut result,
            );
        }
    }
    unsafe {
        DwmFlush();
    }
}

fn resolve_theme_file(theme: Theme, cfg: &Config) -> PathBuf {
    let custom = match theme {
        Theme::Light => cfg.theme_day.as_deref(),
        Theme::Dark => cfg.theme_night.as_deref(),
    };
    if let Some(p) = custom {
        let path = PathBuf::from(p);
        if path.exists() {
            return path;
        }
    }
    let win_dir = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    let leaf = match theme {
        Theme::Light => "aero.theme",
        Theme::Dark => "dark.theme",
    };
    PathBuf::from(win_dir)
        .join("Resources")
        .join("Themes")
        .join(leaf)
}

fn apply_theme_file(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy();
    let path_w = wide(&s);
    let verb = wide("open");
    unsafe {
        let h = ShellExecuteW(
            ptr::null_mut(),
            verb.as_ptr(),
            path_w.as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_HIDE,
        );
        (h as isize) > 32
    }
}

fn start_settings_closer() {
    let started = Instant::now();
    std::thread::spawn(move || {
        let class = wide("ApplicationFrameWindow");
        let titles = [
            ("Settings", wide("Settings")),
            ("Themes", wide("Themes")),
            ("Personalization", wide("Personalization")),
        ];
        let mut first_close_logged = false;
        for _ in 0..14 {
            std::thread::sleep(Duration::from_millis(150));
            for (name, title_w) in &titles {
                unsafe {
                    let hwnd = FindWindowW(class.as_ptr(), title_w.as_ptr());
                    if (hwnd as usize) != 0 {
                        PostMessageW(hwnd, WM_CLOSE, 0, 0);
                        if !first_close_logged {
                            log_event(&format!(
                                "{} settings_closed title={} after_ms={}",
                                Local::now().to_rfc3339(),
                                name,
                                started.elapsed().as_millis(),
                            ));
                            first_close_logged = true;
                        }
                    }
                }
            }
        }
    });
}

fn start_commit_watcher(target: Theme) {
    let started = Instant::now();
    std::thread::spawn(move || {
        for _ in 0..25 {
            std::thread::sleep(Duration::from_millis(200));
            if current_theme() == Some(target) {
                log_event(&format!(
                    "{} commit_observed target={} after_ms={}",
                    Local::now().to_rfc3339(),
                    theme_str(Some(target)),
                    started.elapsed().as_millis(),
                ));
                return;
            }
        }
        log_event(&format!(
            "{} commit_timeout target={} actual={} after_ms={}",
            Local::now().to_rfc3339(),
            theme_str(Some(target)),
            theme_str(current_theme()),
            started.elapsed().as_millis(),
        ));
        // ShellExecute(.theme) lied about success — Settings UWP didn't actually apply.
        // Observed when the schedule fires while the user isn't interactive (sunset while
        // away, immediately after WTS_SESSION_UNLOCK, immediately after PBT_APMRESUMEAUTOMATIC).
        // Force the mode flip via direct registry write so at minimum light/dark is correct;
        // wallpaper won't change on this path (would require IThemeManager2 — see CLAUDE.md).
        let fb_started = Instant::now();
        match write_theme_registry(target) {
            Ok(()) => {
                broadcast_setting_change();
                poke_shell();
                let mut confirmed = false;
                for _ in 0..10 {
                    std::thread::sleep(Duration::from_millis(100));
                    if current_theme() == Some(target) {
                        confirmed = true;
                        break;
                    }
                }
                log_event(&format!(
                    "{} fallback_registry target={} confirmed={} after_ms={}",
                    Local::now().to_rfc3339(),
                    theme_str(Some(target)),
                    confirmed,
                    fb_started.elapsed().as_millis(),
                ));
            }
            Err(e) => {
                log_event(&format!(
                    "{} fallback_registry_err target={} err=\"{}\"",
                    Local::now().to_rfc3339(),
                    theme_str(Some(target)),
                    e,
                ));
            }
        }
    });
}

/// Reads `[Theme]\nDisplayName=...` from a `.theme` (INI) file.
/// `DisplayName` may be a literal string, OR an SHLoadIndirectString resource
/// reference of the form `@%SystemRoot%\System32\themeui.dll,-2060` (system themes
/// use this — the actual user-visible name is in a localized string table).
/// Returns the resolved literal string, or None if the file is unreadable / has no
/// DisplayName / the indirect-string resolution fails.
///
/// Reads as raw bytes + lossy UTF-8 decode rather than `fs::read_to_string`
/// because system .theme files are sometimes Windows-1252 (e.g. `aero.theme`'s
/// copyright comment has a raw `0xa9` for `©`, which is invalid UTF-8 and would
/// make the strict decode fail outright). The keyword we care about
/// (`DisplayName=`) is pure ASCII, and comment lines (which contain the funky
/// bytes) are skipped before any lossy replacement matters.
fn resolve_theme_display_name(theme_file: &Path) -> Option<String> {
    let bytes = fs::read(theme_file).ok()?;
    let content = String::from_utf8_lossy(&bytes);
    let mut in_theme_section = false;
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.starts_with(';') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_theme_section = line.eq_ignore_ascii_case("[Theme]");
            continue;
        }
        if !in_theme_section {
            continue;
        }
        if let Some(rest) = line.strip_prefix("DisplayName=") {
            let raw = rest.trim();
            if raw.starts_with('@') {
                return resolve_indirect_string(raw);
            }
            return Some(raw.to_string());
        }
    }
    None
}

/// Resolves `@dll,-id` resource string references using SHLoadIndirectString.
fn resolve_indirect_string(source: &str) -> Option<String> {
    let src_w = wide(source);
    let mut buf = [0u16; 512];
    let hr = unsafe {
        SHLoadIndirectString(
            src_w.as_ptr(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            ptr::null_mut(),
        )
    };
    if hr < 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    if len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// Apply a `.theme` file via the IThemeManager2 COM interface — the same API
/// the Settings UWP itself wraps. Reliable from any context (post-unlock,
/// scheduled-while-away, background tick) — unlike the ShellExecuteW(.theme)
/// path which silently fails when the user isn't actively interactive.
///
/// The interface enumerates installed themes by index (no by-path lookup), so
/// we resolve the target's DisplayName from the .theme file and match it
/// against `ITheme::GetDisplayName` in the enumeration. System themes
/// (aero.theme, dark.theme) are always present after `Init`. Custom user
/// themes need to have been installed first (e.g. via Settings → Themes, or
/// AddAndSelectTheme — not implemented here; custom themes fall through to
/// the legacy path).
///
/// Logs `theme_manager2_apply` on success; the caller logs the err string.
fn apply_via_theme_manager2(theme: Theme, theme_file: &Path) -> Result<(), Box<dyn Error>> {
    let target_name = resolve_theme_display_name(theme_file)
        .ok_or("could not resolve DisplayName from .theme file")?;
    let started = Instant::now();
    unsafe {
        let mgr =
            ThemeMgr::create().map_err(|hr| format!("CoCreateInstance/Init hr=0x{:08x}", hr))?;
        let n = mgr
            .count()
            .map_err(|hr| format!("GetThemeCount hr=0x{:08x}", hr))?;
        for i in 0..n {
            let name = match mgr.theme_display_name(i) {
                Ok(n) => n,
                Err(hr) => {
                    log_event(&format!(
                        "{} theme_manager2_enum_skip i={} hr=0x{:08x}",
                        Local::now().to_rfc3339(),
                        i,
                        hr
                    ));
                    continue;
                }
            };
            if name == target_name {
                mgr.set_current(i, THEME_APPLY_FLAG_NO_HOURGLASS)
                    .map_err(|hr| format!("SetCurrentTheme i={} hr=0x{:08x}", i, hr))?;
                log_event(&format!(
                    "{} theme_manager2_apply target={} display=\"{}\" idx={} after_ms={}",
                    Local::now().to_rfc3339(),
                    theme_str(Some(theme)),
                    name,
                    i,
                    started.elapsed().as_millis(),
                ));
                return Ok(());
            }
        }
        Err(format!("no installed theme matches DisplayName \"{}\"", target_name).into())
    }
}

/// Three-tier apply, best-to-worst:
///   1. IThemeManager2  — atomic, reliable, no Settings UWP, no AV-tripping broadcast.
///   2. ShellExecuteW(.theme) + commit_watcher — legacy. Watcher promotes to (3) on silent fail.
///   3. Direct registry write — flips light/dark mode but not wallpaper. Last resort.
fn apply_theme(theme: Theme, cfg: &Config) -> Result<&'static str, Box<dyn Error>> {
    let theme_file = resolve_theme_file(theme, cfg);

    if theme_file.exists() {
        match apply_via_theme_manager2(theme, &theme_file) {
            Ok(()) => return Ok("theme-manager2"),
            Err(e) => log_event(&format!(
                "{} theme_manager2_err target={} msg=\"{}\"",
                Local::now().to_rfc3339(),
                theme_str(Some(theme)),
                e,
            )),
        }
    }

    if theme_file.exists() && apply_theme_file(&theme_file) {
        start_commit_watcher(theme);
        start_settings_closer();
        std::thread::sleep(Duration::from_millis(300));
        poke_shell();
        return Ok("theme-file");
    }

    write_theme_registry(theme)?;
    broadcast_setting_change();
    poke_shell();
    Ok("registry")
}

fn set_auto_start(enable: bool) -> Result<(), Box<dyn Error>> {
    let subkey = wide(RUN_KEY);
    let name = wide(APP_NAME);
    unsafe {
        let mut hkey: HKEY = ptr::null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        ) != 0
        {
            return Err("RegOpenKeyExW failed for Run".into());
        }
        if enable {
            let exe = std::env::current_exe()?;
            let exe_w = wide(&format!("\"{}\"", exe.to_string_lossy()));
            RegSetValueExW(
                hkey,
                name.as_ptr(),
                0,
                REG_SZ,
                exe_w.as_ptr() as *const u8,
                (exe_w.len() * 2) as u32,
            );
        } else {
            RegDeleteValueW(hkey, name.as_ptr());
        }
        RegCloseKey(hkey);
    }
    Ok(())
}

/// Civil sunrise/sunset threshold: sun center 0.833° below the horizon
/// (accounts for refraction + solar radius; same convention as `sun_times`).
const SUNRISE_ALTITUDE_DEG: f64 = -0.833;

/// Solar altitude in degrees at `t` for the given location. Standard
/// low-precision solar position (declination + hour angle against Greenwich
/// sidereal time) — well under a degree of error, plenty for deciding polar
/// day vs. polar night. Implemented locally because the `sun_times` crate's
/// own `altitude` has math bugs (seconds term, spurious to_degrees on an
/// already-degrees longitude).
fn solar_altitude_deg(t: DateTime<Utc>, lat: f64, lon: f64) -> f64 {
    // Fractional days since J2000.0 (JD 2451545.0).
    let n = t.timestamp_millis() as f64 / 86_400_000.0 + 2_440_587.5 - 2_451_545.0;
    let mean_long = (280.460 + 0.985_647_4 * n).rem_euclid(360.0);
    let mean_anom = (357.528 + 0.985_600_3 * n).rem_euclid(360.0).to_radians();
    let ecl_long =
        (mean_long + 1.915 * mean_anom.sin() + 0.020 * (2.0 * mean_anom).sin()).to_radians();
    let obliquity = (23.439 - 0.000_000_4 * n).to_radians();
    let declination = (obliquity.sin() * ecl_long.sin()).asin();
    let right_ascension = f64::atan2(obliquity.cos() * ecl_long.sin(), ecl_long.cos());
    let gmst_deg = (280.460_618_37 + 360.985_647_366_29 * n).rem_euclid(360.0);
    let hour_angle = (gmst_deg + lon - right_ascension.to_degrees())
        .rem_euclid(360.0)
        .to_radians();
    let lat_r = lat.to_radians();
    (lat_r.sin() * declination.sin() + lat_r.cos() * declination.cos() * hour_angle.cos())
        .asin()
        .to_degrees()
}

/// Sunrise/sunset instants for the UTC dates `d-1 ..= d+1` around `now`,
/// sorted, each tagged with the theme in effect AFTER it. All comparisons are
/// on UTC instants — an event must never be assumed to fall on any particular
/// LOCAL calendar date (`sun_times` takes a UTC date and keys events to the
/// solar day: in UTC+13/+14 the events for UTC date d land on local d+1, and
/// near the arctic circle a sunset crosses local midnight).
fn transitions_window(now: DateTime<Utc>, lat: f64, lon: f64) -> Vec<(DateTime<Utc>, Theme)> {
    let base = now.date_naive();
    let mut events = Vec::with_capacity(6);
    for off in -1..=1 {
        let date = base + chrono::Duration::days(off);
        if let Some((sunrise, sunset)) = sun_times(date, lat, lon, 0.0) {
            events.push((sunrise, Theme::Light));
            events.push((sunset, Theme::Dark));
        }
    }
    events.sort_by_key(|&(t, _)| t);
    events
}

/// Current theme and next transition instant — the single source of truth
/// for tick(). When the ±1-day window has no usable events (polar day/night),
/// the current state comes from the solar altitude and the next transition
/// from a forward scan.
fn schedule(now: DateTime<Utc>, lat: f64, lon: f64) -> (Theme, DateTime<Utc>) {
    let window = transitions_window(now, lat, lon);
    let current = window
        .iter()
        .rev()
        .find(|&&(t, _)| t <= now)
        .map(|&(_, theme)| theme)
        .unwrap_or_else(|| {
            if solar_altitude_deg(now, lat, lon) > SUNRISE_ALTITUDE_DEG {
                Theme::Light
            } else {
                Theme::Dark
            }
        });
    let next = window
        .iter()
        .find(|&&(t, _)| t > now)
        .map(|&(t, _)| t)
        .unwrap_or_else(|| next_transition_beyond_window(now, lat, lon));
    (current, next)
}

/// Forward scan for the first transition after a polar day/night period.
/// 200 days covers even the poles' ~6-month seasons; each probe is pure math.
fn next_transition_beyond_window(now: DateTime<Utc>, lat: f64, lon: f64) -> DateTime<Utc> {
    let base = now.date_naive();
    for off in 2..=200 {
        if let Some((sunrise, sunset)) =
            sun_times(base + chrono::Duration::days(off), lat, lon, 0.0)
        {
            if sunrise > now {
                return sunrise;
            }
            if sunset > now {
                return sunset;
            }
        }
    }
    now + chrono::Duration::days(1)
}

fn deadline_instant(target: DateTime<Local>) -> Instant {
    let now = Local::now();
    let delta = (target - now).to_std().unwrap_or(Duration::from_secs(1));
    Instant::now() + delta
}

fn make_tray_icon() -> Option<tray_icon::Icon> {
    const SIZE: u32 = 32;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    let cx = SIZE as f32 / 2.0;
    let cy = SIZE as f32 / 2.0;
    let r = SIZE as f32 / 2.0 - 1.5;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            let idx = ((y * SIZE + x) * 4) as usize;
            if d <= r {
                if dx < 0.0 {
                    rgba[idx] = 255;
                    rgba[idx + 1] = 140;
                    rgba[idx + 2] = 0;
                } else {
                    rgba[idx] = 44;
                    rgba[idx + 1] = 62;
                    rgba[idx + 2] = 100;
                }
                rgba[idx + 3] = 255;
            }
        }
    }
    tray_icon::Icon::from_rgba(rgba, SIZE, SIZE).ok()
}

fn tick(cfg: &Config, elwt: &ActiveEventLoop, kind: TickKind, cause: &str, state: &mut TickState) {
    let now = Local::now();
    let now_str = now.to_rfc3339();

    if !cfg.has_location() {
        log_event(&format!("{} cause={} skipped=no-location", now_str, cause));
        elwt.set_control_flow(ControlFlow::Wait);
        return;
    }

    let now_utc = now.with_timezone(&Utc);
    let (want, next_utc) = schedule(now_utc, cfg.latitude, cfg.longitude);
    let current = current_theme();
    let next = next_utc.with_timezone(&Local);

    // Decide with the PRE-tick state; record the outcome after the apply
    // result is known (a failed apply must leave reconciled_next stale — see
    // TickState).
    let action = decide_tick(kind, current, want, now_utc, state);

    let mut retry_note = String::new();
    let mut deadline = next;
    let outcome = match action {
        TickAction::Apply => {
            if kind == TickKind::Refresh {
                // Fresh user intent (likely a just-fixed config): a Refresh
                // never inherits a burned retry budget.
                state.retry_count = 0;
                state.retry_baseline = None;
                state.episode_next = None;
            }
            match apply_theme(want, cfg) {
                Ok(method) => {
                    note_reconciled(state, next_utc);
                    format!("applied={}", method)
                }
                Err(e) => {
                    // Bounded retry: reschedule soon instead of silently
                    // waiting up to ~12 h for the next transition. The retry
                    // arrives as a normal ResumeTimeReached tick; the
                    // pending-retry gate in decide_tick stands it down if
                    // the user changes the theme in the meantime.
                    if note_apply_failed(state, current, next_utc) {
                        retry_note = format!(" retry={}", state.retry_count);
                        deadline = retry_deadline(Local::now(), next);
                    } else {
                        retry_note = " retry=exhausted".to_string();
                    }
                    format!("err=\"{}\"", sanitize_log_msg(&e.to_string()))
                }
            }
        }
        TickAction::SkipInSync => {
            note_reconciled(state, next_utc);
            "applied=skip".to_string()
        }
        TickAction::SkipOverride => {
            note_reconciled(state, next_utc);
            "applied=skip-override".to_string()
        }
        TickAction::CancelRetry => {
            note_reconciled(state, next_utc);
            "applied=skip-user-intervened".to_string()
        }
    };

    // Stamp at write time, not tick start — apply_theme logs detail lines
    // (theme_manager2_apply, theme_manager2_err) mid-tick, and reusing the
    // tick-start timestamp here made this summary line sort before them.
    log_event(&format!(
        "{} cause={} current={} target={} {}{} next={}",
        Local::now().to_rfc3339(),
        cause,
        theme_str(current),
        theme_str(Some(want)),
        outcome,
        retry_note,
        deadline.to_rfc3339(),
    ));

    elwt.set_control_flow(ControlFlow::WaitUntil(deadline_instant(deadline)));
}

unsafe extern "system" fn wake_window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let kind = match msg {
        WM_WTSSESSION_CHANGE if wparam == WTS_SESSION_UNLOCK => Some(WakeKind::Unlock),
        WM_POWERBROADCAST if wparam == PBT_APMRESUMEAUTOMATIC => Some(WakeKind::Power),
        _ => None,
    };
    if let Some(k) = kind {
        if let Some(proxy) = EVENT_PROXY.get() {
            let _ = proxy.send_event(AppEvent::Wake(k));
        }
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn start_wake_listener() {
    std::thread::spawn(|| {
        let class_name = wide("WinThemeSwitcherWakeListener");
        unsafe {
            let hinstance = GetModuleHandleW(ptr::null());
            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.lpfnWndProc = Some(wake_window_proc);
            wc.hInstance = hinstance;
            wc.lpszClassName = class_name.as_ptr();
            if RegisterClassW(&wc) == 0 {
                // Capture immediately: Local::now()/log_event make Win32
                // calls that clobber the thread's last error.
                let err = GetLastError();
                log_event(&format!(
                    "{} wake_listener_err stage=register_class code={}",
                    Local::now().to_rfc3339(),
                    err,
                ));
                // CreateWindowExW will fail below and log; fall through.
            }

            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                ptr::null_mut(),
                hinstance,
                ptr::null(),
            );
            if (hwnd as usize) == 0 {
                let err = GetLastError();
                log_event(&format!(
                    "{} wake_listener_err stage=create_window code={}",
                    Local::now().to_rfc3339(),
                    err,
                ));
                return;
            }
            // Failures below degrade wake coverage (unlock or resume events
            // won't arrive) but don't kill the listener thread — log each so
            // a missing wake-tick has a diagnosable trace instead of silence.
            //
            // WTSRegisterSessionNotification depends on the terminal-services
            // machinery, which may not be up yet when we auto-start at logon
            // via HKCU\Run — retry briefly before settling for the log line.
            for attempt in 1..=3 {
                if WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) != 0 {
                    if attempt > 1 {
                        log_event(&format!(
                            "{} wake_listener_wts_ok attempt={}",
                            Local::now().to_rfc3339(),
                            attempt,
                        ));
                    }
                    break;
                }
                let err = GetLastError();
                log_event(&format!(
                    "{} wake_listener_err stage=wts_register attempt={} code={}",
                    Local::now().to_rfc3339(),
                    attempt,
                    err,
                ));
                std::thread::sleep(Duration::from_secs(2));
            }
            let mut handle = ptr::null_mut();
            let power_rc = PowerRegisterSuspendResumeNotification(
                DEVICE_NOTIFY_WINDOW_HANDLE,
                hwnd as _,
                &mut handle,
            );
            if power_rc != 0 {
                log_event(&format!(
                    "{} wake_listener_err stage=power_register code={}",
                    Local::now().to_rfc3339(),
                    power_rc,
                ));
            }

            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    });
}

/// Install a panic hook that leaves a trace in events.log. With
/// `panic = "abort"` and the windowed subsystem, an unhooked panic is a
/// zero-trace process death.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        // location() is compile-time data (survives strip = true) and often
        // the only actionable part; payload_as_str covers &str and String
        // panics, which is all this codebase produces.
        let at = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".to_string());
        let msg = info.payload_as_str().unwrap_or("<non-string panic>");
        log_event(&format!(
            "{} panic at={} msg=\"{}\"",
            Local::now().to_rfc3339(),
            at,
            sanitize_log_msg(msg),
        ));
    }));
}

/// Claim the per-session single-instance mutex. Returns false when another
/// instance already holds it. The handle is intentionally leaked — it must
/// live exactly as long as the process.
fn claim_single_instance() -> bool {
    let name = wide("Local\\WinThemeSwitcher.single-instance");
    unsafe {
        let handle = CreateMutexW(ptr::null(), 0, name.as_ptr());
        let last = GetLastError();
        if (handle as usize) != 0 && last == ERROR_ALREADY_EXISTS {
            // Second instance; the OS closes the extra handle at process exit.
            return false;
        }
        if (handle as usize) == 0 {
            // Failing to create the mutex is no reason to refuse to run.
            log_event(&format!(
                "{} single_instance_err code={}",
                Local::now().to_rfc3339(),
                last,
            ));
        }
        true
    }
}

fn main() {
    install_panic_hook();
    if !claim_single_instance() {
        log_event(&format!(
            "{} duplicate_instance action=exit",
            Local::now().to_rfc3339(),
        ));
        show_message_box(
            "WinThemeSwitcher",
            "WinThemeSwitcher is already running — look for its icon in the \
             notification area.",
            MB_OK | MB_ICONINFORMATION,
        );
        std::process::exit(0);
    }
    if let Err(e) = run() {
        // Fail loudly: tray creation racing the taskbar at login, event-loop
        // build errors, and event-loop death all used to be silent exits.
        let msg = sanitize_log_msg(&e.to_string());
        log_event(&format!(
            "{} fatal_error msg=\"{}\"",
            Local::now().to_rfc3339(),
            msg,
        ));
        show_message_box(
            "WinThemeSwitcher — Error",
            &format!(
                "WinThemeSwitcher stopped because of an error:\n\n{e}\n\n\
                 See events.log next to the exe for details."
            ),
            MB_OK | MB_ICONWARNING,
        );
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    ensure_com_initialized();

    // On a broken config file the session runs read-only against it: theme
    // switching continues with in-memory defaults + best-effort coordinates,
    // but nothing is persisted (acquire_location saves on success, which
    // would overwrite the very file the user needs to fix) and the autostart
    // registration is left exactly as the user last set it (the fallback
    // default auto_start=true must not override a broken file's false).
    let mut cfg = match load_config_at(&config_path()) {
        Ok(mut cfg) => {
            if !cfg.has_location() {
                acquire_location(&mut cfg);
            }
            let _ = set_auto_start(cfg.auto_start);
            cfg
        }
        Err(e) => {
            report_config_error(&e);
            let mut cfg = Config::default();
            if let Some((lat, lon)) = try_get_windows_location() {
                cfg.latitude = lat;
                cfg.longitude = lon;
            }
            cfg
        }
    };

    let event_loop = EventLoop::<AppEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let _ = EVENT_PROXY.set(event_loop.create_proxy());
    start_wake_listener();

    let tray_menu = Menu::new();
    let toggle_i = MenuItem::new("Toggle Theme", true, None);
    let open_cfg_i = MenuItem::new("Open Config", true, None);
    let refresh_i = MenuItem::new("Refresh", true, None);
    let quit_i = MenuItem::new("Quit", true, None);
    tray_menu.append_items(&[
        &toggle_i,
        &open_cfg_i,
        &refresh_i,
        &PredefinedMenuItem::separator(),
        &quit_i,
    ])?;

    let mut tray_builder = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_tooltip("WinThemeSwitcher");
    if let Some(icon) = make_tray_icon() {
        tray_builder = tray_builder.with_icon(icon);
    }
    let _tray = tray_builder.build()?;

    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let _ = proxy.send_event(AppEvent::Menu(event.id));
    }));

    let toggle_id = toggle_i.id().clone();
    let open_cfg_id = open_cfg_i.id().clone();
    let refresh_id = refresh_i.id().clone();
    let quit_id = quit_i.id().clone();

    let mut state = TickState::new();

    event_loop.run(move |event, elwt| match event {
        Event::NewEvents(StartCause::Init) => tick(&cfg, elwt, TickKind::Init, "init", &mut state),
        Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
            tick(&cfg, elwt, TickKind::Scheduled, "resume-time", &mut state);
        }
        Event::UserEvent(AppEvent::Wake(kind)) => {
            let cause = match kind {
                WakeKind::Unlock => "wake-unlock",
                WakeKind::Power => "wake-power",
            };
            tick(&cfg, elwt, TickKind::Wake, cause, &mut state);
        }
        Event::UserEvent(AppEvent::Menu(id)) => {
            if id == quit_id {
                elwt.exit();
            } else if id == open_cfg_id {
                open_config_in_editor();
            } else if id == toggle_id {
                // A deliberate manual override: applies the opposite theme
                // and intentionally does NOT tick, touch TickState (it
                // tracks reconciliation with the schedule, not the screen),
                // or disturb the pending WaitUntil — so the override
                // survives lock/unlock (see decide_tick) and resets at the
                // next natural transition, exactly like an override made in
                // Settings. If a failed-apply retry is pending, the toggled
                // theme diverges from the retry baseline and the
                // pending-retry gate stands the retry down.
                let before = current_theme();
                let target = toggle_target(before);
                let outcome = match apply_theme(target, &cfg) {
                    Ok(method) => format!("applied={}", method),
                    Err(e) => format!("err=\"{}\"", sanitize_log_msg(&e.to_string())),
                };
                log_event(&format!(
                    "{} cause=toggle current={} target={} {}",
                    Local::now().to_rfc3339(),
                    theme_str(before),
                    theme_str(Some(target)),
                    outcome,
                ));
            } else if id == refresh_id {
                match load_config_at(&config_path()) {
                    Ok(new_cfg) => {
                        cfg = new_cfg;
                        if !cfg.has_location() {
                            if let Some((lat, lon)) = try_get_windows_location() {
                                cfg.latitude = lat;
                                cfg.longitude = lon;
                                let _ = save_config(&cfg);
                            }
                        }
                    }
                    // Keep the last-known-good config; the broken file stays
                    // on disk for the user to fix.
                    Err(e) => report_config_error(&e),
                }
                // Runs in both arms: Refresh re-asserting the Run value from
                // the (possibly last-known-good) config is the documented
                // recovery path when e.g. an AV quarantine deletes it.
                let _ = set_auto_start(cfg.auto_start);
                tick(&cfg, elwt, TickKind::Refresh, "refresh", &mut state);
            }
        }
        _ => {}
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    fn mins(m: i64) -> chrono::Duration {
        chrono::Duration::minutes(m)
    }

    // Apia, Samoa — UTC+13, west of the antimeridian. Regression fixture for
    // the wrong-solar-day bug: the old code passed the LOCAL date to
    // sun_times (which wants a UTC date), so every event landed on the wrong
    // local day and the app was permanently dark here.
    const APIA: (f64, f64) = (-13.83, -171.77);
    // Riyadh — baseline the deployed build has verified for months.
    const RIYADH: (f64, f64) = (24.753, 46.765);
    // Reykjavik — UTC+0 year-round; in June sunset falls just past midnight.
    const REYKJAVIK: (f64, f64) = (64.147, -21.94);
    // Tromsø — above the arctic circle: midnight sun in June, polar night in December.
    const TROMSO: (f64, f64) = (69.65, 18.96);

    #[test]
    fn apia_noon_is_light() {
        // 2026-01-15 12:00 local (UTC+13) = 2026-01-14 23:00 UTC
        let now = utc(2026, 1, 14, 23, 0, 0);
        let (theme, next) = schedule(now, APIA.0, APIA.1);
        assert_eq!(theme, Theme::Light);
        // next transition is that evening's sunset (~19:10 local)
        assert!(next > now && next - now < mins(9 * 60), "next = {next}");
    }

    #[test]
    fn apia_night_is_dark() {
        // 2026-01-15 22:00 local = 2026-01-15 09:00 UTC
        let now = utc(2026, 1, 15, 9, 0, 0);
        let (theme, next) = schedule(now, APIA.0, APIA.1);
        assert_eq!(theme, Theme::Dark);
        // next transition is the ~06:20 local sunrise
        assert!(next > now && next - now < mins(9 * 60), "next = {next}");
    }

    #[test]
    fn riyadh_matches_deployed_log() {
        // The deployed build logged next=2026-07-04T18:46:05+03:00 (15:46:05Z)
        // for a mid-day tick. Riyadh is a same-day timezone, where the old
        // math was correct — the new math must agree with it.
        let now = utc(2026, 7, 4, 9, 0, 0); // 12:00 local
        let (theme, next) = schedule(now, RIYADH.0, RIYADH.1);
        assert_eq!(theme, Theme::Light);
        let expected = utc(2026, 7, 4, 15, 46, 5);
        assert!((next - expected).abs() < mins(5), "next = {next}");
    }

    #[test]
    fn riyadh_evening_is_dark_until_sunrise() {
        let now = utc(2026, 7, 4, 19, 0, 0); // 22:00 local
        let (theme, next) = schedule(now, RIYADH.0, RIYADH.1);
        assert_eq!(theme, Theme::Dark);
        // sunrise is ~05:35 local = 02:35Z, ~7.6 h away
        assert!(next > now && next - now < mins(11 * 60), "next = {next}");
    }

    #[test]
    fn reykjavik_june_sunset_crosses_midnight() {
        // Sun sets a few minutes past local midnight on June 21; at 23:30 on
        // June 20 it is still up. The old single-local-date math missed the
        // post-midnight sunset entirely.
        let now = utc(2026, 6, 20, 23, 30, 0);
        let (theme, next) = schedule(now, REYKJAVIK.0, REYKJAVIK.1);
        assert_eq!(theme, Theme::Light);
        assert!(
            next - now < mins(120),
            "sunset should be < 2h away, next = {next}"
        );
        // Just after that sunset: dark until the ~03:00 sunrise.
        let later = next + mins(1);
        let (theme2, next2) = schedule(later, REYKJAVIK.0, REYKJAVIK.1);
        assert_eq!(theme2, Theme::Dark);
        assert!(
            next2 > later && next2 - later < mins(4 * 60),
            "next2 = {next2}"
        );
    }

    #[test]
    fn tromso_midnight_sun_is_light_with_far_next() {
        let now = utc(2026, 6, 21, 12, 0, 0);
        let (theme, next) = schedule(now, TROMSO.0, TROMSO.1);
        assert_eq!(theme, Theme::Light);
        // Polar day runs to ~late July — the next transition is weeks away
        // and must come from the forward scan, not a 24 h fallback.
        assert!(next - now > mins(5 * 24 * 60), "next = {next}");
        assert!(next - now < mins(60 * 24 * 60), "next = {next}");
    }

    #[test]
    fn tromso_polar_night_is_dark() {
        let now = utc(2026, 12, 21, 12, 0, 0);
        let (theme, next) = schedule(now, TROMSO.0, TROMSO.1);
        assert_eq!(theme, Theme::Dark);
        assert!(next > now);
    }

    #[test]
    fn theme_flips_exactly_at_transition() {
        let now = utc(2026, 7, 4, 9, 0, 0);
        let (_, next) = schedule(now, RIYADH.0, RIYADH.1);
        let (before, _) = schedule(next - mins(1), RIYADH.0, RIYADH.1);
        let (at, next_after) = schedule(next, RIYADH.0, RIYADH.1);
        assert_eq!(before, Theme::Light);
        assert_eq!(at, Theme::Dark);
        assert!(next_after > next);
    }

    /// Temp file that cleans up after itself. Uniqueness comes from the
    /// caller-supplied `name` — every test must pass a distinct one or the
    /// parallel runner will clobber files across tests. The pid only guards
    /// against two simultaneous `cargo test` processes.
    struct TempConfig(PathBuf);

    impl TempConfig {
        fn new(name: &str, content: Option<&str>) -> Self {
            let path = std::env::temp_dir().join(format!(
                "wts-test-{}-{}-config.json",
                std::process::id(),
                name
            ));
            let _ = fs::remove_file(&path);
            if let Some(c) = content {
                fs::write(&path, c).unwrap();
            }
            Self(path)
        }
    }

    impl Drop for TempConfig {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn config_missing_file_creates_defaults() {
        let t = TempConfig::new("missing", None);
        let cfg = load_config_at(&t.0).expect("first run must succeed");
        assert!(!cfg.has_location());
        assert!(cfg.auto_start);
        // First run writes the defaults file, and it must round-trip.
        let written = fs::read_to_string(&t.0).expect("defaults file must be created");
        let reparsed: Config = serde_json::from_str(&written).unwrap();
        assert!(!reparsed.has_location());
    }

    #[test]
    fn config_valid_file_loads_values_and_stays_untouched() {
        let json = r#"{
  "latitude": 24.753,
  "longitude": 46.765,
  "auto_start": false,
  "theme_day": "C:\\Themes\\day.theme",
  "theme_night": null
}"#;
        let t = TempConfig::new("valid", Some(json));
        let cfg = load_config_at(&t.0).expect("valid config must parse");
        assert_eq!(cfg.latitude, 24.753);
        assert_eq!(cfg.longitude, 46.765);
        assert!(!cfg.auto_start);
        assert_eq!(cfg.theme_day.as_deref(), Some("C:\\Themes\\day.theme"));
        // Loading must never rewrite a readable file — byte-identical.
        assert_eq!(fs::read_to_string(&t.0).unwrap(), json);
    }

    #[test]
    fn config_parse_error_is_reported_and_file_kept() {
        // The roadmap's exact failure: a hand-edited theme path with single
        // backslashes is invalid JSON. This must NOT be reset to defaults.
        let json =
            r#"{"latitude": 24.7, "longitude": 46.7, "theme_night": "C:\Tools\night.theme"}"#;
        let t = TempConfig::new("broken", Some(json));
        let err = load_config_at(&t.0).expect_err("parse error must be reported, not defaulted");
        assert!(!err.is_empty());
        assert_eq!(
            fs::read_to_string(&t.0).unwrap(),
            json,
            "a broken config file must be left byte-identical on disk"
        );
    }

    #[test]
    fn config_empty_file_is_healed_to_defaults() {
        // A crash mid-write (fs::write truncates first) leaves a 0-byte
        // config.json. There is nothing in it to preserve, so it must
        // self-heal like first run instead of erroring on every launch.
        let t = TempConfig::new("empty", Some("  \n"));
        let cfg = load_config_at(&t.0).expect("empty file must heal to defaults");
        assert!(!cfg.has_location());
        assert!(cfg.auto_start);
        let written = fs::read_to_string(&t.0).unwrap();
        let reparsed: Config = serde_json::from_str(&written).unwrap();
        assert!(!reparsed.has_location());
    }

    #[test]
    fn config_unknown_and_missing_fields_are_defaulted() {
        let json = r#"{"latitude": 1.0, "some_future_field": true}"#;
        let t = TempConfig::new("partial", Some(json));
        let cfg = load_config_at(&t.0).expect("unknown/missing fields are not errors");
        assert_eq!(cfg.latitude, 1.0);
        assert_eq!(cfg.longitude, 0.0);
        assert!(cfg.auto_start, "missing fields fall back to defaults");
        assert_eq!(fs::read_to_string(&t.0).unwrap(), json);
    }

    #[test]
    fn solar_altitude_sanity() {
        // Riyadh at local solar noon in July: sun nearly overhead (~88°).
        assert!(solar_altitude_deg(utc(2026, 7, 4, 8, 53, 0), RIYADH.0, RIYADH.1) > 80.0);
        // Tromsø, December noon: polar night — below the sunrise threshold.
        assert!(
            solar_altitude_deg(utc(2026, 12, 21, 11, 0, 0), TROMSO.0, TROMSO.1)
                < SUNRISE_ALTITUDE_DEG
        );
        // Tromsø, June, near local solar midnight: midnight sun stays up (~3°).
        assert!(solar_altitude_deg(utc(2026, 6, 20, 22, 45, 0), TROMSO.0, TROMSO.1) > 0.0);
    }

    // --- tick decision: manual-override preservation (v0.4.0) ---

    /// A reconciled state whose recorded next transition is at `next`.
    fn reconciled_at(next: DateTime<Utc>) -> TickState {
        let mut s = TickState::new();
        note_reconciled(&mut s, next);
        s
    }

    #[test]
    fn refresh_forces_even_when_matching() {
        // Refresh force-applies so config edits take effect immediately —
        // CLAUDE.md invariant. Must beat the in-sync skip.
        let s = reconciled_at(utc(2026, 7, 4, 15, 46, 5));
        let now = utc(2026, 7, 4, 9, 0, 0);
        assert_eq!(
            decide_tick(TickKind::Refresh, Some(Theme::Light), Theme::Light, now, &s),
            TickAction::Apply
        );
    }

    #[test]
    fn matching_current_skips_apply() {
        let sunset = utc(2026, 7, 4, 15, 46, 5);
        let now = utc(2026, 7, 4, 9, 0, 0);
        let s = reconciled_at(sunset);
        for kind in [TickKind::Init, TickKind::Scheduled, TickKind::Wake] {
            assert_eq!(
                decide_tick(kind, Some(Theme::Light), Theme::Light, now, &s),
                TickAction::SkipInSync,
                "kind {kind:?}"
            );
        }
    }

    #[test]
    fn scheduled_transition_applies_over_override() {
        // A natural transition resets any manual override — documented
        // behavior. The sunset tick fires at/after the recorded next.
        let sunset = utc(2026, 7, 4, 15, 46, 5);
        let s = reconciled_at(sunset);
        assert_eq!(
            decide_tick(
                TickKind::Scheduled,
                Some(Theme::Light),
                Theme::Dark,
                sunset,
                &s
            ),
            TickAction::Apply
        );
    }

    #[test]
    fn wake_before_next_transition_preserves_override() {
        // Mid-day the user picked Dark (schedule says Light until sunset).
        // Win+L → unlock before sunset: no transition passed → override
        // survives.
        let sunset = utc(2026, 7, 4, 15, 46, 5);
        let s = reconciled_at(sunset);
        let now = utc(2026, 7, 4, 12, 0, 0);
        assert_eq!(
            decide_tick(TickKind::Wake, Some(Theme::Dark), Theme::Light, now, &s),
            TickAction::SkipOverride
        );
    }

    #[test]
    fn wake_after_missed_transition_reconciles() {
        // Slept through sunset: now >= recorded next → reconcile.
        let sunset = utc(2026, 7, 4, 15, 46, 5);
        let s = reconciled_at(sunset);
        let now = utc(2026, 7, 4, 20, 0, 0);
        assert_eq!(
            decide_tick(TickKind::Wake, Some(Theme::Light), Theme::Dark, now, &s),
            TickAction::Apply
        );
    }

    #[test]
    fn wake_after_even_number_of_missed_transitions_reconciles() {
        // Overnight lock spanning sunset AND sunrise: the schedule's target
        // is back to Light — same THEME as when we reconciled, but two
        // transitions passed. A parity comparison of themes would wrongly
        // preserve yesterday's override; the time rule must reconcile.
        let sunset = utc(2026, 7, 4, 15, 46, 5);
        let s = reconciled_at(sunset);
        let next_morning = utc(2026, 7, 5, 4, 0, 0);
        assert_eq!(
            decide_tick(
                TickKind::Wake,
                Some(Theme::Dark), // yesterday's override, still on screen
                Theme::Light,
                next_morning,
                &s
            ),
            TickAction::Apply
        );
    }

    #[test]
    fn wake_without_baseline_reconciles() {
        // A wake before any reconciled tick (e.g. Init hit the no-location
        // path): the safe default is reconcile, not preserve.
        let s = TickState::new();
        let now = utc(2026, 7, 4, 12, 0, 0);
        assert_eq!(
            decide_tick(TickKind::Wake, Some(Theme::Dark), Theme::Light, now, &s),
            TickAction::Apply
        );
    }

    #[test]
    fn wake_with_unreadable_current_skips_inside_window() {
        // current_theme() = None (registry read failed): inside a preserved
        // window the app declines to apply — pinned deliberately.
        let sunset = utc(2026, 7, 4, 15, 46, 5);
        let s = reconciled_at(sunset);
        let now = utc(2026, 7, 4, 12, 0, 0);
        assert_eq!(
            decide_tick(TickKind::Wake, None, Theme::Light, now, &s),
            TickAction::SkipOverride
        );
    }

    #[test]
    fn theme_opposite_flips() {
        assert_eq!(Theme::Light.opposite(), Theme::Dark);
        assert_eq!(Theme::Dark.opposite(), Theme::Light);
    }

    #[test]
    fn toggle_target_flips_current_and_defaults_dark() {
        assert_eq!(toggle_target(Some(Theme::Light)), Theme::Dark);
        assert_eq!(toggle_target(Some(Theme::Dark)), Theme::Light);
        // Unreadable current: base defaults to Light → toggle lands on Dark.
        assert_eq!(toggle_target(None), Theme::Dark);
    }

    #[test]
    fn sanitize_log_msg_keeps_field_parseable() {
        assert_eq!(
            sanitize_log_msg("bad \"path\" at\nline\r\ntwo"),
            "bad 'path' at line  two"
        );
    }

    // --- bounded apply retry (v0.4.0) ---

    #[test]
    fn retry_deadline_is_soon_but_never_past_next_transition() {
        let now = utc(2026, 7, 4, 9, 0, 0).with_timezone(&Local);
        let far_next = utc(2026, 7, 4, 15, 0, 0).with_timezone(&Local);
        let near_next = utc(2026, 7, 4, 9, 0, 30).with_timezone(&Local);
        assert_eq!(
            retry_deadline(now, far_next),
            now + chrono::Duration::seconds(APPLY_RETRY_DELAY_SECS)
        );
        assert_eq!(retry_deadline(now, near_next), near_next);
    }

    #[test]
    fn failed_apply_then_wake_must_reapply_not_preserve() {
        // THE design-review blocker: a failed apply must never be mistaken
        // for a user override. Sequence: noon tick reconciles; sunset passes
        // while asleep; resume tick's apply FAILS; seconds later the unlock
        // wake fires — it must Apply (acting as a free retry), not skip.
        let sunset = utc(2026, 7, 4, 15, 46, 5);
        let mut s = reconciled_at(sunset);
        let resume_at = utc(2026, 7, 4, 17, 0, 0);
        assert_eq!(
            decide_tick(
                TickKind::Wake,
                Some(Theme::Light),
                Theme::Dark,
                resume_at,
                &s
            ),
            TickAction::Apply
        );
        assert!(note_apply_failed(
            &mut s,
            Some(Theme::Light),
            utc(2026, 7, 5, 2, 35, 0)
        ));
        let unlock_at = utc(2026, 7, 4, 17, 0, 10);
        assert_eq!(
            decide_tick(
                TickKind::Wake,
                Some(Theme::Light),
                Theme::Dark,
                unlock_at,
                &s
            ),
            TickAction::Apply,
            "wake after failed apply must re-apply, not preserve the failure"
        );
    }

    #[test]
    fn user_intervention_during_retry_window_stands_down() {
        // Sunset apply fails (screen stuck Light); the user then explicitly
        // picks a theme. The pending retry must cancel instead of clobbering
        // their choice — and afterwards the override survives normally.
        let sunset = utc(2026, 7, 4, 15, 46, 5);
        let mut s = reconciled_at(sunset);
        assert!(note_apply_failed(
            &mut s,
            Some(Theme::Light),
            utc(2026, 7, 5, 2, 35, 0)
        ));
        // User toggles to Dark (matches schedule — converged) or picks Light
        // again in Settings; either way the observed theme moved off the
        // failure baseline. Here: user picked Dark, so current == target.
        let retry_at = utc(2026, 7, 4, 16, 47, 5);
        assert_eq!(
            decide_tick(
                TickKind::Scheduled,
                Some(Theme::Dark),
                Theme::Dark,
                retry_at,
                &s
            ),
            TickAction::CancelRetry
        );
        note_reconciled(&mut s, utc(2026, 7, 5, 2, 35, 0));
        assert_eq!(s.retry_count, 0);
    }

    #[test]
    fn unreadable_current_does_not_cancel_retry() {
        // None proves nothing about user intent — the retry must proceed.
        let sunset = utc(2026, 7, 4, 15, 46, 5);
        let mut s = reconciled_at(sunset);
        assert!(note_apply_failed(
            &mut s,
            Some(Theme::Light),
            utc(2026, 7, 5, 2, 35, 0)
        ));
        let retry_at = utc(2026, 7, 4, 16, 47, 5);
        assert_eq!(
            decide_tick(TickKind::Scheduled, None, Theme::Dark, retry_at, &s),
            TickAction::Apply
        );
    }

    #[test]
    fn retry_budget_is_bounded_and_resets_per_episode() {
        let mut s = TickState::new();
        // Three failures schedule retries; the fourth gives up AND resets,
        // so the next transition window gets a fresh budget instead of
        // inheriting a permanently burned one.
        assert!(note_apply_failed(
            &mut s,
            Some(Theme::Light),
            utc(2026, 7, 5, 2, 35, 0)
        ));
        assert!(note_apply_failed(
            &mut s,
            Some(Theme::Light),
            utc(2026, 7, 5, 2, 35, 0)
        ));
        assert!(note_apply_failed(
            &mut s,
            Some(Theme::Light),
            utc(2026, 7, 5, 2, 35, 0)
        ));
        assert!(!note_apply_failed(
            &mut s,
            Some(Theme::Light),
            utc(2026, 7, 5, 2, 35, 0)
        ));
        assert_eq!(s.retry_count, 0);
        assert_eq!(s.retry_baseline, None);
        assert!(note_apply_failed(
            &mut s,
            Some(Theme::Light),
            utc(2026, 7, 5, 2, 35, 0)
        ));
    }

    #[test]
    fn reconcile_clears_retry_episode() {
        let next = utc(2026, 7, 5, 2, 35, 0);
        let mut s = TickState::new();
        assert!(note_apply_failed(&mut s, Some(Theme::Light), next));
        note_reconciled(&mut s, next);
        assert_eq!(s.retry_count, 0);
        assert_eq!(s.retry_baseline, None);
        assert_eq!(s.reconciled_next, Some(next));
    }

    // --- .theme DisplayName resolution (v0.4.0) ---

    /// Temp .theme file that cleans up after itself; same uniqueness contract
    /// as TempConfig (distinct `name` per test).
    struct TempTheme(PathBuf);

    impl TempTheme {
        fn new(name: &str, bytes: &[u8]) -> Self {
            let path = std::env::temp_dir().join(format!(
                "wts-test-{}-{}.theme",
                std::process::id(),
                name
            ));
            fs::write(&path, bytes).unwrap();
            Self(path)
        }
    }

    impl Drop for TempTheme {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn theme_display_name_literal() {
        let t = TempTheme::new(
            "literal",
            b"; comment\r\n[Theme]\r\nDisplayName=My Custom Theme\r\nColor=1\r\n",
        );
        assert_eq!(
            resolve_theme_display_name(&t.0).as_deref(),
            Some("My Custom Theme")
        );
    }

    #[test]
    fn theme_display_name_only_read_from_theme_section() {
        let t = TempTheme::new(
            "wrong-section",
            b"[Control Panel\\Desktop]\r\nDisplayName=Nope\r\n[Slideshow]\r\nInterval=1\r\n",
        );
        assert_eq!(resolve_theme_display_name(&t.0), None);
    }

    #[test]
    fn theme_display_name_section_header_case_insensitive() {
        let t = TempTheme::new("case", b"[THEME]\r\nDisplayName=Loud\r\n");
        assert_eq!(resolve_theme_display_name(&t.0).as_deref(), Some("Loud"));
    }

    #[test]
    fn theme_display_name_survives_windows_1252_comment_bytes() {
        // aero.theme's copyright comment carries a raw 0xa9 (©) — invalid
        // UTF-8. The lossy decode + comment skipping must not derail parsing.
        let t = TempTheme::new(
            "cp1252",
            b"; Copyright \xa9 Microsoft\r\n[Theme]\r\nDisplayName=Real\r\n",
        );
        assert_eq!(resolve_theme_display_name(&t.0).as_deref(), Some("Real"));
    }

    #[test]
    fn theme_display_name_missing_file_or_key_is_none() {
        let missing =
            std::env::temp_dir().join(format!("wts-test-{}-nonexistent.theme", std::process::id()));
        assert_eq!(resolve_theme_display_name(&missing), None);
        let t = TempTheme::new("no-name", b"[Theme]\r\nColor=1\r\n");
        assert_eq!(resolve_theme_display_name(&t.0), None);
    }

    #[test]
    fn system_theme_display_names_resolve_via_indirect_strings() {
        // Guarded: meaningful anywhere the stock themes exist (any normal
        // Windows, incl. GitHub windows-latest runners). Exercises the
        // SHLoadIndirectString path used for tier-1 apply of system themes.
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let aero = PathBuf::from(&root).join("Resources\\Themes\\aero.theme");
        let dark = PathBuf::from(&root).join("Resources\\Themes\\dark.theme");
        if !aero.exists() || !dark.exists() {
            // Rust has no test-skip; make the silent pass greppable so a
            // runner without stock themes doesn't hide that this is the only
            // coverage of resolve_indirect_string.
            eprintln!("SKIP: system .theme files absent — indirect-string path not exercised");
            return;
        }
        let a = resolve_theme_display_name(&aero).expect("aero.theme display name");
        let d = resolve_theme_display_name(&dark).expect("dark.theme display name");
        assert!(!a.is_empty() && !d.is_empty());
        assert_ne!(a, d);
    }

    // --- resolve_theme_file fallback chain (v0.4.0) ---

    #[test]
    fn theme_file_custom_path_wins_when_present() {
        let t = TempTheme::new("custom-day", b"[Theme]\r\nDisplayName=Custom\r\n");
        let cfg = Config {
            theme_day: Some(t.0.to_string_lossy().into_owned()),
            ..Config::default()
        };
        assert_eq!(resolve_theme_file(Theme::Light, &cfg), t.0);
    }

    #[test]
    fn theme_file_falls_back_when_custom_path_missing() {
        let cfg = Config {
            theme_night: Some("C:\\definitely\\not\\here.theme".into()),
            ..Config::default()
        };
        let p = resolve_theme_file(Theme::Dark, &cfg);
        assert!(p.ends_with("dark.theme"), "got {p:?}");
    }

    #[test]
    fn theme_file_defaults_by_theme() {
        let cfg = Config::default();
        assert!(resolve_theme_file(Theme::Light, &cfg).ends_with("aero.theme"));
        assert!(resolve_theme_file(Theme::Dark, &cfg).ends_with("dark.theme"));
    }
}
