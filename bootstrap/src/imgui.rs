//! The devs' own ImGui panels in the box (findings/0029, 0033). Retail registers every `ImGui*` Luau native as a no-op,
//! and Dear ImGui isn't linked. We swap the natives the scripts use for ours before the VM registers them: on the game
//! thread they record each frame's widget calls and answer with what the overlay saw last frame (a click, a new
//! value, whether a tree node is open). The overlay (hudhook, D3D11 or D3D12 Present) replays the latest recorded frame with
//! real Dear ImGui on the render thread, so the scripts' panels draw unchanged. F9 shows it and takes the mouse and
//! keyboard: window messages through hudhook's filter, DirectInput (what the game plays with) through our device hooks.

use hudhook::hooks::{dx11::ImguiDx11Hooks, dx12::ImguiDx12Hooks};
use hudhook::imgui::{sys, Context, Io, Ui};
use hudhook::{Hudhook, ImguiRenderLoop, MessageFilter, RenderContext};
use retour::RawDetour;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CString};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use super::{log, read_ptr, vfunc, wide, EXE, LUA_BASE, LUA_SETTOP_RVA, LUA_TOP, TAG_STRING, TSTRING_DATA, TSTRING_LEN, TVALUE_TAG};

/// The VM's native table: (u32 atom, u32 0, u64 fn) rows, zero-terminated, read by the VM setup 0x871548 (findings/0033).
const NATIVE_TABLE_RVA: usize = 0x0200_7500;
const NATIVE_ROWS: usize = 197;
pub(crate) const LUA_PUSHNIL_RVA: usize = 0x0139_e6b0; // (L)
pub(crate) const LUA_PUSHNIL_PROLOGUE: [u8; 16] =
    [0x48, 0x8b, 0x41, 0x08, 0xc7, 0x40, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x48, 0x83, 0x41, 0x08, 0x10];
pub(crate) const LUA_NEXT_RVA: usize = 0x00bb_a9d0; // (L, idx) -> 0 at the end; pushes key, value
pub(crate) const LUA_NEXT_PROLOGUE: [u8; 16] =
    [0x48, 0x89, 0x5c, 0x24, 0x08, 0x48, 0x89, 0x74, 0x24, 0x10, 0x57, 0x48, 0x83, 0xec, 0x20, 0xf6];
/// (L, engine string*): what the stubs return strings with (GC step, luaS_newlstr, push).
pub(crate) const PUSH_STRING_RVA: usize = 0x00e8_5620;
pub(crate) const PUSH_STRING_PROLOGUE: [u8; 16] =
    [0x48, 0x89, 0x5c, 0x24, 0x08, 0x48, 0x89, 0x74, 0x24, 0x10, 0x57, 0x48, 0x83, 0xec, 0x20, 0x48];
const TAG_NIL: i32 = 0;
const TAG_BOOLEAN: i32 = 1;
const TAG_NUMBER: i32 = 3; // this fork stores numbers as f32
const TAG_TABLE: i32 = 6;
const MAX_COMMANDS: usize = 20_000; // per frame; a runaway script stops recording instead of eating memory
const MAX_ITEMS: usize = 1000; // list entries read from a table
const VK_F9: i32 = 0x78;

type Native = unsafe extern "C" fn(*mut u8) -> i32;

/// Every ImGui native the 978 cache scripts call; names hash-verified with research/name_atoms.py (findings/0033).
const NATIVES: [(u32, &str, Native); 33] = [
    (0x489d_e21b, "ImGuiBegin", begin),
    (0x48bc_afdc, "ImGuiEnd", end),
    (0x05a8_56ad, "ImGuiText", text),
    (0xe56e_c6e9, "ImGuiTextWrapped", text_wrapped),
    (0x6708_a400, "ImGuiTextColored", text_colored),
    (0x4f36_59d1, "ImGuiSeparatorText", separator_text),
    (0x0794_7f07, "ImGuiSpacing", spacing),
    (0x78ae_ed31, "ImGuiSameLine", same_line),
    (0x80af_b308, "ImGuiButton", button),
    (0x2ebd_f85a, "ImGuiCollapsingHeader", collapsing_header),
    (0xd265_5286, "ImGuiCheckbox", checkbox),
    (0x3f9d_4ca0, "ImGuiSliderFloat", slider_float),
    (0x330b_d387, "ImGuiSliderInt", slider_int),
    (0x4f10_b8c4, "ImGuiInputText", input_text),
    (0xfdfa_b732, "ImGuiCombo", combo),
    (0x1e44_eb21, "ImGuiListBox", list_box),
    (0xb046_908b, "ImGuiRadioButtons", radio_buttons),
    (0x83a4_046b, "ImGuiBeginTabBar", begin_tab_bar),
    (0x8ec4_e5b4, "ImGuiEndTabBar", end_tab_bar),
    (0x19d0_2a1f, "ImGuiBeginTabItem", begin_tab_item),
    (0xc530_3493, "ImGuiEndTabItem", end_tab_item),
    (0x350a_8882, "ImGuiSetNextItemWidth", set_next_item_width),
    (0x4a1d_80a8, "ImGuiBeginDisabled", begin_disabled),
    (0x857d_7f2f, "ImGuiEndDisabled", end_disabled),
    (0xfca9_22b7, "ImGuiBeginTable", begin_table),
    (0x20c1_61dd, "ImGuiTableSetupColumn", table_setup_column),
    (0x04c2_d310, "ImGuiTableHeadersRow", table_headers_row),
    (0xefa1_c312, "ImGuiTableNextRow", table_next_row),
    (0xa9e3_d06e, "ImGuiTableSetColumnIndex", table_set_column_index),
    (0x3648_d9a4, "ImGuiEndTable", end_table),
    (0xc4e4_efbb, "ImGuiTreeNodeEx", tree_node_ex),
    (0xc7d8_5e6b, "ImGuiTreePop", tree_pop),
    (0xcc16_19d2, "ImGuiIsItemClicked", is_item_clicked),
];

/// Overlay shown (F9). While it is, it takes mouse and keyboard from the game.
pub(crate) static VISIBLE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Scope {
    Window,
    TabBar,
    TabItem,
    Table,
    Tree,
    Disabled,
}

#[derive(Debug)]
enum Op {
    /// `open` is what the script was told (so it drew the contents, or didn't); `arg` is per scope: window flags
    /// (CLOSABLE, HIDDEN), table columns, tree node flags, disabled.
    Open { scope: Scope, label: String, key: String, open: bool, arg: i32 },
    Close(Scope),
    Text(String),
    Wrapped(String),
    Colored([f32; 4], String),
    SeparatorText(String),
    Spacing,
    SameLine,
    Button { label: String, key: String },
    Header { label: String, key: String },
    Checkbox { label: String, key: String, value: bool },
    SliderFloat { label: String, key: String, value: f32, min: f32, max: f32 },
    SliderInt { label: String, key: String, value: i32, min: i32, max: i32 },
    InputText { label: String, key: String, text: String },
    /// Lists are 1-based, as the scripts index them.
    Combo { label: String, key: String, current: i32, items: Vec<String> },
    ListBox { label: String, key: String, current: i32, items: Vec<String>, height: i32 },
    Radio { key: String, current: i32, items: Vec<String> },
    ItemWidth(f32),
    Clicked { key: String, button: i32 },
    Column { label: String, width: f32 },
    HeadersRow,
    NextRow,
    ColumnIndex(i32),
}
const CLOSABLE: i32 = 1;
const HIDDEN: i32 = 2;
const NO_TREE_PUSH_ON_OPEN: i32 = 1 << 3; // ImGuiTreeNodeFlags_NoTreePushOnOpen

