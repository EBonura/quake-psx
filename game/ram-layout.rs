// Quake's top-of-RAM layout, the one copy. `game/build.rs` writes both values into linker scripts
// ahead of the SDK's psoxide.ld, and `host/quake-build` reads them to know where the linked heap
// ends (`include!`d by both, so the link and the ship-boot heap check cannot disagree).

/// RAM the program links for (SDK psoxide.ld:16-21). 0x1FE000 puts STACK_INIT at 0x801FDF00 and
/// leaves 0x801FDF00..0x80200000 to the scene system's resident core window (8,192 B) and its
/// 256 B handoff block. PROVISIONAL: the window size comes from the scene-system plan (campaign
/// scene-system-1123); the final number comes after the core's first proof run.
pub const RAM_SIZE_OVERRIDE: u32 = 0x1F_E000;

/// Stack reserve below STACK_INIT (SDK STACK_RESERVE_OVERRIDE hook); see game/build.rs for why
/// Quake needs 52 KiB.
pub const STACK_RESERVE_OVERRIDE: u32 = 0xD000;

/// psoxide.ld: STACK_INIT = RAM_BASE + RAM_SIZE - 0x100.
pub const STACK_INIT: u32 = 0x8000_0000 + RAM_SIZE_OVERRIDE - 0x100;

/// psoxide.ld: __heap_end = STACK_INIT - STACK_RESERVE.
pub const HEAP_END: u32 = STACK_INIT - STACK_RESERVE_OVERRIDE;
