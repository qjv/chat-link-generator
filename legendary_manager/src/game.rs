use memtools::{Pattern, PatternByte};
use once_cell::sync::Lazy;
use parking_lot::Mutex;

#[cfg(target_os = "windows")]
use windows::Win32::Foundation::CloseHandle;
#[cfg(target_os = "windows")]
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetModuleHandleW, GetProcAddress};
#[cfg(target_os = "windows")]
use windows::Win32::System::Memory::{
    VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_GUARD, PAGE_NOACCESS,
};
#[cfg(target_os = "windows")]
use windows::Win32::System::Threading::{OpenThread, THREAD_QUERY_INFORMATION};

const VDF_CTX_PATTERN: &str =
    "74 19 41 B8 ?? ?? ?? ?? 48 8D 15 ?? ?? ?? ?? 48 8D 0D <? ? ? ?> E8 ?? ?? ?? ?? 48 8B CF E8 ?? ?? ?? ?? E8 <? ? ? ?>";

const IMAGE_BASE: usize = 0x140000000;
const CHAR_CONTEXT_GETTER_ABS: usize = 0x1409b03b0;
const PROP_CTX_PATTERN: &str =
    "8B 0D ?? ?? ?? ?? 65 48 8B 04 25 58 00 00 00 BA ?? ?? ?? ?? 48 8B 04 C8 48 8B 04 02 C3";
const PROP_CTX_PATTERN_FALLBACK: &str =
    "8B 15 ?? ?? ?? ?? 65 48 8B 04 25 58 00 00 00 41 B8 ?? ?? ?? ?? 48 8B 04 D0 49 89 0C 00 C3";
const MAIN_THREAD_PATTERN: &str =
    "E8 ?? ?? ?? ?? 8B 0D ?? ?? ?? ?? 85 C9 75 08 89 05 ?? ?? ?? ?? EB 1D 3B C8 74 19";

#[derive(Default)]
pub struct Runtime {
    pub get_vdf_ctx_fn: usize,
    pub prop_ctx_getter: usize,
    pub main_thread_match: usize,
    pub initialized: bool,
    pub last_error: String,
}

pub static RUNTIME: Lazy<Mutex<Runtime>> = Lazy::new(|| Mutex::new(Runtime::default()));

#[derive(Debug, Clone)]
pub struct LiveSlotEntry {
    pub slot_index: u32,
    pub item_ptr: usize,
    pub item_def_ptr: usize,
    pub item_id: u32,
}

#[cfg(target_os = "windows")]
pub unsafe fn is_readable(ptr: *const u8, size: usize) -> bool {
    if ptr.is_null() {
        return false;
    }
    let Some(ptr_end) = (ptr as usize).checked_add(size) else {
        return false;
    };

    let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
    let ret = VirtualQuery(
        Some(ptr as *const _),
        &mut mbi,
        std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
    );
    if ret == 0 || mbi.State != MEM_COMMIT {
        return false;
    }

    if mbi.Protect.contains(PAGE_NOACCESS) || mbi.Protect.contains(PAGE_GUARD) {
        return false;
    }

    ptr_end <= (mbi.BaseAddress as usize).saturating_add(mbi.RegionSize)
}

#[cfg(not(target_os = "windows"))]
pub unsafe fn is_readable(_ptr: *const u8, _size: usize) -> bool {
    false
}

#[cfg(target_os = "windows")]
fn game_module_range() -> Option<(usize, usize)> {
    unsafe {
        let handle = GetModuleHandleW(None).ok()?;
        let base = handle.0 as *const u8;
        if !is_readable(base, 0x40) {
            return None;
        }
        let e_lfanew = *(base.add(0x3C) as *const i32) as usize;
        let pe_header = base.add(e_lfanew);
        if !is_readable(pe_header, 0x78) {
            return None;
        }
        let size_of_image = *(pe_header.add(4 + 20 + 56) as *const u32) as usize;
        let base_addr = base as usize;
        Some((base_addr, base_addr + size_of_image))
    }
}

#[cfg(not(target_os = "windows"))]
fn game_module_range() -> Option<(usize, usize)> {
    None
}

fn pattern_matches_at(pattern: &Pattern, bytes: &[u8], offset: usize) -> bool {
    if offset + pattern.len() > bytes.len() {
        return false;
    }
    pattern.bytes.iter().enumerate().all(|(i, pb)| match pb {
        PatternByte::Wildcard => true,
        PatternByte::Value(v) => bytes[offset + i] == *v,
    })
}