/// Render thread -> game thread, taken once by the native that asked.
enum Event {
    Clicked,
    Closed,
    Bool(bool),
    Float(f32),
    Int(i32),
    Text(String),
}

#[derive(Default)]
struct Recording {
    ops: Vec<Op>,
    windows: Vec<String>,
    seen: HashMap<String, u32>,
    last_key: String,
}

impl Recording {
    /// Widget identity: window + label (+ occurrence), the same thing ImGui's ID stack keys on.
    fn key(&mut self, label: &str) -> String {
        let base = format!("{}\x1f{label}", self.windows.last().map_or("", String::as_str));
        let n = self.seen.entry(base.clone()).or_insert(0);
        *n += 1;
        self.last_key = if *n == 1 { base } else { format!("{base}\x1f{n}") };
        self.last_key.clone()
    }

    fn push(&mut self, op: Op) {
        if self.ops.len() < MAX_COMMANDS {
            self.ops.push(op);
        }
    }
}

static RECORDING: LazyLock<Mutex<Recording>> = LazyLock::new(Default::default);
static PUBLISHED: LazyLock<Mutex<Arc<Vec<Op>>>> = LazyLock::new(Default::default);
static EVENTS: LazyLock<Mutex<HashMap<String, Event>>> = LazyLock::new(Default::default);
/// Scope state the overlay saw (window visible, tree/header/tab open), kept until the overlay says otherwise.
static OPEN: LazyLock<Mutex<HashMap<String, bool>>> = LazyLock::new(Default::default);

fn record<R>(f: impl FnOnce(&mut Recording) -> R) -> Option<R> {
    RECORDING.lock().ok().map(|mut r| f(&mut r))
}

fn take_event(key: &str) -> Option<Event> {
    EVENTS.lock().ok()?.remove(key)
}

fn is_open(key: &str, default: bool) -> bool {
    OPEN.lock().ok().and_then(|o| o.get(key).copied()).unwrap_or(default)
}

/// Swap our natives into the VM's native table. Runs before the VM exists (the table is read at VM setup).
pub(crate) unsafe fn install(exe: *const u8) -> Result<usize, String> {
    #[link(name = "kernel32")]
    extern "system" {
        fn VirtualProtect(at: *mut u8, len: usize, protect: u32, old: *mut u32) -> i32;
    }
    const PAGE_READWRITE: u32 = 0x04;
    let table = exe.add(NATIVE_TABLE_RVA).cast_mut();
    let len = NATIVE_ROWS * 16;
    let mut rows = Vec::new();
    for (atom, name, native) in NATIVES {
        let row = (0..NATIVE_ROWS)
            .map(|i| table.add(i * 16))
            .find(|row| row.cast::<u32>().read_unaligned() == atom)
            .ok_or(format!("{name} ({atom:08x}) not in the native table"))?;
        rows.push((row, native));
    }
    let mut old = 0;
    if VirtualProtect(table, len, PAGE_READWRITE, &mut old) == 0 {
        return Err("VirtualProtect failed".into());
    }
    for (row, native) in &rows {
        row.add(8).cast::<usize>().write_unaligned(*native as usize);
    }
    VirtualProtect(table, len, old, &mut old);
    Ok(rows.len())
}

/// Game thread, once per frame (player update): hand the frame's widget calls to the overlay.
pub(crate) fn publish_frame() {
    let Some(ops) = record(|r| {
        r.windows.clear();
        r.seen.clear();
        std::mem::take(&mut r.ops)
    }) else {
        return;
    };
    if let Ok(mut published) = PUBLISHED.lock() {
        *published = Arc::new(ops);
    }
}

/// F9: show the overlay (and take the mouse), or hide it. Polled from the player update.
pub(crate) unsafe fn poll_toggle() {
    if super::pressed(VK_F9, 2) {
        let on = !VISIBLE.fetch_xor(true, Relaxed);
        if on {
            ClipCursor(std::ptr::null()); // our ClipCursor hook keeps it free until the overlay hides
        }
        log(&format!("imgui: overlay {}", if on { "shown" } else { "hidden" }));
    }
}

// ---- the game's input while the overlay is shown ----
// The game plays with DirectInput 8 (EE.log "Found Keyboard (Enhanced), emulated"; the exe only has IID_IDirectInput8W,
// GUID_SysKeyboard and GUID_SysMouse). DirectInput reads the devices directly, never through the window procedure, so
// hudhook's message filter can't keep keys and mouse from the game. While the overlay is shown, the devices still poll
// (they stay acquired and their buffers drain) but hand the game nothing; its cursor calls are ignored, so ImGui's
// cursor moves freely.

const IID_IDIRECTINPUT8W: [u8; 16] = [0x31, 0x80, 0x79, 0xbf, 0x3a, 0x48, 0xa2, 0x4d, 0xaa, 0x99, 0x5d, 0x64, 0xed, 0x36, 0x97, 0x00];
const GUID_SYSKEYBOARD: [u8; 16] = [0x61, 0x2b, 0x1d, 0x6f, 0xa0, 0xd5, 0xcf, 0x11, 0xbf, 0xc7, 0x44, 0x45, 0x53, 0x54, 0x00, 0x00];
const DIRECTINPUT_VERSION: u32 = 0x0800;
// Byte offsets into the vtables: IDirectInput8W::CreateDevice; IDirectInputDevice8W (keyboard and mouse share it).
const CREATE_DEVICE: usize = 3 * 8;
const RELEASE: usize = 2 * 8;
const GET_DEVICE_STATE: usize = 9 * 8;
const GET_DEVICE_DATA: usize = 10 * 8;
const SET_COOPERATIVE_LEVEL: usize = 13 * 8;
const DISCL_EXCLUSIVE: u32 = 1;
const DISCL_NONEXCLUSIVE: u32 = 2;

type GetDeviceState = unsafe extern "system" fn(*mut c_void, u32, *mut u8) -> i32;
type GetDeviceData = unsafe extern "system" fn(*mut c_void, u32, *mut c_void, *mut u32, u32) -> i32;
type SetCooperativeLevel = unsafe extern "system" fn(*mut c_void, *mut c_void, u32) -> i32;
type SetCursorPosFn = unsafe extern "system" fn(i32, i32) -> i32;
type ClipCursorFn = unsafe extern "system" fn(*const c_void) -> i32;

static DEVICE_STATE: OnceLock<RawDetour> = OnceLock::new();
static DEVICE_DATA: OnceLock<RawDetour> = OnceLock::new();
static COOPERATIVE_LEVEL: OnceLock<RawDetour> = OnceLock::new();
static SET_CURSOR_POS: OnceLock<RawDetour> = OnceLock::new();
static CLIP_CURSOR: OnceLock<RawDetour> = OnceLock::new();

#[link(name = "user32")]
extern "system" {
    fn ClipCursor(rect: *const c_void) -> i32;
}

