//! Bounded x86_64 ELF GOT transaction; preload does not need GOT rewriting.
use libc::{c_int, c_void};
use std::ffi::CStr;
use std::mem::size_of;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};

const DT_NULL: i64 = 0;
const DT_PLTRELSZ: i64 = 2;
const DT_STRTAB: i64 = 5;
const DT_SYMTAB: i64 = 6;
const DT_RELA: i64 = 7;
const DT_RELASZ: i64 = 8;
const DT_RELAENT: i64 = 9;
const DT_SYMENT: i64 = 11;
const DT_PLTREL: i64 = 20;
const DT_JMPREL: i64 = 23;
const DT_RELA_TAG: u64 = 7;
const R_X86_64_GLOB_DAT: u32 = 6;
const R_X86_64_JUMP_SLOT: u32 = 7;
const MAX_DYNAMIC: usize = 4096;

#[repr(C)]
#[derive(Clone, Copy)]
struct Dynamic {
    tag: i64,
    value: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Rela {
    offset: u64,
    info: u64,
    addend: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Symbol {
    name: u32,
    info: u8,
    other: u8,
    section: u16,
    value: u64,
    size: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct Target {
    pub(crate) name: &'static [u8],
    pub(crate) replacement: *const c_void,
    pub(crate) is_swap: bool,
}

struct MemoryRegion {
    start: usize,
    end: usize,
    protection: c_int,
}

struct Patch {
    slot: *mut usize,
    original: usize,
    protection: c_int,
}

struct Install<'a> {
    targets: &'a [Target],
    regions: Vec<MemoryRegion>,
    patches: Vec<Patch>,
    failed: bool,
    swap_covered: bool,
}

/// Rewrites already-loaded ELF GOT slots. This is used only by the explicit
/// attach export after `dlopen`; preload interposition does not need it.
pub(crate) unsafe fn install(targets: &[Target]) -> bool {
    if !cfg!(target_arch = "x86_64") {
        return false;
    }
    let Some(regions) = memory_regions() else {
        return false;
    };
    let mut state = Install {
        targets,
        regions,
        patches: Vec::with_capacity(16384),
        failed: false,
        swap_covered: false,
    };
    // SAFETY: callback bounds every program-header and dynamic-table walk.
    unsafe {
        libc::dl_iterate_phdr(Some(iterate), (&mut state as *mut Install<'_>).cast());
    }
    if state.failed || !state.swap_covered {
        // Roll back in reverse order. A rollback failure still returns false;
        // callers must not treat a partially installed hook as usable.
        for patch in state.patches.iter().rev() {
            let _ = unsafe { write_slot(patch.slot, patch.original, patch.protection) };
        }
        false
    } else {
        true
    }
}

unsafe extern "C" fn iterate(
    information: *mut libc::dl_phdr_info,
    _: usize,
    opaque: *mut c_void,
) -> c_int {
    if information.is_null() || opaque.is_null() {
        return 0;
    }
    // SAFETY: dl_iterate_phdr supplies these pointers for this callback only.
    let information = unsafe { &*information };
    let state = unsafe { &mut *opaque.cast::<Install<'_>>() };
    if state.failed {
        return 1;
    }
    let name = if information.dlpi_name.is_null() {
        &[][..]
    } else {
        // SAFETY: loader-owned module name is NUL-terminated during callback.
        unsafe { CStr::from_ptr(information.dlpi_name) }.to_bytes()
    };
    if excluded(name) {
        return 0;
    }

    let count = information.dlpi_phnum as usize;
    if information.dlpi_phdr.is_null() || count == 0 {
        return 0;
    }
    // SAFETY: loader reports exactly dlpi_phnum headers.
    let headers = unsafe { std::slice::from_raw_parts(information.dlpi_phdr, count) };
    let Some(dynamic_header) = headers
        .iter()
        .find(|header| header.p_type == libc::PT_DYNAMIC)
    else {
        return 0;
    };
    let base = information.dlpi_addr as usize;
    let dynamic_address = match base.checked_add(dynamic_header.p_vaddr as usize) {
        Some(address) => address,
        None => {
            state.failed = true;
            return 1;
        }
    };
    let maximum = (dynamic_header.p_memsz as usize / size_of::<Dynamic>()).min(MAX_DYNAMIC);
    if maximum == 0 {
        return 0;
    }
    if !mapped(
        &state.regions,
        dynamic_address,
        maximum * size_of::<Dynamic>(),
    ) {
        state.failed = true;
        return 1;
    }
    // SAFETY: PT_DYNAMIC is loader-mapped and p_memsz bounds this view.
    let dynamics =
        unsafe { std::slice::from_raw_parts(dynamic_address as *const Dynamic, maximum) };

    let mut string_table = 0usize;
    let mut symbol_table = 0usize;
    let mut symbol_size = size_of::<Symbol>();
    let mut rela = 0usize;
    let mut rela_size = 0usize;
    let mut rela_entry = size_of::<Rela>();
    let mut jump = 0usize;
    let mut jump_size = 0usize;
    let mut plt_rela = false;
    for entry in dynamics {
        match entry.tag {
            DT_NULL => break,
            DT_STRTAB => string_table = pointer(base, entry.value),
            DT_SYMTAB => symbol_table = pointer(base, entry.value),
            DT_SYMENT => symbol_size = entry.value as usize,
            DT_RELA => rela = pointer(base, entry.value),
            DT_RELASZ => rela_size = entry.value as usize,
            DT_RELAENT => rela_entry = entry.value as usize,
            DT_JMPREL => jump = pointer(base, entry.value),
            DT_PLTRELSZ => jump_size = entry.value as usize,
            DT_PLTREL => plt_rela = entry.value == DT_RELA_TAG,
            _ => {}
        }
    }
    if string_table == 0 || symbol_table == 0 || symbol_size != size_of::<Symbol>() {
        return 0;
    }
    if rela != 0 && rela_entry == size_of::<Rela>() {
        unsafe { patch_relocations(state, base, string_table, symbol_table, rela, rela_size) };
    }
    if !state.failed && jump != 0 && plt_rela {
        unsafe { patch_relocations(state, base, string_table, symbol_table, jump, jump_size) };
    }
    if state.failed { 1 } else { 0 }
}

unsafe fn patch_relocations(
    state: &mut Install<'_>,
    base: usize,
    strings: usize,
    symbols: usize,
    relocations: usize,
    bytes: usize,
) {
    if bytes == 0 || !bytes.is_multiple_of(size_of::<Rela>()) {
        return;
    }
    let count = bytes / size_of::<Rela>();
    if count > 65536 || !mapped(&state.regions, relocations, bytes) {
        state.failed = true;
        return;
    }
    // SAFETY: DT_RELASZ is required to bound a DT_RELA/JMPREL array.
    let relocations = unsafe { std::slice::from_raw_parts(relocations as *const Rela, count) };
    for relocation in relocations {
        if state.failed {
            break;
        }
        let kind = relocation.info as u32;
        if kind != R_X86_64_JUMP_SLOT && kind != R_X86_64_GLOB_DAT {
            continue;
        }
        let symbol_index = (relocation.info >> 32) as usize;
        let Some(symbol_address) =
            symbols.checked_add(symbol_index.saturating_mul(size_of::<Symbol>()))
        else {
            state.failed = true;
            break;
        };
        if !mapped(&state.regions, symbol_address, size_of::<Symbol>()) {
            state.failed = true;
            break;
        }
        // SAFETY: ELF relocation references a symbol in this module's DT_SYMTAB.
        let symbol = unsafe { ptr::read(symbol_address as *const Symbol) };
        let Some(name_address) = strings.checked_add(symbol.name as usize) else {
            state.failed = true;
            break;
        };
        let Some(region) = state.regions.iter().find(|r| {
            r.start <= name_address && name_address < r.end && r.protection & libc::PROT_READ != 0
        }) else {
            state.failed = true;
            break;
        };
        let bytes = unsafe {
            std::slice::from_raw_parts(
                name_address as *const u8,
                (region.end - name_address).min(256),
            )
        };
        let Some(end) = bytes.iter().position(|b| *b == 0) else {
            state.failed = true;
            break;
        };
        let name = &bytes[..end];
        let Some(target) = state.targets.iter().find(|target| target.name == name) else {
            continue;
        };
        let Some(slot_address) = base.checked_add(relocation.offset as usize) else {
            state.failed = true;
            break;
        };
        if slot_address % size_of::<usize>() != 0 || state.patches.len() == 16384 {
            state.failed = true;
            break;
        }
        let slot = slot_address as *mut usize;
        if state.patches.iter().any(|patch| patch.slot == slot) {
            continue;
        }
        let Some(region) = state.regions.iter().find(|region| {
            region.start <= slot_address
                && slot_address
                    .checked_add(size_of::<usize>())
                    .is_some_and(|end| end <= region.end)
                && region.protection & libc::PROT_READ != 0
        }) else {
            state.failed = true;
            break;
        };
        // SAFETY: the relocation slot is mapped and pointer-sized.
        let original = unsafe { (&*slot.cast::<AtomicUsize>()).load(Ordering::Acquire) };
        let replacement = target.replacement as usize;
        if original == replacement {
            // A preload may already own this presentation boundary. It is
            // covered without another write and needs no rollback entry.
            state.swap_covered |= target.is_swap;
            continue;
        }
        if unsafe { write_slot(slot, replacement, region.protection) }.is_err() {
            state.failed = true;
            break;
        }
        state.patches.push(Patch {
            slot,
            original,
            protection: region.protection,
        });
        state.swap_covered |= target.is_swap;
    }
}

unsafe fn write_slot(slot: *mut usize, value: usize, protection: c_int) -> Result<(), ()> {
    // SAFETY: sysconf has no pointer preconditions.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page_size <= 0 {
        return Err(());
    }
    let page_size = page_size as usize;
    if !page_size.is_power_of_two() {
        return Err(());
    }
    let page = (slot as usize) & !(page_size - 1);
    // SAFETY: caller validated that slot is mapped and readable.
    let previous = unsafe { (&*slot.cast::<AtomicUsize>()).load(Ordering::Acquire) };
    // SAFETY: page is mapped according to /proc/self/maps and aligned.
    if unsafe {
        libc::mprotect(
            page as *mut c_void,
            page_size,
            protection | libc::PROT_WRITE,
        )
    } != 0
    {
        return Err(());
    }
    // SAFETY: caller validated a writable pointer-sized relocation slot.
    unsafe {
        (&*slot.cast::<AtomicUsize>()).store(value, Ordering::Release);
    }
    // SAFETY: restore the exact original mapping protection immediately.
    if unsafe { libc::mprotect(page as *mut c_void, page_size, protection) } != 0 {
        // The page is still writable, so restore the slot before failing. A
        // second protection restore is best-effort; the caller reports failure.
        unsafe {
            (&*slot.cast::<AtomicUsize>()).store(previous, Ordering::Release);
        }
        let _ = unsafe { libc::mprotect(page as *mut c_void, page_size, protection) };
        return Err(());
    }
    Ok(())
}

fn memory_regions() -> Option<Vec<MemoryRegion>> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    let mut regions = Vec::new();
    for line in maps.lines() {
        let mut fields = line.split_whitespace();
        let mut range = fields.next()?.split('-');
        let start = usize::from_str_radix(range.next()?, 16).ok()?;
        let end = usize::from_str_radix(range.next()?, 16).ok()?;
        let permissions = fields.next()?.as_bytes();
        if permissions.len() < 3 {
            return None;
        }
        let mut protection = 0;
        if permissions[0] == b'r' {
            protection |= libc::PROT_READ;
        }
        if permissions[1] == b'w' {
            protection |= libc::PROT_WRITE;
        }
        if permissions[2] == b'x' {
            protection |= libc::PROT_EXEC;
        }
        regions.push(MemoryRegion {
            start,
            end,
            protection,
        });
    }
    Some(regions)
}

fn pointer(base: usize, value: u64) -> usize {
    // Dynamic pointers in an in-memory ET_DYN image are normally relative to
    // dlpi_addr. A value already inside a mapped high address range is absolute.
    let value = value as usize;
    if value >= base && base != 0 {
        value
    } else {
        base.saturating_add(value)
    }
}

fn excluded(name: &[u8]) -> bool {
    fn contains(name: &[u8], part: &[u8]) -> bool {
        name.windows(part.len()).any(|window| window == part)
    }
    contains(name, b"libvosulay_overlay")
        || contains(name, b"libGL.so")
        || contains(name, b"libEGL.so")
        || contains(name, b"libOpenGL.so")
        || contains(name, b"libGLX.so")
}

fn mapped(regions: &[MemoryRegion], address: usize, bytes: usize) -> bool {
    address.checked_add(bytes).is_some_and(|end| {
        regions
            .iter()
            .any(|r| r.start <= address && end <= r.end && r.protection & libc::PROT_READ != 0)
    })
}