fn wildcard_set_start(pattern: &Pattern, set_number: usize) -> Option<usize> {
    let mut in_wildcard = false;
    let mut seen = 0usize;
    for (idx, byte) in pattern.bytes.iter().enumerate() {
        let is_wildcard = *byte == PatternByte::Wildcard;
        if is_wildcard && !in_wildcard {
            seen += 1;
            if seen == set_number {
                return Some(idx);
            }
        }
        in_wildcard = is_wildcard;
    }
    None
}

unsafe fn follow_relative_checked(
    addr: *const u8,
    module: Option<(usize, usize)>,
) -> Option<*mut u8> {
    if !is_readable(addr, std::mem::size_of::<i32>()) {
        return None;
    }
    let disp = (addr as *const i32).read_unaligned();
    let target = addr.add(4).offset(disp as isize) as *mut u8;
    if let Some((start, end)) = module {
        let t = target as usize;
        if t < start || t >= end {
            return None;
        }
    }
    Some(target)
}

unsafe fn cstr_eq_bounded(ptr: *const u8, expected: &str) -> bool {
    let expected = expected.as_bytes();
    if !is_readable(ptr, expected.len() + 1) {
        return false;
    }
    for (i, b) in expected.iter().enumerate() {
        if *ptr.add(i) != *b {
            return false;
        }
    }
    *ptr.add(expected.len()) == 0
}

unsafe fn scan_module_all(pattern: &Pattern, module: Option<(usize, usize)>) -> Vec<*mut u8> {
    #[cfg(target_os = "windows")]
    {
        let Some((module_start, module_end)) = module else {
            return Vec::new();
        };
        let mut results = Vec::new();
        let mut addr = module_start;
        let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();

        while addr < module_end {
            let ret = VirtualQuery(
                Some(addr as *const _),
                &mut mbi,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            );
            if ret == 0 || mbi.RegionSize == 0 {
                break;
            }

            let region_start = (mbi.BaseAddress as usize).max(module_start);
            let region_end = (mbi.BaseAddress as usize)
                .saturating_add(mbi.RegionSize)
                .min(module_end);
            addr = (mbi.BaseAddress as usize).saturating_add(mbi.RegionSize);

            if mbi.State != MEM_COMMIT
                || mbi.Protect.contains(PAGE_NOACCESS)
                || mbi.Protect.contains(PAGE_GUARD)
                || region_end <= region_start
                || region_end - region_start < pattern.len()
            {
                continue;
            }

            let bytes = std::slice::from_raw_parts(
                region_start as *const u8,
                region_end.saturating_sub(region_start),
            );
            let scan_end = bytes.len() - pattern.len();
            for offset in 0..=scan_end {
                if pattern_matches_at(pattern, bytes, offset) {
                    results.push((region_start + offset) as *mut u8);
                }
            }
        }

        results
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (pattern, module);
        Vec::new()
    }
}

unsafe fn scan_vdf_context(module: Option<(usize, usize)>) -> Option<usize> {
    let pattern = Pattern::from_str(VDF_CTX_PATTERN).ok()?;
    let string_disp_offset = wildcard_set_start(&pattern, 3)?;
    let getter_disp_offset = wildcard_set_start(&pattern, 6)?;
    let expected = "Content::TERRAIN_TYPES == (m_terrainTypeTable.Count() + 1)";

    scan_module_all(&pattern, module).into_iter().find_map(|m| {
        let string_ptr = follow_relative_checked(m.add(string_disp_offset), module)?;
        if !cstr_eq_bounded(string_ptr as *const u8, expected) {
            return None;
        }
        follow_relative_checked(m.add(getter_disp_offset), module).map(|p| p as usize)
    })
}

pub fn initialize() {
    let mut rt = RUNTIME.lock();
    if rt.initialized {
        return;
    }

    let module = game_module_range();
    rt.get_vdf_ctx_fn = unsafe { scan_vdf_context(module).unwrap_or(0) };
    rt.prop_ctx_getter = unsafe {
        scan_raw_first(PROP_CTX_PATTERN, module)
            .or_else(|| scan_raw_first(PROP_CTX_PATTERN_FALLBACK, module))
            .unwrap_or(0)
    };
    rt.main_thread_match = unsafe { scan_raw_first(MAIN_THREAD_PATTERN, module).unwrap_or(0) };

    if rt.get_vdf_ctx_fn == 0 {
        rt.last_error = "GetVdfContext signature not found".to_string();
        rt.initialized = false;
        return;
    }

    rt.initialized = true;
    rt.last_error.clear();
}