/// The DirectInput device vtable, from a keyboard device of our own (dinput8 stays loaded: we keep our reference).
unsafe fn dinput_device_vtable() -> Result<*const u8, String> {
    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryW(name: *const u16) -> *const u8;
        fn GetModuleHandleW(name: *const u16) -> *const u8;
    }
    type Create = unsafe extern "system" fn(*const u8, u32, *const [u8; 16], *mut *mut u8, *mut c_void) -> i32;
    type CreateDevice = unsafe extern "system" fn(*mut u8, *const [u8; 16], *mut *mut u8, *mut c_void) -> i32;
    type Release = unsafe extern "system" fn(*mut u8) -> u32;
    let dll = LoadLibraryW(wide::<12>("dinput8.dll").as_ptr());
    let create = if dll.is_null() { std::ptr::null() } else { super::GetProcAddress(dll, c"DirectInput8Create".as_ptr()) };
    if create.is_null() {
        return Err("no dinput8.dll / DirectInput8Create".into());
    }
    let create: Create = std::mem::transmute(create);
    let mut di = std::ptr::null_mut();
    let hr = create(GetModuleHandleW(std::ptr::null()), DIRECTINPUT_VERSION, &IID_IDIRECTINPUT8W, &mut di, std::ptr::null_mut());
    if hr < 0 || di.is_null() {
        return Err(format!("DirectInput8Create failed ({hr:#x})"));
    }
    let create_device: CreateDevice = std::mem::transmute(vfunc(di, CREATE_DEVICE));
    let mut device = std::ptr::null_mut();
    let hr = create_device(di, &GUID_SYSKEYBOARD, &mut device, std::ptr::null_mut());
    let vtable = if hr < 0 || device.is_null() { None } else { Some(read_ptr(device)) };
    if !device.is_null() {
        std::mem::transmute::<_, Release>(vfunc(device, RELEASE))(device);
    }
    std::mem::transmute::<_, Release>(vfunc(di, RELEASE))(di);
    vtable.ok_or(format!("CreateDevice(keyboard) failed ({hr:#x})"))
}

/// Hook the DirectInput device and the cursor calls. A failure only leaves the game's input running under the overlay.
unsafe fn hook_input() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *const u8;
    }
    let user32 = GetModuleHandleW(wide::<11>("user32.dll").as_ptr());
    let vtable = dinput_device_vtable().unwrap_or_else(|e| {
        log(&format!("imgui: {e}; DirectInput reaches the game under the overlay"));
        std::ptr::null()
    });
    let slot = |offset| if vtable.is_null() { std::ptr::null() } else { read_ptr(vtable.add(offset)).cast::<c_void>() };
    let hooks: [(&str, &OnceLock<RawDetour>, *const c_void, *const ()); 5] = [
        ("DirectInput GetDeviceState", &DEVICE_STATE, slot(GET_DEVICE_STATE), hook_get_device_state as _),
        ("DirectInput GetDeviceData", &DEVICE_DATA, slot(GET_DEVICE_DATA), hook_get_device_data as _),
        ("DirectInput SetCooperativeLevel", &COOPERATIVE_LEVEL, slot(SET_COOPERATIVE_LEVEL), hook_set_cooperative_level as _),
        ("SetCursorPos", &SET_CURSOR_POS, super::GetProcAddress(user32, c"SetCursorPos".as_ptr()), hook_set_cursor_pos as _),
        ("ClipCursor", &CLIP_CURSOR, super::GetProcAddress(user32, c"ClipCursor".as_ptr()), hook_clip_cursor as _),
    ];
    for (name, slot, target, detour) in hooks {
        let result = if target.is_null() {
            Err("not found".to_string())
        } else {
            RawDetour::new(target.cast(), detour).and_then(|d| slot.get_or_init(|| d).enable()).map_err(|e| e.to_string())
        };
        match result {
            Ok(()) => log(&format!("imgui: hooked {name}")),
            Err(e) => log(&format!("imgui: FAILED to hook {name}: {e}")),
        }
    }
}

/// Keyboard: 256 key bytes; mouse: axes and buttons. All zero = nothing pressed, no movement.
unsafe extern "system" fn hook_get_device_state(device: *mut c_void, size: u32, data: *mut u8) -> i32 {
    let original: GetDeviceState = std::mem::transmute(DEVICE_STATE.get().unwrap().trampoline());
    let hr = original(device, size, data);
    if hr >= 0 && VISIBLE.load(Relaxed) && !data.is_null() {
        std::ptr::write_bytes(data, 0, size as usize);
    }
    hr
}

/// Buffered events: read (so they don't pile up for when the overlay hides), then report none.
unsafe extern "system" fn hook_get_device_data(device: *mut c_void, size: u32, data: *mut c_void, count: *mut u32, flags: u32) -> i32 {
    let original: GetDeviceData = std::mem::transmute(DEVICE_DATA.get().unwrap().trampoline());
    let hr = original(device, size, data, count, flags);
    if hr >= 0 && VISIBLE.load(Relaxed) && !count.is_null() {
        *count = 0;
    }
    hr
}

/// An exclusive mouse gets no window messages, so neither would the overlay: always ask for non-exclusive.
// ponytail: the game hides and centres its own cursor (it imports ShowCursor/SetCursorPos/ClipCursor), so this should
// change nothing in play. If the system cursor ever shows in play, only downgrade while the overlay is shown.
unsafe extern "system" fn hook_set_cooperative_level(device: *mut c_void, window: *mut c_void, flags: u32) -> i32 {
    let original: SetCooperativeLevel = std::mem::transmute(COOPERATIVE_LEVEL.get().unwrap().trampoline());
    let ours = if flags & DISCL_EXCLUSIVE != 0 { flags & !DISCL_EXCLUSIVE | DISCL_NONEXCLUSIVE } else { flags };
    log(&format!("imgui: DirectInput cooperative level {flags:#x} -> {ours:#x}"));
    original(device, window, ours)
}

unsafe extern "system" fn hook_set_cursor_pos(x: i32, y: i32) -> i32 {
    if VISIBLE.load(Relaxed) {
        return 1; // the game re-centres its cursor every frame; leave ImGui's where the mouse put it
    }
    let original: SetCursorPosFn = std::mem::transmute(SET_CURSOR_POS.get().unwrap().trampoline());
    original(x, y)
}

unsafe extern "system" fn hook_clip_cursor(rect: *const c_void) -> i32 {
    let original: ClipCursorFn = std::mem::transmute(CLIP_CURSOR.get().unwrap().trampoline());
    original(if VISIBLE.load(Relaxed) { std::ptr::null() } else { rect })
}

/// Hook Present once the game has loaded D3D11 or D3D12, whichever -graphicsDriver sf-launch picked. Own thread: hudhook
/// builds a dummy device to find the vtable, which must not hold up sf-launch's start signal.
pub(crate) fn start_overlay() {
    std::thread::spawn(|| unsafe {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetModuleHandleW(name: *const u16) -> *const u8;
        }
        hook_input(); // before the game sets up its devices, so SetCooperativeLevel is seen
        // By the flag, not by which DLL shows up: a D3D12 game (and vkd3d-proton under Proton) may load d3d11.dll too.
        let dx12 = std::env::args().any(|a| a.eq_ignore_ascii_case("-graphicsDriver:dx12"));
        let dll = super::wide::<10>(if dx12 { "d3d12.dll" } else { "d3d11.dll" });
        while GetModuleHandleW(dll.as_ptr()).is_null() {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        log(&format!("imgui: {} loaded, hooking Present", if dx12 { "d3d12.dll" } else { "d3d11.dll" }));
        let hooks = Hudhook::builder();
        let hooks = if dx12 { hooks.with::<ImguiDx12Hooks>(Overlay) } else { hooks.with::<ImguiDx11Hooks>(Overlay) };
        match hooks.build().apply() {
            Ok(()) => log("imgui: overlay hooked (F9)"),
            Err(e) => log(&format!("imgui: overlay hook FAILED: {e:?}")),
        }
    });
}

