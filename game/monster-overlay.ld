/* The shared SDK excludes this same-link code/table image from the resident
   executable. Runtime storage borrows the existing map transfer allocation. */
MEMORY { QUAKE_MONSTER_COLD (rx) : ORIGIN = 0x80408000, LENGTH = 64K
    QUAKE_DOOR_COLD (rx) : ORIGIN = 0x80418000, LENGTH = 64K
    QUAKE_SAVE_COLD (rx) : ORIGIN = 0x80428000, LENGTH = 64K
    QUAKE_LOAD_COLD (rx) : ORIGIN = 0x80438000, LENGTH = 64K
    QUAKE_CARD_COLD (rx) : ORIGIN = 0x80448000, LENGTH = 64K }
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
    /* The pause menu's SAVE GAME and LOAD GAME: encoder, decoder and the
       memory-card driver, three images so that none exceeds a transfer half.
       A function belongs to one image and may call only itself and resident
       code, so the shared checksum stays resident. */
    .overlay.quake_save.text : ALIGN(4) {
        *(.overlay.quake_save.text .overlay.quake_save.text.*)
        *(.text.*save_dynamic*)
        *(.text.*Snapshot4save*)
        *(.text.*4saveNtB*6Writer*)
        *(.text.*4save5frame*)
        *(.text.*write_words*)
        *(.text.*save_activator*)
        *(.text.*save_state*)
        *(.text.*save_bits*)
    } > QUAKE_SAVE_COLD
    .overlay.quake_save.rodata : ALIGN(4) {
        *(.overlay.quake_save.rodata .overlay.quake_save.rodata.*)
        *(.rodata.*save_dynamic*)
        *(.rodata.*Snapshot4save*)
        *(.rodata.*write_words*)
        *(.rodata.*save_activator*)
        *(.rodata.*save_state*)
        *(.rodata.*save_bits*)
        *(.rodata.*8savegame6encode*)
    } > QUAKE_SAVE_COLD
    .overlay.quake_load.text : ALIGN(4) {
        *(.overlay.quake_load.text .overlay.quake_load.text.*)
        *(.text.*load_dynamic*)
        *(.text.*Snapshot4load*)
        *(.text.*4saveNtB*6Reader*)
        *(.text.*read_words*)
        *(.text.*load_activator*)
        *(.text.*load_state*)
        *(.text.*load_bits*)
    } > QUAKE_LOAD_COLD
    .overlay.quake_load.rodata : ALIGN(4) {
        *(.overlay.quake_load.rodata .overlay.quake_load.rodata.*)
        *(.rodata.*load_dynamic*)
        *(.rodata.*Snapshot4load*)
        *(.rodata.*read_words*)
        *(.rodata.*load_activator*)
        *(.rodata.*load_state*)
        *(.rodata.*load_bits*)
        *(.rodata.*8savegame7restore*)
    } > QUAKE_LOAD_COLD
    .overlay.quake_card.text : ALIGN(4) {
        *(.overlay.quake_card.text .overlay.quake_card.text.*)
        *(.text.*8savegame4card*)
        *(.text.*6psx_mc*)
    } > QUAKE_CARD_COLD
    .overlay.quake_card.rodata : ALIGN(4) {
        *(.overlay.quake_card.rodata .overlay.quake_card.rodata.*)
        *(.rodata.*8savegame4card*)
        *(.rodata.*6psx_mc*)
    } > QUAKE_CARD_COLD
} INSERT BEFORE .text;