pub fn is_playable() -> bool {
    let mut get_vdf_ctx_fn = RUNTIME.lock().get_vdf_ctx_fn;
    if get_vdf_ctx_fn == 0 {
        initialize();
        get_vdf_ctx_fn = RUNTIME.lock().get_vdf_ctx_fn;
        if get_vdf_ctx_fn == 0 {
            return false;
        }
    }
    unsafe { get_game_view(get_vdf_ctx_fn as *mut u8).is_some_and(|v| v == 16) }
}

pub fn init_error() -> String {
    RUNTIME.lock().last_error.clone()
}

pub fn read_live_equipment() -> Result<Vec<LiveSlotEntry>, String> {
    unsafe {
        let inv = match controlled_inventory_ptr() {
            Ok(v) => v,
            Err(e) => {
                RUNTIME.lock().last_error = e.clone();
                return Err(e);
            }
        };
        if !is_readable(inv as *const u8, 0x160 + 68 * 8) {
            return Err("inventory pointer unreadable".to_string());
        }

        let mut out = Vec::new();
        let eq_base = (inv + 0x160) as *const usize;
        for slot in 0..=67u32 {
            let item_ptr = *eq_base.add(slot as usize);
            if item_ptr == 0 || !is_readable(item_ptr as *const u8, 0x48) {
                continue;
            }
            let item_def_ptr = *((item_ptr + 0x40) as *const usize);
            if item_def_ptr == 0 || !is_readable(item_def_ptr as *const u8, 0x2c) {
                continue;
            }
            let item_id = *((item_def_ptr + 0x28) as *const u32);
            out.push(LiveSlotEntry {
                slot_index: slot,
                item_ptr,
                item_def_ptr,
                item_id,
            });
        }
        Ok(out)
    }
}

pub fn apply_itemdef_to_slot(item_def_ptr: usize, slot_index: u32) -> Result<(), String> {
    unsafe {
        if !is_playable() {
            return Err("not playable".to_string());
        }
        let inv = controlled_inventory_ptr().map_err(|e| {
            RUNTIME.lock().last_error = e.clone();
            e
        })?;
        if item_def_ptr == 0 || !is_readable(item_def_ptr as *const u8, 0x2c) {
            return Err("item_def_ptr invalid".to_string());
        }

        let vtable = *(inv as *const usize);
        if vtable == 0 || !is_readable(vtable as *const u8, 0x560) {
            return Err("inventory vtable invalid".to_string());
        }

        let fn_addr = *((vtable + 0x558) as *const usize);
        if fn_addr == 0 || !is_readable(fn_addr as *const u8, 1) {
            return Err("inventory apply function not valid".to_string());
        }

        type EquipByItemDefFn = unsafe extern "C" fn(*const u8, usize, u32) -> i32;
        let f: EquipByItemDefFn = std::mem::transmute(fn_addr);
        let ret = f(inv as *const u8, item_def_ptr, slot_index);
        if ret == 0 {
            return Err("game rejected equip operation".to_string());
        }

        Ok(())
    }
}