// ---- reading arguments and pushing results, straight off the Luau stack ----
// Nothing here calls a game function that can raise a Lua error: an error would unwind through our Rust frame.

unsafe fn slot(l: *mut u8, i: usize) -> Option<*const u8> {
    let base = read_ptr(l.add(LUA_BASE));
    let top = read_ptr(l.add(LUA_TOP));
    let value = base.wrapping_add((i - 1) * 16);
    (value < top).then_some(value)
}

unsafe fn tag(l: *mut u8, i: usize) -> i32 {
    slot(l, i).map_or(TAG_NIL, |v| v.add(TVALUE_TAG).cast::<i32>().read_unaligned())
}

pub(crate) unsafe fn number(l: *mut u8, i: usize) -> Option<f32> {
    match tag(l, i) {
        TAG_NUMBER => slot(l, i).map(|v| v.cast::<f32>().read_unaligned()),
        TAG_STRING => string(l, i).trim().parse().ok(),
        _ => None,
    }
}

unsafe fn int(l: *mut u8, i: usize, default: i32) -> i32 {
    number(l, i).map_or(default, |n| n as i32) // `as` saturates, NaN -> 0
}

/// Slider bounds Dear ImGui accepts (SliderBehavior asserts on every frame past ±FLT_MAX/2, ±INT_MAX/2).
fn float_bound(n: f32) -> f32 {
    if n.is_nan() { 0.0 } else { n.clamp(-f32::MAX / 2.0, f32::MAX / 2.0) }
}

fn int_bound(n: i32) -> i32 {
    n.clamp(i32::MIN / 2, i32::MAX / 2)
}

unsafe fn truthy(l: *mut u8, i: usize) -> bool {
    match tag(l, i) {
        TAG_NIL => false,
        TAG_BOOLEAN => slot(l, i).is_some_and(|v| v.cast::<i32>().read_unaligned() != 0),
        _ => true,
    }
}

unsafe fn tstring(ts: *const u8) -> String {
    let len = ts.add(TSTRING_LEN).cast::<u32>().read_unaligned() as usize;
    String::from_utf8_lossy(std::slice::from_raw_parts(ts.add(TSTRING_DATA), len)).into_owned()
}

/// A string argument the way the stubs' checkstring reads it: strings as-is, numbers formatted like Lua.
unsafe fn string(l: *mut u8, i: usize) -> String {
    match (tag(l, i), slot(l, i)) {
        (TAG_STRING, Some(v)) => tstring(read_ptr(v)),
        (TAG_NUMBER, Some(v)) => lua_number(v.cast::<f32>().read_unaligned()),
        _ => String::new(),
    }
}

fn lua_number(n: f32) -> String {
    if n.fract() == 0.0 && n.abs() < 1e9 { format!("{}", n as i64) } else { format!("{n}") }
}

/// A list argument: the table's integer keys 1..n in order, strings and numbers only (others become "").
unsafe fn list(l: *mut u8, i: usize) -> Vec<String> {
    if tag(l, i) != TAG_TABLE {
        return Vec::new();
    }
    let exe = EXE.load(Relaxed);
    let pushnil: unsafe extern "C" fn(*mut u8) = std::mem::transmute(exe.add(LUA_PUSHNIL_RVA));
    let next: unsafe extern "C" fn(*mut u8, i32) -> i32 = std::mem::transmute(exe.add(LUA_NEXT_RVA));
    let settop: unsafe extern "C" fn(*mut u8, i32) = std::mem::transmute(exe.add(LUA_SETTOP_RVA));
    let mut items: Vec<String> = Vec::new();
    let mut seen = 0;
    pushnil(l);
    while next(l, i as i32) != 0 {
        let top = read_ptr(l.add(LUA_TOP));
        let (key, value) = (top.wrapping_sub(32), top.wrapping_sub(16));
        let index = (key.add(TVALUE_TAG).cast::<i32>().read_unaligned() == TAG_NUMBER)
            .then(|| key.cast::<f32>().read_unaligned())
            .filter(|n| n.fract() == 0.0 && *n >= 1.0 && *n <= MAX_ITEMS as f32);
        if let Some(n) = index {
            let text = match value.add(TVALUE_TAG).cast::<i32>().read_unaligned() {
                TAG_STRING => tstring(read_ptr(value)),
                TAG_NUMBER => lua_number(value.cast::<f32>().read_unaligned()),
                _ => String::new(),
            };
            let n = n as usize;
            if items.len() < n {
                items.resize(n, String::new());
            }
            items[n - 1] = text;
        }
        settop(l, -2); // pop the value, keep the key for the next lua_next
        seen += 1;
        if seen > MAX_ITEMS * 4 {
            settop(l, -2); // pop the key too: stop early
            break;
        }
    }
    items
}

pub(crate) unsafe fn push_bool(l: *mut u8, b: bool) {
    let top = read_ptr(l.add(LUA_TOP)).cast_mut();
    top.cast::<i32>().write_unaligned(b as i32);
    top.add(TVALUE_TAG).cast::<i32>().write_unaligned(TAG_BOOLEAN);
    l.add(LUA_TOP).cast::<*mut u8>().write_unaligned(top.add(16));
}

pub(crate) unsafe fn push_number(l: *mut u8, n: f32) {
    let top = read_ptr(l.add(LUA_TOP)).cast_mut();
    top.cast::<f32>().write_unaligned(n);
    top.add(TVALUE_TAG).cast::<i32>().write_unaligned(TAG_NUMBER);
    l.add(LUA_TOP).cast::<*mut u8>().write_unaligned(top.add(16));
}

unsafe fn push_string(l: *mut u8, s: &str) {
    // The engine's string, heap form: pointer, u32 length (low 28 bits), byte 15 = 0xff.
    let mut engine = [0u8; 16];
    engine[..8].copy_from_slice(&(s.as_ptr() as usize).to_le_bytes());
    engine[8..12].copy_from_slice(&((s.len() as u32) & 0x0fff_ffff).to_le_bytes());
    engine[15] = 0xff;
    let push: unsafe extern "C" fn(*mut u8, *const u8) = std::mem::transmute(EXE.load(Relaxed).add(PUSH_STRING_RVA));
    push(l, engine.as_ptr());
}

fn lua_args(l: *mut u8) -> usize {
    unsafe { (read_ptr(l.add(LUA_TOP)) as usize).saturating_sub(read_ptr(l.add(LUA_BASE)) as usize) / 16 }
}

// ---- the natives (game thread) ----

