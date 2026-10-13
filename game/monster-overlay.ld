/* The shared SDK excludes this same-link code/table image from the resident
   executable. Runtime storage borrows the existing map transfer allocation. */
MEMORY { QUAKE_MONSTER_COLD (rx) : ORIGIN = 0x80408000, LENGTH = 64K
    QUAKE_DOOR_COLD (rx) : ORIGIN = 0x80418000, LENGTH = 64K }
SECTIONS {
    .overlay.quake_monsters.text : ALIGN(4) {
        *(.overlay.quake_monsters.text .overlay.quake_monsters.text.*)
    } > QUAKE_MONSTER_COLD
    .overlay.quake_monsters.rodata : ALIGN(4) {
        *(.overlay.quake_monsters.rodata .overlay.quake_monsters.rodata.*)
        *(.rodata.*14MonsterRuntime13advance_ticks*)
        *(.rodata.*14MonsterRuntime5think*)
        *(.rodata.quake_monster_advance*)
        *(.rodata.*11EntityScene23update_monster_missiles*)
        *(.rodata.*15ImpactParticles18tick_death_bubbles*)
        *(.rodata.quake_monster_missiles*)
        *(.rodata.quake_bubble_tick*)
    } > QUAKE_MONSTER_COLD
    .overlay.quake_doors.text : ALIGN(4) {
        *(.overlay.quake_doors.text .overlay.quake_doors.text.*)
        *(.text.*11EntityScene16link_door_chains*)
    } > QUAKE_DOOR_COLD
    .overlay.quake_doors.rodata : ALIGN(4) {
        *(.overlay.quake_doors.rodata .overlay.quake_doors.rodata.*)
        *(.rodata.*11EntityScene10link_doors*)
        *(.rodata.*11EntityScene16link_door_chains*)
        *(.rodata.quake_door_link*)
        *(.rodata.*10QuakeMover11from_entity*)
        *(.rodata.quake_mover_from_entity*)
    } > QUAKE_DOOR_COLD
} INSERT BEFORE .text;