unsafe fn controlled_inventory_ptr() -> Result<usize, String> {
    let rt = RUNTIME.lock();
    let module = game_module_range().ok_or_else(|| "module range unavailable".to_string())?;
    let prop_getter = rt.prop_ctx_getter;
    let main_thread_match = rt.main_thread_match;
    drop(rt);

    if prop_getter == 0 {
        return Err("PropContext getter signature not found".to_string());
    }

    let main_thread_id = if main_thread_match == 0 {
        None
    } else {
        resolve_main_thread_id(main_thread_match as *mut u8)
    };

    let char_cli_ctx = if let Some(prop_ctx) = read_prop_ctx(prop_getter as *mut u8, main_thread_id) {
        let prop_ctx = prop_ctx as usize;
        if !is_readable(prop_ctx as *const u8, 0xA0) {
            return Err("PropContext unreadable".to_string());
        }
        let c = *((prop_ctx + 0x98) as *const usize);
        if c == 0 || !is_readable(c as *const u8, 0x90) {
            return Err("char_cli_ctx invalid from PropContext+0x98".to_string());
        }
        c
    } else {
        // Fallback chain from VdfContext gameplay assertion path:
        // FUN_1409b03b0() -> [ret + 0x98] (CharCliCtx)
        let getter_rva = CHAR_CONTEXT_GETTER_ABS
            .checked_sub(IMAGE_BASE)
            .ok_or_else(|| "getter RVA underflow".to_string())?;
        let getter_addr = module
            .0
            .checked_add(getter_rva)
            .ok_or_else(|| "getter RVA overflow".to_string())?;
        if !is_readable(getter_addr as *const u8, 1) {
            return Err(format!(
                "PropContext unavailable; fallback getter unreadable at 0x{getter_addr:X}"
            ));
        }
        type GetViewContextFn = unsafe extern "C" fn() -> usize;
        let get_ctx: GetViewContextFn = std::mem::transmute(getter_addr);
        let view_ctx = get_ctx();
        if view_ctx == 0 || !is_readable(view_ctx as *const u8, 0xA0) {
            return Err("PropContext unavailable; fallback view_ctx invalid".to_string());
        }
        let c = *((view_ctx + 0x98) as *const usize);
        if c == 0 || !is_readable(c as *const u8, 0x90) {
            return Err("PropContext unavailable; fallback char_cli_ctx invalid".to_string());
        }
        c
    };

    let cc_vt = *(char_cli_ctx as *const usize);
    if cc_vt == 0 || !is_readable(cc_vt as *const u8, 0x68) {
        return Err("char_cli_ctx vtable invalid".to_string());
    }

    // GW2RE maps GetControlledCharacter at vtable +0x50 for ChCliContext.
    let mut get_controlled_character_addr = *((cc_vt + 0x50) as *const usize);
    if get_controlled_character_addr == 0 || !is_readable(get_controlled_character_addr as *const u8, 1) {
        // Fallback seen in decompiled Vdf flow on some builds.
        get_controlled_character_addr = *((cc_vt + 0x60) as *const usize);
    }
    if get_controlled_character_addr == 0 || !is_readable(get_controlled_character_addr as *const u8, 1) {
        return Err("GetControlledCharacter fn ptr invalid".to_string());
    }

    type GetControlledCharacterFn = unsafe extern "C" fn(usize) -> usize;
    let get_char: GetControlledCharacterFn = std::mem::transmute(get_controlled_character_addr);
    let character = get_char(char_cli_ctx);
    if character == 0 || !is_readable(character as *const u8, 0x10) {
        return Err("controlled character invalid".to_string());
    }

    let subclass_vtable = *((character + 8) as *const usize);
    if subclass_vtable == 0 || !is_readable(subclass_vtable as *const u8, 0xD0) {
        return Err("character subclass vtable invalid".to_string());
    }

    let get_inventory_addr = *((subclass_vtable + 0xC8) as *const usize);
    if get_inventory_addr == 0 || !is_readable(get_inventory_addr as *const u8, 1) {
        return Err("GetInventory fn ptr invalid".to_string());
    }

    type GetInventoryFn = unsafe extern "C" fn(usize) -> usize;
    let get_inventory: GetInventoryFn = std::mem::transmute(get_inventory_addr);
    // Known wrapper passes &character->SubclassVTable (character + 8), but keep a fallback.
    let mut inv = get_inventory(character + 8);
    if inv == 0 {
        inv = get_inventory(character);
    }
    if inv == 0 {
        return Err("inventory pointer null".to_string());
    }
    Ok(inv)
}