/// `visible, open, flag3 = ImGuiBegin(title[, open[, flag3]])`. A closable window passed `open == false` stays hidden.
unsafe extern "C" fn begin(l: *mut u8) -> i32 {
    let title = string(l, 1);
    let closable = lua_args(l) >= 2;
    let open_in = !closable || truthy(l, 2);
    let flag3 = truthy(l, 3);
    let key = format!("window\x1f{title}");
    let closed = closable && matches!(take_event(&key), Some(Event::Closed));
    let open = open_in && !closed;
    // Hidden overlay: answer like the retail stub (not visible), so shipped debug bodies don't start running in play.
    let visible = open && VISIBLE.load(Relaxed) && is_open(&key, true);
    record(|r| {
        r.windows.push(title.clone());
        let arg = if closable { CLOSABLE } else { 0 } | if open { 0 } else { HIDDEN };
        r.push(Op::Open { scope: Scope::Window, label: title, key, open: visible, arg });
    });
    push_bool(l, visible);
    push_bool(l, open);
    push_bool(l, flag3);
    3
}

unsafe extern "C" fn end(_l: *mut u8) -> i32 {
    record(|r| {
        r.windows.pop();
        r.push(Op::Close(Scope::Window));
    });
    0
}

unsafe fn text_op(l: *mut u8, op: fn(String) -> Op) -> i32 {
    let s = string(l, 1);
    record(|r| r.push(op(s)));
    0
}

unsafe extern "C" fn text(l: *mut u8) -> i32 {
    text_op(l, Op::Text)
}

unsafe extern "C" fn text_wrapped(l: *mut u8) -> i32 {
    text_op(l, Op::Wrapped)
}

unsafe extern "C" fn separator_text(l: *mut u8) -> i32 {
    text_op(l, Op::SeparatorText)
}

/// `(r, g, b, a, text)`, colours 0..1.
unsafe extern "C" fn text_colored(l: *mut u8) -> i32 {
    let colour = [1, 2, 3, 4].map(|i| number(l, i).unwrap_or(1.0));
    let s = string(l, 5);
    record(|r| r.push(Op::Colored(colour, s)));
    0
}

unsafe extern "C" fn spacing(_l: *mut u8) -> i32 {
    record(|r| r.push(Op::Spacing));
    0
}

unsafe extern "C" fn same_line(_l: *mut u8) -> i32 {
    record(|r| r.push(Op::SameLine));
    0
}

unsafe extern "C" fn button(l: *mut u8) -> i32 {
    let label = string(l, 1);
    let key = record(|r| {
        let key = r.key(&label);
        r.push(Op::Button { label, key: key.clone() });
        key
    });
    push_bool(l, key.is_some_and(|k| take_event(&k).is_some()));
    1
}

unsafe extern "C" fn collapsing_header(l: *mut u8) -> i32 {
    let label = string(l, 1);
    let key = record(|r| {
        let key = r.key(&label);
        r.push(Op::Header { label, key: key.clone() });
        key
    });
    push_bool(l, key.is_some_and(|k| is_open(&k, false)));
    1
}

/// `changed, value = ImGuiCheckbox(label, value)`
unsafe extern "C" fn checkbox(l: *mut u8) -> i32 {
    let (label, value) = (string(l, 1), truthy(l, 2));
    let key = record(|r| {
        let key = r.key(&label);
        r.push(Op::Checkbox { label, key: key.clone(), value });
        key
    });
    match key.and_then(|k| take_event(&k)) {
        Some(Event::Bool(new)) => {
            push_bool(l, true);
            push_bool(l, new);
        }
        _ => {
            push_bool(l, false);
            push_bool(l, value);
        }
    }
    2
}

/// `value = ImGuiSliderFloat(label, value, min, max)`
unsafe extern "C" fn slider_float(l: *mut u8) -> i32 {
    let label = string(l, 1);
    let [value, min, max] = [2, 3, 4].map(|i| number(l, i).unwrap_or(0.0));
    let key = record(|r| {
        let key = r.key(&label);
        r.push(Op::SliderFloat { label, key: key.clone(), value, min, max });
        key
    });
    let new = match key.and_then(|k| take_event(&k)) {
        Some(Event::Float(new)) => new,
        _ => value,
    };
    push_number(l, new);
    1
}

/// `value = ImGuiSliderInt(label, value, min, max)`
unsafe extern "C" fn slider_int(l: *mut u8) -> i32 {
    let label = string(l, 1);
    let [value, min, max] = [2, 3, 4].map(|i| int(l, i, 0));
    let key = record(|r| {
        let key = r.key(&label);
        r.push(Op::SliderInt { label, key: key.clone(), value, min, max });
        key
    });
    push_number(l, int_event(key, value) as f32);
    1
}

fn int_event(key: Option<String>, current: i32) -> i32 {
    match key.and_then(|k| take_event(&k)) {
        Some(Event::Int(new)) => new,
        _ => current,
    }
}

/// `text = ImGuiInputText(label, text)`; one argument is the text (BossFightTester `ImGuiInputText("1")`).
unsafe extern "C" fn input_text(l: *mut u8) -> i32 {
    let (label, text) = if lua_args(l) >= 2 { (string(l, 1), string(l, 2)) } else { ("##input".into(), string(l, 1)) };
    let key = record(|r| {
        let key = r.key(&label);
        r.push(Op::InputText { label, key: key.clone(), text: text.clone() });
        key
    });
    match key.and_then(|k| take_event(&k)) {
        Some(Event::Text(new)) => push_string(l, &new),
        _ => push_string(l, &text),
    }
    1
}

/// `index = ImGuiCombo(label, index, items)`
unsafe extern "C" fn combo(l: *mut u8) -> i32 {
    let (label, current, items) = (string(l, 1), int(l, 2, 1), list(l, 3));
    let key = record(|r| {
        let key = r.key(&label);
        r.push(Op::Combo { label, key: key.clone(), current, items });
        key
    });
    push_number(l, int_event(key, current) as f32);
    1
}

/// `index = ImGuiListBox(label, items, index, height)`
unsafe extern "C" fn list_box(l: *mut u8) -> i32 {
    let (label, items, current, height) = (string(l, 1), list(l, 2), int(l, 3, 1), int(l, 4, -1));
    let key = record(|r| {
        let key = r.key(&label);
        r.push(Op::ListBox { label, key: key.clone(), current, items, height });
        key
    });
    push_number(l, int_event(key, current) as f32);
    1
}

/// `index = ImGuiRadioButtons(items, index)`
unsafe extern "C" fn radio_buttons(l: *mut u8) -> i32 {
    let (items, current) = (list(l, 1), int(l, 2, 1));
    let key = record(|r| {
        let key = r.key(&format!("##radio{}", items.join("|")));
        r.push(Op::Radio { key: key.clone(), current, items });
        key
    });
    push_number(l, int_event(key, current) as f32);
    1
}

/// A scope opener: the script is told what the overlay saw last frame (`default` before it has drawn it once).
unsafe fn open_scope(l: *mut u8, scope: Scope, default: bool, arg: i32) -> i32 {
    let label = string(l, 1);
    let open = record(|r| {
        let key = r.key(&label);
        let open = is_open(&key, default);
        r.push(Op::Open { scope, label, key, open, arg });
        open
    });
    push_bool(l, open.unwrap_or(false));
    1
}

unsafe fn close_scope(scope: Scope) -> i32 {
    record(|r| r.push(Op::Close(scope)));
    0
}

unsafe extern "C" fn begin_tab_bar(l: *mut u8) -> i32 {
    open_scope(l, Scope::TabBar, true, 0)
}

unsafe extern "C" fn end_tab_bar(_l: *mut u8) -> i32 {
    close_scope(Scope::TabBar)
}

unsafe extern "C" fn begin_tab_item(l: *mut u8) -> i32 {
    open_scope(l, Scope::TabItem, false, 0)
}

unsafe extern "C" fn end_tab_item(_l: *mut u8) -> i32 {
    close_scope(Scope::TabItem)
}

/// `ImGuiBeginTable(id, columns)`
unsafe extern "C" fn begin_table(l: *mut u8) -> i32 {
    let columns = int(l, 2, 1);
    open_scope(l, Scope::Table, true, columns)
}

unsafe extern "C" fn end_table(_l: *mut u8) -> i32 {
    close_scope(Scope::Table)
}

/// `ImGuiTreeNodeEx(label, flags)` (DevImGuiTools' type tree)
unsafe extern "C" fn tree_node_ex(l: *mut u8) -> i32 {
    let flags = int(l, 2, 0);
    open_scope(l, Scope::Tree, false, flags)
}

unsafe extern "C" fn tree_pop(_l: *mut u8) -> i32 {
    close_scope(Scope::Tree)
}

unsafe extern "C" fn begin_disabled(l: *mut u8) -> i32 {
    let disabled = lua_args(l) == 0 || truthy(l, 1);
    record(|r| r.push(Op::Open { scope: Scope::Disabled, label: String::new(), key: String::new(), open: true, arg: disabled as i32 }));
    0
}

unsafe extern "C" fn end_disabled(_l: *mut u8) -> i32 {
    close_scope(Scope::Disabled)
}

unsafe extern "C" fn set_next_item_width(l: *mut u8) -> i32 {
    let width = number(l, 1).unwrap_or(0.0);
    record(|r| r.push(Op::ItemWidth(width)));
    0
}

/// `ImGuiTableSetupColumn(label[, width])`
unsafe extern "C" fn table_setup_column(l: *mut u8) -> i32 {
    let (label, width) = (string(l, 1), number(l, 2).unwrap_or(0.0));
    record(|r| r.push(Op::Column { label, width }));
    0
}

unsafe extern "C" fn table_headers_row(_l: *mut u8) -> i32 {
    record(|r| r.push(Op::HeadersRow));
    0
}

unsafe extern "C" fn table_next_row(_l: *mut u8) -> i32 {
    record(|r| r.push(Op::NextRow));
    0
}

unsafe extern "C" fn table_set_column_index(l: *mut u8) -> i32 {
    let column = int(l, 1, 0);
    record(|r| r.push(Op::ColumnIndex(column)));
    0
}

/// `ImGuiIsItemClicked(button)`: about the widget recorded just before it.
unsafe extern "C" fn is_item_clicked(l: *mut u8) -> i32 {
    let button = int(l, 1, 0);
    let key = record(|r| {
        let key = format!("{}\x1fclicked", r.last_key);
        r.push(Op::Clicked { key: key.clone(), button });
        key
    });
    push_bool(l, key.is_some_and(|k| take_event(&k).is_some()));
    1
}

// ---- the overlay (render thread) ----

struct Overlay;

impl ImguiRenderLoop for Overlay {
    fn initialize<'a>(&'a mut self, ctx: &mut Context, _render: &'a mut dyn RenderContext) {
        // Window positions persist next to sf-bootstrap.log. Not next to the DLL: that's under Documents, and a file the
        // box creates there sets off Sandboxie's Immediate Recovery prompt (box run 2026-10-30 13:19).
        let ini = std::env::var_os("LOCALAPPDATA").map(|d| std::path::Path::new(&d).join("Soulframe\\sf-imgui.ini"));
        ctx.set_ini_filename(ini);
    }

    fn before_render<'a>(&'a mut self, ctx: &mut Context, _render: &'a mut dyn RenderContext) {
        ctx.io_mut().mouse_draw_cursor = VISIBLE.load(Relaxed);
    }

    fn render(&mut self, _ui: &mut Ui) {
        if !VISIBLE.load(Relaxed) {
            return;
        }
        let Some(frame) = PUBLISHED.lock().ok().map(|f| Arc::clone(&f)) else { return };
        let (Ok(mut events), Ok(mut open)) = (EVENTS.lock(), OPEN.lock()) else { return };
        if frame.is_empty() {
            unsafe { hint() };
        }
        unsafe { replay(&frame, &mut events, &mut open) };
    }

    fn message_filter(&self, _io: &Io) -> MessageFilter {
        if VISIBLE.load(Relaxed) { MessageFilter::InputAll } else { MessageFilter::empty() }
    }
}

unsafe fn hint() {
    let title = c"sf-bootstrap";
    sys::igBegin(title.as_ptr(), std::ptr::null_mut(), 0);
    imtext("No ImGui calls this frame. In the world, run `python tools/devlua.py` once for the SF dev panel.");
    sys::igEnd();
}

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

unsafe fn imtext(s: &str) {
    let start = s.as_ptr().cast::<c_char>();
    sys::igTextUnformatted(start, start.add(s.len()));
}

struct Entry {
    scope: Scope,
    /// ImGui needs the matching end call (Begin always; the others only when they returned true).
    close: bool,
    columns: i32,
    setup: i32,
    rows: bool,
}

unsafe fn close_entry(e: &Entry) {
    if !e.close {
        return;
    }
    match e.scope {
        Scope::Window => sys::igEnd(),
        Scope::TabBar => sys::igEndTabBar(),
        Scope::TabItem => sys::igEndTabItem(),
        Scope::Table => sys::igEndTable(),
        Scope::Tree => sys::igTreePop(),
        Scope::Disabled => sys::igEndDisabled(),
    }
}