fn scan_raw_first(pattern: &str, module: Option<(usize, usize)>) -> Option<usize> {
    let p = Pattern::from_str(pattern).ok()?;
    let hits = unsafe { scan_module_all(&p, module) };
    hits.first().map(|h| *h as usize)
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
unsafe fn resolve_main_thread_id(match_addr: *mut u8) -> Option<u32> {
    if match_addr.is_null() || !is_readable(match_addr as *const u8, 11) {
        return None;
    }
    let disp = (match_addr.add(7) as *const i32).read_unaligned();
    let global_addr = match_addr.add(11).offset(disp as isize);
    if !is_readable(global_addr as *const u8, 4) {
        return None;
    }
    let tid = *(global_addr as *const u32);
    if tid == 0 { None } else { Some(tid) }
}

#[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
unsafe fn resolve_main_thread_id(_match_addr: *mut u8) -> Option<u32> {
    None
}

#[cfg(target_os = "windows")]
unsafe fn read_teb_of_thread(thread_id: u32) -> Option<*const u8> {
    let handle = OpenThread(THREAD_QUERY_INFORMATION, false, thread_id).ok()?;
    let ntdll = GetModuleHandleA(windows::core::s!("ntdll.dll")).ok()?;
    let proc = GetProcAddress(ntdll, windows::core::s!("NtQueryInformationThread"))?;

    type NtQueryInfoThread = unsafe extern "system" fn(
        handle: *mut core::ffi::c_void,
        class: u32,
        info: *mut u8,
        len: u32,
        ret_len: *mut u32,
    ) -> i32;

    let nt_query: NtQueryInfoThread = std::mem::transmute(proc);
    let mut info = [0u8; 0x30];
    let status = nt_query(handle.0, 0, info.as_mut_ptr(), 0x30, std::ptr::null_mut());
    let _ = CloseHandle(handle);
    if status != 0 {
        return None;
    }
    let teb = *(info.as_ptr().add(0x08) as *const *const u8);
    if teb.is_null() || !is_readable(teb, 0x60) { None } else { Some(teb) }
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
unsafe fn read_prop_ctx(fn_ptr: *mut u8, main_thread_id: Option<u32>) -> Option<*const u8> {
    if fn_ptr.is_null() || !is_readable(fn_ptr as *const u8, 30) {
        return None;
    }

    let b0 = *fn_ptr;
    let b1 = *fn_ptr.add(1);
    if b0 != 0x8B || (b1 != 0x0D && b1 != 0x15) {
        return None;
    }

    let disp = (fn_ptr.add(2) as *const i32).read_unaligned();
    let tls_index_ptr = fn_ptr.add(6).offset(disp as isize) as *const u32;
    if !is_readable(tls_index_ptr as *const u8, 4) {
        return None;
    }
    let tls_index = *tls_index_ptr as usize;

    let tls_offset = if *fn_ptr.add(15) == 0xBA {
        (fn_ptr.add(16) as *const u32).read_unaligned() as usize
    } else if *fn_ptr.add(15) == 0x41 && *fn_ptr.add(16) == 0xB8 {
        (fn_ptr.add(17) as *const u32).read_unaligned() as usize
    } else {
        return None;
    };

    let tls_array: *const *const u8 = if let Some(tid) = main_thread_id {
        let teb = read_teb_of_thread(tid)?;
        if !is_readable(teb.add(0x58), std::mem::size_of::<usize>()) {
            return None;
        }
        *(teb.add(0x58) as *const *const *const u8)
    } else {
        let ptr: *const *const u8;
        core::arch::asm!("mov {}, gs:[0x58]", out(reg) ptr, options(nostack, preserves_flags));
        ptr
    };
    if tls_array.is_null() {
        return None;
    }
    let tls_entry = tls_array.add(tls_index) as *const u8;
    if !is_readable(tls_entry, std::mem::size_of::<usize>()) {
        return None;
    }
    let tls_block = *tls_array.add(tls_index);
    if tls_block.is_null() {
        return None;
    }
    let prop_ctx_slot = tls_block.add(tls_offset);
    if !is_readable(prop_ctx_slot, std::mem::size_of::<usize>()) {
        return None;
    }
    let prop_ctx = *(prop_ctx_slot as *const *const u8);
    if prop_ctx.is_null() || !is_readable(prop_ctx, 0x300) {
        None
    } else {
        Some(prop_ctx)
    }
}

#[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
unsafe fn read_prop_ctx(_fn_ptr: *mut u8, _main_thread_id: Option<u32>) -> Option<*const u8> {
    None
}

unsafe fn get_game_view(get_vdf_ctx_fn: *mut u8) -> Option<u32> {
    if get_vdf_ctx_fn.is_null() {
        return None;
    }

    if let Some((start, end)) = game_module_range() {
        let addr = get_vdf_ctx_fn as usize;
        if addr < start || addr >= end {
            return None;
        }
    }

    type GetVdfCtxFn = unsafe extern "C" fn() -> *const u8;
    let get_ctx: GetVdfCtxFn = std::mem::transmute(get_vdf_ctx_fn);
    let vdf_ctx = get_ctx();
    if vdf_ctx.is_null() || !is_readable(vdf_ctx, std::mem::size_of::<usize>()) {
        return None;
    }

    let vtable = *(vdf_ctx as *const *const u8);
    if vtable.is_null() || !is_readable(vtable, 0x118) {
        return None;
    }

    let fn_slot = vtable.add(0x110);
    if !is_readable(fn_slot, std::mem::size_of::<usize>()) {
        return None;
    }
    let get_view_addr = *(fn_slot as *const usize);
    if get_view_addr == 0 || !is_readable(get_view_addr as *const u8, 1) {
        return None;
    }

    type GetViewFn = unsafe extern "C" fn(*const u8) -> u32;
    let get_view: GetViewFn = std::mem::transmute(get_view_addr);
    Some(get_view(vdf_ctx))
}