/// Draw one recorded frame. Scopes are matched here, not trusted: an unbalanced or out-of-place call from a script
/// is dropped or closed for it, because a Dear ImGui assert would take the game down.
unsafe fn replay(ops: &[Op], events: &mut HashMap<String, Event>, open: &mut HashMap<String, bool>) {
    let mut stack: Vec<Entry> = Vec::new();
    // While Some(d), everything inside the scope at stack index d-1 is skipped: the script drew contents the overlay
    // just closed (or hid).
    let mut skip: Option<usize> = None;
    let in_table = |stack: &[Entry]| {
        stack.iter().rposition(|e| matches!(e.scope, Scope::Table | Scope::Window)).filter(|&i| {
            stack[i].scope == Scope::Table && stack[i].close
        })
    };
    for op in ops {
        if let Op::Close(scope) = op {
            let Some(at) = stack.iter().rposition(|e| e.scope == *scope) else { continue };
            while stack.len() > at {
                let e = stack.pop().expect("len > at");
                close_entry(&e);
            }
            if skip.is_some_and(|d| stack.len() < d) {
                skip = None;
            }
            continue;
        }
        if skip.is_some() {
            if let Op::Open { scope, .. } = op {
                stack.push(Entry { scope: *scope, close: false, columns: 0, setup: 0, rows: false });
            }
            continue;
        }
        match op {
            Op::Open { scope, label, key, open: told, arg } => {
                let c = cstr(label);
                let shown = match scope {
                    Scope::Window => {
                        if arg & HIDDEN != 0 {
                            stack.push(Entry { scope: Scope::Window, close: false, columns: 0, setup: 0, rows: false });
                            skip = Some(stack.len());
                            continue;
                        }
                        let title = if label.is_empty() { c"##untitled".to_owned() } else { c };
                        sys::igSetNextWindowSize(sys::ImVec2 { x: 420.0, y: 360.0 }, sys::ImGuiCond_FirstUseEver as i32);
                        let mut keep = true;
                        let p_open = if arg & CLOSABLE != 0 { &raw mut keep } else { std::ptr::null_mut() };
                        let visible = sys::igBegin(title.as_ptr(), p_open, 0);
                        if !keep {
                            events.insert(key.clone(), Event::Closed);
                        }
                        open.insert(key.clone(), visible);
                        // igEnd is due whatever Begin returned, and scripts call ImGuiEnd either way.
                        stack.push(Entry { scope: Scope::Window, close: true, columns: 0, setup: 0, rows: false });
                        if !*told {
                            // The script drew no contents, and some only call ImGuiEnd when visible (MoteFishing):
                            // close it now; a later ImGuiEnd at top level finds no window and is dropped.
                            close_entry(&stack.pop().expect("just pushed"));
                        } else if !visible {
                            skip = Some(stack.len());
                        }
                        continue;
                    }
                    Scope::Disabled => {
                        sys::igBeginDisabled(*arg != 0);
                        stack.push(Entry { scope: Scope::Disabled, close: true, columns: 0, setup: 0, rows: false });
                        continue;
                    }
                    Scope::TabBar => sys::igBeginTabBar(c.as_ptr(), 0),
                    Scope::TabItem => {
                        let in_bar = stack.iter().rev().find(|e| e.scope != Scope::Disabled);
                        in_bar.is_some_and(|e| e.scope == Scope::TabBar && e.close)
                            && sys::igBeginTabItem(c.as_ptr(), std::ptr::null_mut(), 0)
                    }
                    Scope::Table => {
                        let columns = (*arg).clamp(1, 64);
                        // The column count is part of the ID: one ID with two counts asserts (BossFightTester has
                        // two "LevelGuide" tables, 4 and 6 columns).
                        let id = cstr(&format!("{label}##{columns}"));
                        let shown = sys::igBeginTable(id.as_ptr(), columns, 0, sys::ImVec2 { x: 0.0, y: 0.0 }, 0.0);
                        if shown {
                            open.insert(key.clone(), true);
                            if *told {
                                stack.push(Entry { scope: Scope::Table, close: true, columns, setup: 0, rows: false });
                            } else {
                                sys::igEndTable();
                            }
                        } else {
                            open.insert(key.clone(), false);
                            if *told {
                                stack.push(Entry { scope: Scope::Table, close: false, columns, setup: 0, rows: false });
                                skip = Some(stack.len());
                            }
                        }
                        continue;
                    }
                    Scope::Tree if arg & NO_TREE_PUSH_ON_OPEN != 0 => {
                        // No ID push and no TreePop due: an item, not a scope. (Our pop would assert.)
                        let shown = sys::igTreeNodeEx_Str(c.as_ptr(), *arg);
                        open.insert(key.clone(), shown);
                        continue;
                    }
                    Scope::Tree => sys::igTreeNodeEx_Str(c.as_ptr(), *arg),
                };
                open.insert(key.clone(), shown);
                let entry = Entry { scope: *scope, close: shown, columns: 0, setup: 0, rows: false };
                match (*told, shown) {
                    (true, true) => stack.push(entry),
                    (true, false) => {
                        stack.push(entry);
                        skip = Some(stack.len());
                    }
                    // The script didn't draw contents and won't call the closer: close it right away.
                    (false, _) => close_entry(&entry),
                }
            }
            Op::Close(_) => unreachable!("handled above"),
            Op::Text(s) => imtext(s),
            Op::Wrapped(s) => {
                sys::igPushTextWrapPos(0.0);
                imtext(s);
                sys::igPopTextWrapPos();
            }
            Op::Colored([r, g, b, a], s) => {
                sys::igPushStyleColor_Vec4(sys::ImGuiCol_Text as i32, sys::ImVec4 { x: *r, y: *g, z: *b, w: *a });
                imtext(s);
                sys::igPopStyleColor(1);
            }
            Op::SeparatorText(s) => {
                // ImGui 1.89.2 predates SeparatorText.
                sys::igSeparator();
                if !s.is_empty() {
                    imtext(s);
                }
            }
            Op::Spacing => sys::igSpacing(),
            Op::SameLine => sys::igSameLine(0.0, -1.0),
            Op::Button { label, key } => {
                if sys::igButton(cstr(label).as_ptr(), sys::ImVec2 { x: 0.0, y: 0.0 }) {
                    events.insert(key.clone(), Event::Clicked);
                }
            }
            Op::Header { label, key } => {
                let shown = sys::igCollapsingHeader_TreeNodeFlags(cstr(label).as_ptr(), 0);
                open.insert(key.clone(), shown);
            }
            Op::Checkbox { label, key, value } => {
                let mut v = *value;
                if sys::igCheckbox(cstr(label).as_ptr(), &mut v) {
                    events.insert(key.clone(), Event::Bool(v));
                }
            }
            Op::SliderFloat { label, key, value, min, max } => {
                let mut v = float_bound(*value);
                let (min, max) = (float_bound(*min), float_bound(*max));
                if sys::igSliderFloat(cstr(label).as_ptr(), &mut v, min, max, c"%.3f".as_ptr(), 0) {
                    events.insert(key.clone(), Event::Float(v));
                }
            }
            Op::SliderInt { label, key, value, min, max } => {
                let mut v = *value;
                if sys::igSliderInt(cstr(label).as_ptr(), &mut v, int_bound(*min), int_bound(*max), c"%d".as_ptr(), 0) {
                    events.insert(key.clone(), Event::Int(v));
                }
            }
            Op::InputText { label, key, text } => {
                let mut buf = text.replace('\0', "").into_bytes();
                buf.resize(buf.len() + 256, 0);
                if sys::igInputText(cstr(label).as_ptr(), buf.as_mut_ptr().cast(), buf.len(), 0, None, std::ptr::null_mut()) {
                    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                    events.insert(key.clone(), Event::Text(String::from_utf8_lossy(&buf[..len]).into_owned()));
                }
            }
            Op::Combo { label, key, current, items } => {
                let (_keep, ptrs) = c_list(items);
                let mut index = current - 1;
                if sys::igCombo_Str_arr(cstr(label).as_ptr(), &mut index, ptrs.as_ptr(), ptrs.len() as i32, -1) {
                    events.insert(key.clone(), Event::Int(index + 1));
                }
            }
            Op::ListBox { label, key, current, items, height } => {
                let (_keep, ptrs) = c_list(items);
                let mut index = current - 1;
                if sys::igListBox_Str_arr(cstr(label).as_ptr(), &mut index, ptrs.as_ptr(), ptrs.len() as i32, *height) {
                    events.insert(key.clone(), Event::Int(index + 1));
                }
            }
            Op::Radio { key, current, items } => {
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        sys::igSameLine(0.0, -1.0);
                    }
                    let label = cstr(&format!("{item}##{key}{i}"));
                    if sys::igRadioButton_Bool(label.as_ptr(), *current == i as i32 + 1) {
                        events.insert(key.clone(), Event::Int(i as i32 + 1));
                    }
                }
            }
            Op::ItemWidth(w) => sys::igSetNextItemWidth(*w),
            Op::Clicked { key, button } => {
                if sys::igIsItemClicked((*button).clamp(0, 4)) {
                    events.insert(key.clone(), Event::Clicked);
                }
            }
            Op::Column { label, width } => {
                if let Some(t) = in_table(&stack) {
                    let e = &mut stack[t];
                    if !e.rows && e.setup < e.columns {
                        e.setup += 1;
                        let flags = if *width > 0.0 { sys::ImGuiTableColumnFlags_WidthFixed as i32 } else { 0 };
                        sys::igTableSetupColumn(cstr(label).as_ptr(), flags, *width, 0);
                    }
                }
            }
            Op::HeadersRow => {
                if let Some(t) = in_table(&stack) {
                    stack[t].rows = true;
                    sys::igTableHeadersRow();
                }
            }
            Op::NextRow => {
                if let Some(t) = in_table(&stack) {
                    stack[t].rows = true;
                    sys::igTableNextRow(0, 0.0);
                }
            }
            Op::ColumnIndex(i) => {
                if let Some(t) = in_table(&stack) {
                    if !stack[t].rows {
                        stack[t].rows = true;
                        sys::igTableNextRow(0, 0.0);
                    }
                    sys::igTableSetColumnIndex((*i).clamp(0, stack[t].columns - 1));
                }
            }
        }
    }
    while let Some(e) = stack.pop() {
        close_entry(&e);
    }
}

fn c_list(items: &[String]) -> (Vec<CString>, Vec<*const c_char>) {
    let owned: Vec<CString> = items.iter().map(|s| cstr(s)).collect();
    let ptrs = owned.iter().map(|c| c.as_ptr()).collect();
    (owned, ptrs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every native we install is a row of the pinned exe's native table, under the name its atom hashes to.
    #[test]
    fn natives_are_in_the_pinned_table() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../soulframe-builds/P16H6/pristine/Soulframe.x64.exe");
        let Ok(image) = super::super::tests::map_pinned(path) else { return eprintln!("skipped: no {path}") };
        let atoms: Vec<u32> = (0..NATIVE_ROWS)
            .map(|i| u32::from_le_bytes(image[NATIVE_TABLE_RVA + i * 16..][..4].try_into().unwrap()))
            .collect();
        assert_eq!(u64::from_le_bytes(image[NATIVE_TABLE_RVA + NATIVE_ROWS * 16 + 8..][..8].try_into().unwrap()), 0);
        for (atom, name, _) in NATIVES {
            assert!(atoms.contains(&atom), "{name}");
        }
    }

    /// The overlay's input block hooks these slots; they must be real functions here, not null.
    #[test]
    fn dinput_device_vtable_has_the_hooked_slots() {
        let vtable = unsafe { dinput_device_vtable() }.expect("DirectInput keyboard device");
        for slot in [GET_DEVICE_STATE, GET_DEVICE_DATA, SET_COOPERATIVE_LEVEL] {
            assert!(!unsafe { read_ptr(vtable.add(slot)) }.is_null());
        }
    }

    #[test]
    fn keys_count_repeats_per_window() {
        let mut r = Recording::default();
        r.windows.push("W".into());
        assert_eq!(r.key("Go"), "W\x1fGo");
        assert_eq!(r.key("Go"), "W\x1fGo\x1f2");
        r.windows.pop();
        assert_eq!(r.key("Go"), "\x1fGo");
    }

    /// Scripts that break ImGui's rules must not trip a Dear ImGui assert (that aborts the game). Two frames, so the
    /// second replays with the open/closed state the first one stored.
    #[test]
    fn unbalanced_scripts_replay_without_asserts() {
        let open_ = |scope, label: &str, open, arg| Op::Open { scope, label: label.into(), key: label.into(), open, arg };
        let ops = vec![
            Op::Close(Scope::Tree),                         // TreePop with no tree
            Op::Close(Scope::Disabled),                     // EndDisabled with no BeginDisabled
            open_(Scope::TabItem, "tab outside a bar", true, 0),
            Op::Text("in the debug window".into()),
            Op::Close(Scope::TabItem),
            Op::ColumnIndex(3),                             // table calls outside a table
            Op::Column { label: "c".into(), width: 10.0 },
            open_(Scope::Window, "W", true, 0),
            open_(Scope::Disabled, "", true, 1),
            open_(Scope::Table, "T", true, 2),
            Op::Column { label: "a".into(), width: 50.0 },
            Op::Column { label: "b".into(), width: 0.0 },
            Op::Column { label: "too many".into(), width: 0.0 },
            Op::HeadersRow,
            Op::ColumnIndex(7),                             // past the last column
            Op::Column { label: "after rows".into(), width: 0.0 },
            open_(Scope::Tree, "node", true, 0),            // told open, the overlay has it closed: skipped
            Op::Button { label: "inside".into(), key: "inside".into() },
            Op::Close(Scope::Tree),
            open_(Scope::Tree, "never told", false, 0),     // no TreePop follows
            Op::Close(Scope::Window),                       // closes the table and the disabled block too
            open_(Scope::Window, "", true, CLOSABLE),
            open_(Scope::TabBar, "bar", true, 0),
            open_(Scope::TabItem, "one", false, 0),
            open_(Scope::TabItem, "two", true, 0),
            Op::Combo { label: "##c".into(), key: "c".into(), current: 9, items: vec!["x".into()] },
            Op::Radio { key: "r".into(), current: 1, items: vec!["a".into(), "b".into()] },
            Op::InputText { label: "in".into(), key: "in".into(), text: "%s%n".into() },
            // From the review: leaf nodes that push no ID, slider bounds past what ImGui accepts, one table ID with
            // two column counts, and a window whose End only comes when visible.
            open_(Scope::Tree, "leaf told", true, 0x108),
            open_(Scope::Tree, "leaf untold", false, 0x108),
            Op::SliderFloat { label: "f".into(), key: "f".into(), value: f32::NAN, min: f32::NEG_INFINITY, max: f32::MAX },
            Op::SliderInt { label: "i".into(), key: "i".into(), value: 0, min: i32::MIN, max: i32::MAX },
            open_(Scope::Table, "same", true, 4),
            Op::Close(Scope::Table),
            open_(Scope::Table, "same", true, 6),
            Op::Close(Scope::Table),
            open_(Scope::Window, "collapsed, no End", false, 0),
            open_(Scope::Window, "after it", true, 0),
            Op::Text("drawn".into()),
            Op::Close(Scope::Window),
            open_(Scope::Window, "hidden", false, CLOSABLE | HIDDEN),
            Op::Text("never drawn".into()),
            // ...and the frame ends with the tab item, tab bar, both windows and the hidden one still open.
        ];
        let mut ctx = Context::create();
        ctx.set_ini_filename(None);
        ctx.io_mut().display_size = [800.0, 600.0];
        ctx.fonts().build_rgba32_texture();
        let (mut events, mut open) = (HashMap::new(), HashMap::new());
        for _ in 0..2 {
            let _ui = ctx.new_frame();
            unsafe { replay(&ops, &mut events, &mut open) };
            ctx.render();
        }
        assert_eq!(open.get("node"), Some(&false));
        assert_eq!(open.get("W"), Some(&true));
    }

    #[test]
    fn numbers_print_like_lua() {
        assert_eq!((lua_number(3.0), lua_number(-2.0), lua_number(0.5)), ("3".into(), "-2".into(), "0.5".into()));
    }
}
