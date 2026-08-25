//! Platform-neutral Quake boot, pause, options, and controls-menu policy.
//!
//! The game adapter translates pad edges into [`MenuInput`]. Rendering owns
//! GPU packets; this module exposes only stable labels, values, and state.

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MenuPage {
    Main,
    Levels,
    Options,
    Controls,
    Cheats,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MenuAction {
    NewGame,
    /// Start a fresh game on the level at this index of
    /// [`crate::level::LEVEL_NAMES`].
    StartLevel(u8),
    Resume,
    /// Original Quake's developer `impulse 9`: fill every weapon and ammo
    /// bit without choosing a different active weapon.
    Impulse9,
}

/// The original menu's three distinct UI sound roles. The game adapter maps
/// these to `misc/menu1.wav`, `misc/menu2.wav`, and `misc/menu3.wav` after the
/// input policy has decided what the press actually did.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MenuSound {
    Move,
    Enter,
    Adjust,
}

/// The two useful status-bar depths from the original game's view-size
/// continuum, exposed explicitly for a fixed-resolution console port.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HudMode {
    Minimal,
    Classic,
}

/// The compact console overlay leaves the maximum view area visible. The
/// original two-tier stone status bar remains available from Options.
pub const DEFAULT_HUD_MODE: HudMode = HudMode::Minimal;

impl HudMode {
    #[optimize(size)]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Minimal => "MINIMAL",
            Self::Classic => "CLASSIC",
        }
    }
}

/// The Levels page rows, in [`crate::level::LEVEL_NAMES`] order: the cooked
/// map name and its authored title.
pub const LEVEL_ROWS: [&str; 9] = [
    "START INTRODUCTION",
    "E1M1 SLIPGATE COMPLEX",
    "E1M2 CASTLE OF THE DAMNED",
    "E1M3 THE NECROPOLIS",
    "E1M4 THE GRISLY GROTTO",
    "E1M5 GLOOM KEEP",
    "E1M6 THE DOOR TO CHTHON",
    "E1M7 THE HOUSE OF CHTHON",
    "E1M8 ZIGGURAT VERTIGO",
];

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MenuInput {
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
    pub accept: bool,
    pub back: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MenuRow {
    pub label: &'static str,
    pub value: Option<&'static str>,
}

impl MenuRow {
    #[optimize(size)]
    const fn plain(label: &'static str) -> Self {
        Self { label, value: None }
    }

    #[optimize(size)]
    const fn valued(label: &'static str, value: &'static str) -> Self {
        Self {
            label,
            value: Some(value),
        }
    }
}

/// What the PSoXide demo disc calls its four shared menu songs, in disc order.
/// The `TRACK` row names them, and the game's `music` module counts them; both
/// live off this one table so the Options page cannot label a song the drive is
/// not playing. A disc whose menu tracklist changes mislabels as well as
/// misplays, which is the price of borrowing the launcher's audio.
pub const MUSIC_TRACKS: [&str; 4] = [
    "KNUCKLE DUST",
    "RUSTED HAMMER",
    "CHAINSAW HEART",
    "NIGHT CRAWLER",
];

/// Informational lines shown by the Controls page above its Back row.
pub const CONTROL_LINES: [&str; 7] = [
    "LEFT STICK / D-PAD   MOVE",
    "RIGHT STICK          LOOK",
    "CROSS                JUMP",
    "SQUARE               USE",
    "R2                   FIRE",
    "L1/R1 CYCLE TRIANGLE+D-PAD",
    "START / SELECT       MENU",
];

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MenuView {
    pub page: MenuPage,
    pub selected: u8,
    pub can_resume: bool,
    pub look_speed: u8,
    pub invert_y: bool,
    /// Index into [`DEADZONE_RADII`], applied to both scaled-radial sticks.
    pub deadzone: u8,
    /// Palette gamma row, `0..BRIGHTNESS_STEPS`; the renderer maps it to a
    /// cooked CLUT row.
    pub brightness: u8,
    /// `crosshair`. The original ships it off; a pad without a mouse wants it
    /// on, so this port ships it on and lets the row take it away.
    pub crosshair: bool,
    /// PS1-native equivalent of the original underwater screen warp.
    pub water_warp: bool,
    /// Opt-in PS1 translucency with bounded visibility through liquid planes.
    pub water_alpha: bool,
    /// Compact console overlay or the original two-tier status bar.
    pub hud_mode: HudMode,
    /// The `skill` cvar a new game starts on. Start's own skill doors overwrite
    /// it the moment the player walks one, exactly like the original.
    pub skill: u8,
    /// Original `volume`, represented as the menu's ten slider steps.
    pub sound_volume: u8,
    /// The disc carries the shared menu tracks, so the music rows are real.
    pub music_available: bool,
    pub music_on: bool,
    /// Original `bgmvolume`, represented as the menu's ten slider steps.
    pub music_volume: u8,
    /// Index into [`MUSIC_TRACKS`].
    pub track: u8,
    /// Original `god` and `noclip` developer toggles. The page exposing them
    /// only exists in the pause menu, so a boot-time new game cannot erase a
    /// choice the player just made.
    pub god_mode: bool,
    pub noclip: bool,
}

/// Brightness steps the Options page offers (the cooker's palette rows).
pub const BRIGHTNESS_STEPS: u8 = 6;
/// The default brightness step: the cooker's brightest 0.5 power row. The console
/// picture reads far darker than the emulator's (a captured menu scene came
/// out at a third of the emulator's mean luminance), and the original ships
/// dark enough that everyone reached for its brightness slider anyway.
pub const DEFAULT_BRIGHTNESS: u8 = BRIGHTNESS_STEPS - 1;
const BRIGHTNESS_LABELS: [&str; BRIGHTNESS_STEPS as usize] = ["1", "2", "3", "4", "5", "6"];
/// Scaled-radial inner radii offered for both DualShock sticks.
pub const DEADZONE_RADII: [i16; 5] = [12, 20, 28, 36, 44];
const DEADZONE_LABELS: [&str; DEADZONE_RADII.len()] = ["12", "20", "28", "36", "44"];
/// Healthy-pad default inherited from the port's original fixed filter.
pub const DEFAULT_DEADZONE: u8 = 2;
/// The four `skill` values the entity loader reads, in cvar order.
pub const SKILL_LABELS: [&str; 4] = ["EASY", "NORMAL", "HARD", "NIGHTMARE"];
/// `skill 1`: what the original's own menu starts a new game on.
pub const DEFAULT_SKILL: u8 = 1;
/// The original options sliders expose ten intervals from silent to full.
pub const VOLUME_STEPS: u8 = 10;
/// `volume 0.7`, the original sound-effect default.
pub const DEFAULT_SOUND_VOLUME: u8 = 7;
/// `bgmvolume 1`, the original music default.
pub const DEFAULT_MUSIC_VOLUME: u8 = VOLUME_STEPS;
pub const OPTIONS_SOUND_VOLUME_ROW: u8 = 9;
pub const OPTIONS_MUSIC_ROW: u8 = 10;
pub const OPTIONS_MUSIC_VOLUME_ROW: u8 = 11;
pub const OPTIONS_TRACK_ROW: u8 = 12;

impl MenuView {
    #[optimize(size)]
    pub const fn title(self) -> &'static str {
        match self.page {
            MenuPage::Main => "MAIN MENU",
            MenuPage::Levels => "LEVELS",
            MenuPage::Options => "OPTIONS",
            MenuPage::Controls => "CONTROLS",
            MenuPage::Cheats => "CHEATS",
        }
    }

    #[optimize(size)]
    pub const fn row_count(self) -> u8 {
        match self.page {
            MenuPage::Main => 4 + 2 * self.can_resume as u8,
            MenuPage::Levels => LEVEL_ROWS.len() as u8 + 1,
            MenuPage::Options => self.options_back_row() + 1,
            MenuPage::Controls => 1,
            MenuPage::Cheats => 4,
        }
    }

    /// Which Options row is `BACK`. The music rows sit above it and only when
    /// the disc actually has the songs. Sound volume is always present;
    /// music, music volume, and track are conditional.
    #[optimize(size)]
    pub const fn options_back_row(self) -> u8 {
        10 + 3 * self.music_available as u8
    }

    #[optimize(size)]
    pub const fn row(self, index: u8) -> Option<MenuRow> {
        match self.page {
            MenuPage::Main if self.can_resume => match index {
                0 => Some(MenuRow::plain("RESUME")),
                1 => Some(MenuRow::plain("NEW GAME")),
                2 => Some(MenuRow::plain("LEVELS")),
                3 => Some(MenuRow::plain("OPTIONS")),
                4 => Some(MenuRow::plain("CONTROLS")),
                5 => Some(MenuRow::plain("CHEATS")),
                _ => None,
            },
            MenuPage::Main => match index {
                0 => Some(MenuRow::plain("NEW GAME")),
                1 => Some(MenuRow::plain("LEVELS")),
                2 => Some(MenuRow::plain("OPTIONS")),
                3 => Some(MenuRow::plain("CONTROLS")),
                _ => None,
            },
            MenuPage::Levels => {
                if (index as usize) < LEVEL_ROWS.len() {
                    Some(MenuRow::plain(LEVEL_ROWS[index as usize]))
                } else if index as usize == LEVEL_ROWS.len() {
                    Some(MenuRow::plain("BACK"))
                } else {
                    None
                }
            }
            MenuPage::Options => match index {
                0 => Some(MenuRow::valued(
                    "LOOK",
                    LOOK_SPEED_LABELS[if self.look_speed < 5 {
                        self.look_speed as usize
                    } else {
                        4
                    }],
                )),
                1 => Some(MenuRow::valued(
                    "INVERT Y",
                    if self.invert_y { "ON" } else { "OFF" },
                )),
                2 => Some(MenuRow::valued(
                    "DEADZONE",
                    DEADZONE_LABELS[if (self.deadzone as usize) < DEADZONE_LABELS.len() {
                        self.deadzone as usize
                    } else {
                        DEFAULT_DEADZONE as usize
                    }],
                )),
                3 => Some(MenuRow::valued(
                    "GAMMA",
                    BRIGHTNESS_LABELS[if self.brightness < BRIGHTNESS_STEPS {
                        self.brightness as usize
                    } else {
                        BRIGHTNESS_STEPS as usize - 1
                    }],
                )),
                4 => Some(MenuRow::valued(
                    "CROSS",
                    if self.crosshair { "ON" } else { "OFF" },
                )),
                5 => Some(MenuRow::valued(
                    "WARP",
                    if self.water_warp { "ON" } else { "OFF" },
                )),
                6 => Some(MenuRow::valued(
                    "CLEAR WATER",
                    if self.water_alpha { "ON" } else { "OFF" },
                )),
                7 => Some(MenuRow::valued("HUD", self.hud_mode.label())),
                8 => Some(MenuRow::valued(
                    "SKILL",
                    SKILL_LABELS[if (self.skill as usize) < SKILL_LABELS.len() {
                        self.skill as usize
                    } else {
                        DEFAULT_SKILL as usize
                    }],
                )),
                OPTIONS_SOUND_VOLUME_ROW => Some(MenuRow::plain("SOUND VOLUME")),
                OPTIONS_MUSIC_ROW if self.music_available => Some(MenuRow::valued(
                    "MUSIC",
                    if self.music_on { "ON" } else { "OFF" },
                )),
                OPTIONS_MUSIC_VOLUME_ROW if self.music_available => {
                    Some(MenuRow::plain("MUSIC VOLUME"))
                }
                OPTIONS_TRACK_ROW if self.music_available => Some(MenuRow::valued(
                    "TRACK",
                    MUSIC_TRACKS[if (self.track as usize) < MUSIC_TRACKS.len() {
                        self.track as usize
                    } else {
                        0
                    }],
                )),
                _ if index == self.options_back_row() => Some(MenuRow::plain("BACK")),
                _ => None,
            },
            MenuPage::Controls => match index {
                0 => Some(MenuRow::plain("BACK")),
                _ => None,
            },
            MenuPage::Cheats => match index {
                0 => Some(MenuRow::valued(
                    "GOD MODE",
                    if self.god_mode { "ON" } else { "OFF" },
                )),
                1 => Some(MenuRow::valued(
                    "NOCLIP",
                    if self.noclip { "ON" } else { "OFF" },
                )),
                2 => Some(MenuRow::plain("IMPULSE 9")),
                3 => Some(MenuRow::plain("BACK")),
                _ => None,
            },
        }
    }
}

const LOOK_SPEED_LABELS: [&str; 5] = ["1", "2", "3", "4", "5"];
const LOOK_SPEED_SCALE: [i32; 5] = [2, 3, 4, 5, 6];

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Menu {
    active: bool,
    page: MenuPage,
    selected: u8,
    can_resume: bool,
    look_speed: u8,
    invert_y: bool,
    deadzone: u8,
    brightness: u8,
    crosshair: bool,
    water_warp: bool,
    water_alpha: bool,
    hud_mode: HudMode,
    skill: u8,
    sound_volume: u8,
    music_available: bool,
    music_on: bool,
    music_volume: u8,
    track: u8,
    god_mode: bool,
    noclip: bool,
    pending_sound: Option<MenuSound>,
}

impl Default for Menu {
    #[optimize(size)]
    fn default() -> Self {
        Self::new()
    }
}

impl Menu {
    #[optimize(size)]
    pub const fn new() -> Self {
        Self {
            active: true,
            page: MenuPage::Main,
            selected: 0,
            can_resume: false,
            look_speed: 2,
            invert_y: false,
            deadzone: DEFAULT_DEADZONE,
            brightness: DEFAULT_BRIGHTNESS,
            crosshair: true,
            water_warp: true,
            water_alpha: false,
            hud_mode: DEFAULT_HUD_MODE,
            skill: DEFAULT_SKILL,
            sound_volume: DEFAULT_SOUND_VOLUME,
            music_available: false,
            music_on: true,
            music_volume: DEFAULT_MUSIC_VOLUME,
            track: 0,
            god_mode: false,
            noclip: false,
            // `m_entersound` is raised when the original first enters a menu
            // and emitted after its first draw. The game consumes this once
            // the global bank is resident.
            pending_sound: Some(MenuSound::Enter),
        }
    }

    #[optimize(size)]
    pub const fn active(&self) -> bool {
        self.active
    }

    #[optimize(size)]
    pub const fn view(&self) -> MenuView {
        MenuView {
            page: self.page,
            selected: self.selected,
            can_resume: self.can_resume,
            look_speed: self.look_speed,
            invert_y: self.invert_y,
            deadzone: self.deadzone,
            brightness: self.brightness,
            crosshair: self.crosshair,
            water_warp: self.water_warp,
            water_alpha: self.water_alpha,
            hud_mode: self.hud_mode,
            skill: self.skill,
            sound_volume: self.sound_volume,
            music_available: self.music_available,
            music_on: self.music_on,
            music_volume: self.music_volume,
            track: self.track,
            god_mode: self.god_mode,
            noclip: self.noclip,
        }
    }

    /// Refresh the music rows from the drive. The player edits them, but the
    /// song advances on its own at the end of a track and at every level load,
    /// so the module that owns the drive re-states the truth each frame and the
    /// menu is only the surface. Losing the rows mid-page cannot strand the
    /// cursor past the last one.
    #[optimize(size)]
    pub fn sync_music(&mut self, available: bool, on: bool, track: u8) {
        self.music_available = available;
        self.music_on = on;
        self.track = track;
        let rows = self.view().row_count();
        if self.selected >= rows {
            self.selected = rows - 1;
        }
    }

    #[optimize(size)]
    pub fn open_pause(&mut self) {
        self.active = true;
        self.page = MenuPage::Main;
        self.selected = 0;
        self.can_resume = true;
        self.pending_sound = Some(MenuSound::Enter);
    }

    #[optimize(size)]
    pub fn close_for_game(&mut self) {
        self.active = false;
        self.page = MenuPage::Main;
        self.selected = 0;
        self.can_resume = true;
    }

    /// Take the strongest semantic sound raised by the most recent update.
    /// An action/adjustment overwrites a cursor move from the same pad poll.
    #[optimize(size)]
    pub fn take_sound(&mut self) -> Option<MenuSound> {
        self.pending_sound.take()
    }

    /// Apply menu sensitivity after the shared deadzone and aim curve.
    #[optimize(size)]
    pub fn apply_look_settings(&self, mut look: [i16; 2]) -> [i16; 2] {
        let scale = LOOK_SPEED_SCALE[self.look_speed.min(4) as usize];
        look[0] = ((i32::from(look[0]) * scale) / 4).clamp(-127, 127) as i16;
        look[1] = ((i32::from(look[1]) * scale) / 4).clamp(-127, 127) as i16;
        if self.invert_y {
            look[1] = look[1].saturating_neg();
        }
        look
    }

    /// Scaled-radial radius applied to both sticks before Quake's aim curve.
    #[optimize(size)]
    pub const fn deadzone_radius(&self) -> i16 {
        DEADZONE_RADII[if (self.deadzone as usize) < DEADZONE_RADII.len() {
            self.deadzone as usize
        } else {
            DEFAULT_DEADZONE as usize
        }]
    }

    #[optimize(size)]
    pub fn update(&mut self, input: MenuInput) -> Option<MenuAction> {
        if !self.active {
            return None;
        }

        let rows = self.view().row_count();
        if input.up {
            self.selected = if self.selected == 0 {
                rows - 1
            } else {
                self.selected - 1
            };
            self.pending_sound = Some(MenuSound::Move);
        }
        if input.down {
            self.selected = (self.selected + 1) % rows;
            self.pending_sound = Some(MenuSound::Move);
        }

        match self.page {
            MenuPage::Main => {
                if input.back && self.can_resume {
                    self.pending_sound = Some(MenuSound::Enter);
                    self.close_for_game();
                    return Some(MenuAction::Resume);
                }
                if !input.accept {
                    return None;
                }
                self.pending_sound = Some(MenuSound::Enter);
                let row = self.selected + u8::from(!self.can_resume);
                match row {
                    0 => {
                        self.close_for_game();
                        Some(MenuAction::Resume)
                    }
                    1 => {
                        self.close_for_game();
                        Some(MenuAction::NewGame)
                    }
                    2 => {
                        self.page = MenuPage::Levels;
                        self.selected = 0;
                        None
                    }
                    3 => {
                        self.page = MenuPage::Options;
                        self.selected = 0;
                        None
                    }
                    4 => {
                        self.page = MenuPage::Controls;
                        self.selected = 0;
                        None
                    }
                    _ => {
                        self.page = MenuPage::Cheats;
                        self.selected = 0;
                        None
                    }
                }
            }
            MenuPage::Levels => {
                if input.back || (input.accept && self.selected as usize == LEVEL_ROWS.len()) {
                    self.pending_sound = Some(MenuSound::Enter);
                    self.page = MenuPage::Main;
                    self.selected = 0;
                    return None;
                }
                if input.accept {
                    self.pending_sound = Some(MenuSound::Enter);
                    let level = self.selected;
                    self.close_for_game();
                    return Some(MenuAction::StartLevel(level));
                }
                None
            }
            MenuPage::Options => {
                if self.selected == 0 && (input.left || input.right) {
                    self.pending_sound = Some(MenuSound::Adjust);
                    if input.right {
                        self.look_speed = (self.look_speed + 1).min(4);
                    } else {
                        self.look_speed = self.look_speed.saturating_sub(1);
                    }
                } else if self.selected == 1 && (input.left || input.right || input.accept) {
                    self.pending_sound = Some(MenuSound::Adjust);
                    self.invert_y = !self.invert_y;
                } else if self.selected == 2 && (input.left || input.right) {
                    self.pending_sound = Some(MenuSound::Adjust);
                    if input.right {
                        self.deadzone = (self.deadzone + 1).min(DEADZONE_RADII.len() as u8 - 1);
                    } else {
                        self.deadzone = self.deadzone.saturating_sub(1);
                    }
                } else if self.selected == 3 && (input.left || input.right) {
                    self.pending_sound = Some(MenuSound::Adjust);
                    if input.right {
                        self.brightness = (self.brightness + 1).min(BRIGHTNESS_STEPS - 1);
                    } else {
                        self.brightness = self.brightness.saturating_sub(1);
                    }
                } else if self.selected == 4 && (input.left || input.right || input.accept) {
                    self.pending_sound = Some(MenuSound::Adjust);
                    self.crosshair = !self.crosshair;
                } else if self.selected == 5 && (input.left || input.right || input.accept) {
                    self.pending_sound = Some(MenuSound::Adjust);
                    self.water_warp = !self.water_warp;
                } else if self.selected == 6 && (input.left || input.right || input.accept) {
                    self.pending_sound = Some(MenuSound::Adjust);
                    self.water_alpha = !self.water_alpha;
                } else if self.selected == 7 && (input.left || input.right || input.accept) {
                    self.pending_sound = Some(MenuSound::Adjust);
                    self.hud_mode = match self.hud_mode {
                        HudMode::Minimal => HudMode::Classic,
                        HudMode::Classic => HudMode::Minimal,
                    };
                } else if self.selected == 8 && (input.left || input.right) {
                    self.pending_sound = Some(MenuSound::Adjust);
                    let last = SKILL_LABELS.len() as u8 - 1;
                    if input.right {
                        self.skill = (self.skill + 1).min(last);
                    } else {
                        self.skill = self.skill.saturating_sub(1);
                    }
                } else if self.selected == OPTIONS_SOUND_VOLUME_ROW && (input.left || input.right) {
                    self.pending_sound = Some(MenuSound::Adjust);
                    if input.right {
                        self.sound_volume = (self.sound_volume + 1).min(VOLUME_STEPS);
                    } else {
                        self.sound_volume = self.sound_volume.saturating_sub(1);
                    }
                } else if self.music_available
                    && self.selected == OPTIONS_MUSIC_ROW
                    && (input.left || input.right || input.accept)
                {
                    self.pending_sound = Some(MenuSound::Adjust);
                    self.music_on = !self.music_on;
                } else if self.music_available
                    && self.selected == OPTIONS_MUSIC_VOLUME_ROW
                    && (input.left || input.right)
                {
                    self.pending_sound = Some(MenuSound::Adjust);
                    if input.right {
                        self.music_volume = (self.music_volume + 1).min(VOLUME_STEPS);
                    } else {
                        self.music_volume = self.music_volume.saturating_sub(1);
                    }
                } else if self.music_available
                    && self.selected == OPTIONS_TRACK_ROW
                    && (input.left || input.right)
                {
                    self.pending_sound = Some(MenuSound::Adjust);
                    let step = if input.right {
                        1
                    } else {
                        MUSIC_TRACKS.len() - 1
                    };
                    self.track = ((self.track as usize + step) % MUSIC_TRACKS.len()) as u8;
                } else if (self.selected == self.view().options_back_row() && input.accept)
                    || input.back
                {
                    self.pending_sound = Some(MenuSound::Enter);
                    self.page = MenuPage::Main;
                    self.selected = 0;
                }
                None
            }
            MenuPage::Controls => {
                if input.accept || input.back {
                    self.pending_sound = Some(MenuSound::Enter);
                    self.page = MenuPage::Main;
                    self.selected = 0;
                }
                None
            }
            MenuPage::Cheats => {
                if self.selected == 0 && (input.left || input.right || input.accept) {
                    self.pending_sound = Some(MenuSound::Adjust);
                    self.god_mode = !self.god_mode;
                } else if self.selected == 1 && (input.left || input.right || input.accept) {
                    self.pending_sound = Some(MenuSound::Adjust);
                    self.noclip = !self.noclip;
                } else if self.selected == 2 && input.accept {
                    self.pending_sound = Some(MenuSound::Adjust);
                    return Some(MenuAction::Impulse9);
                } else if (self.selected == 3 && input.accept) || input.back {
                    self.pending_sound = Some(MenuSound::Enter);
                    self.page = MenuPage::Main;
                    self.selected = 0;
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[optimize(size)]
    fn accept() -> MenuInput {
        MenuInput {
            accept: true,
            ..MenuInput::default()
        }
    }

    #[optimize(size)]
    fn down() -> MenuInput {
        MenuInput {
            down: true,
            ..MenuInput::default()
        }
    }

    #[optimize(size)]
    #[test]
    fn levels_page_lists_every_level_and_starts_the_chosen_one() {
        let mut menu = Menu::new();
        assert_eq!(menu.update(down()), None);
        assert_eq!(menu.view().row(1), Some(MenuRow::plain("LEVELS")));
        assert_eq!(menu.update(accept()), None);
        assert_eq!(menu.view().title(), "LEVELS");
        assert_eq!(menu.view().row_count(), 10);
        assert_eq!(menu.view().row(0), Some(MenuRow::plain(LEVEL_ROWS[0])));
        assert_eq!(menu.view().row(9), Some(MenuRow::plain("BACK")));
        for _ in 0..3 {
            menu.update(down());
        }
        assert_eq!(menu.update(accept()), Some(MenuAction::StartLevel(3)));
        assert!(!menu.active());
        // Back row and the back button both return to the main menu.
        menu.open_pause();
        menu.update(down());
        menu.update(down());
        assert_eq!(menu.update(accept()), None);
        assert_eq!(menu.view().page, MenuPage::Levels);
        assert_eq!(
            menu.update(MenuInput {
                back: true,
                ..MenuInput::default()
            }),
            None
        );
        assert_eq!(menu.view().page, MenuPage::Main);
        assert_eq!(LEVEL_ROWS.len(), crate::level::LEVEL_NAMES.len());
    }

    #[optimize(size)]
    #[test]
    fn brightness_steps_through_the_palette_rows() {
        let mut menu = Menu::new();
        assert_eq!(menu.view().brightness, DEFAULT_BRIGHTNESS);
        menu.update(down());
        menu.update(down());
        menu.update(accept());
        assert_eq!(menu.view().page, MenuPage::Options);
        menu.update(down());
        menu.update(down());
        menu.update(down());
        assert_eq!(menu.view().row(3).map(|row| row.label), Some("GAMMA"));
        let right = MenuInput {
            right: true,
            ..MenuInput::default()
        };
        for _ in 0..10 {
            menu.update(right);
        }
        assert_eq!(menu.view().brightness, BRIGHTNESS_STEPS - 1);
        assert_eq!(menu.view().row(3).and_then(|row| row.value), Some("6"));
        let left = MenuInput {
            left: true,
            ..MenuInput::default()
        };
        for _ in 0..10 {
            menu.update(left);
        }
        assert_eq!(menu.view().brightness, 0);
        for _ in 0..7 {
            menu.update(down());
        }
        assert_eq!(menu.update(accept()), None);
        assert_eq!(menu.view().page, MenuPage::Main);
    }

    #[optimize(size)]
    #[test]
    fn deadzone_row_drives_the_shared_scaled_radial_radius() {
        let mut menu = Menu::new();
        menu.update(down());
        menu.update(down());
        menu.update(accept());
        menu.update(down());
        menu.update(down());
        assert_eq!(menu.deadzone_radius(), 28);
        assert_eq!(menu.view().row(2), Some(MenuRow::valued("DEADZONE", "28")));

        let right = MenuInput {
            right: true,
            ..MenuInput::default()
        };
        let left = MenuInput {
            left: true,
            ..MenuInput::default()
        };
        for _ in 0..10 {
            menu.update(right);
        }
        assert_eq!(menu.deadzone_radius(), 44);
        assert_eq!(menu.view().row(2), Some(MenuRow::valued("DEADZONE", "44")));
        for _ in 0..10 {
            menu.update(left);
        }
        assert_eq!(menu.deadzone_radius(), 12);
        assert_eq!(menu.view().row(2), Some(MenuRow::valued("DEADZONE", "12")));
    }

    #[optimize(size)]
    #[test]
    fn the_crosshair_and_skill_rows_hold_their_own_values() {
        let mut menu = Menu::new();
        menu.update(down());
        menu.update(down());
        menu.update(accept());
        assert_eq!(menu.view().page, MenuPage::Options);
        assert_eq!(menu.view().skill, DEFAULT_SKILL);

        for _ in 0..4 {
            menu.update(down());
        }
        assert_eq!(menu.view().row(4), Some(MenuRow::valued("CROSS", "ON")));
        menu.update(accept());
        assert!(!menu.view().crosshair);
        assert_eq!(menu.view().row(4), Some(MenuRow::valued("CROSS", "OFF")));

        let right = MenuInput {
            right: true,
            ..MenuInput::default()
        };
        let left = MenuInput {
            left: true,
            ..MenuInput::default()
        };
        menu.update(down());
        assert_eq!(menu.view().row(5), Some(MenuRow::valued("WARP", "ON")));
        menu.update(down());
        assert_eq!(
            menu.view().row(6),
            Some(MenuRow::valued("CLEAR WATER", "OFF"))
        );
        menu.update(down());
        assert_eq!(menu.view().row(7), Some(MenuRow::valued("HUD", "MINIMAL")));
        menu.update(down());
        for _ in 0..5 {
            menu.update(right);
        }
        assert_eq!(menu.view().skill, SKILL_LABELS.len() as u8 - 1);
        assert_eq!(
            menu.view().row(8),
            Some(MenuRow::valued("SKILL", "NIGHTMARE"))
        );
        for _ in 0..5 {
            menu.update(left);
        }
        assert_eq!(menu.view().skill, 0);
        assert_eq!(menu.view().row(8), Some(MenuRow::valued("SKILL", "EASY")));
    }

    #[optimize(size)]
    #[test]
    fn water_warp_row_defaults_on_and_can_be_disabled() {
        let mut menu = Menu::new();
        menu.update(down());
        menu.update(down());
        menu.update(accept());
        for _ in 0..5 {
            menu.update(down());
        }
        assert!(menu.view().water_warp);
        assert_eq!(menu.view().row(5), Some(MenuRow::valued("WARP", "ON")));
        menu.update(accept());
        assert!(!menu.view().water_warp);
        assert_eq!(menu.view().row(5), Some(MenuRow::valued("WARP", "OFF")));
    }

    #[optimize(size)]
    #[test]
    fn clear_water_defaults_off_and_can_be_enabled() {
        let mut menu = Menu::new();
        menu.update(down());
        menu.update(down());
        menu.update(accept());
        for _ in 0..6 {
            menu.update(down());
        }
        assert!(!menu.view().water_alpha);
        assert_eq!(
            menu.view().row(6),
            Some(MenuRow::valued("CLEAR WATER", "OFF"))
        );
        menu.update(accept());
        assert!(menu.view().water_alpha);
        assert_eq!(
            menu.view().row(6),
            Some(MenuRow::valued("CLEAR WATER", "ON"))
        );
    }

    #[optimize(size)]
    #[test]
    fn hud_row_switches_between_classic_and_minimal() {
        let mut menu = Menu::new();
        menu.update(down());
        menu.update(down());
        menu.update(accept());
        for _ in 0..7 {
            menu.update(down());
        }
        assert_eq!(menu.view().hud_mode, HudMode::Minimal);
        assert_eq!(menu.view().row(7), Some(MenuRow::valued("HUD", "MINIMAL")));
        menu.update(accept());
        assert_eq!(menu.view().hud_mode, HudMode::Classic);
        assert_eq!(menu.view().row(7), Some(MenuRow::valued("HUD", "CLASSIC")));
    }

    #[optimize(size)]
    #[test]
    fn music_rows_appear_only_when_the_disc_carries_the_tracks() {
        let mut menu = Menu::new();
        menu.update(down());
        menu.update(down());
        menu.update(accept());
        assert_eq!(menu.view().page, MenuPage::Options);
        // Silent disc: sound volume remains useful, BACK still last.
        assert_eq!(menu.view().row_count(), 11);
        assert_eq!(
            menu.view().row(OPTIONS_SOUND_VOLUME_ROW),
            Some(MenuRow::plain("SOUND VOLUME"))
        );
        assert_eq!(menu.view().row(10), Some(MenuRow::plain("BACK")));

        menu.sync_music(true, true, 0);
        assert_eq!(menu.view().row_count(), 14);
        assert_eq!(
            menu.view().row(OPTIONS_MUSIC_ROW),
            Some(MenuRow::valued("MUSIC", "ON"))
        );
        assert_eq!(
            menu.view().row(OPTIONS_MUSIC_VOLUME_ROW),
            Some(MenuRow::plain("MUSIC VOLUME"))
        );
        assert_eq!(
            menu.view().row(OPTIONS_TRACK_ROW),
            Some(MenuRow::valued("TRACK", MUSIC_TRACKS[0]))
        );
        assert_eq!(menu.view().row(13), Some(MenuRow::plain("BACK")));

        let left = MenuInput {
            left: true,
            ..MenuInput::default()
        };
        // MUSIC toggles off, TRACK wraps backwards to the last song.
        for _ in 0..OPTIONS_MUSIC_ROW {
            menu.update(down());
        }
        assert_eq!(menu.update(left), None);
        assert!(!menu.view().music_on);
        menu.update(down());
        assert_eq!(menu.view().selected, OPTIONS_MUSIC_VOLUME_ROW);
        menu.update(down());
        assert_eq!(menu.update(left), None);
        assert_eq!(menu.view().track, MUSIC_TRACKS.len() as u8 - 1);
        // BACK has moved down with the rows.
        menu.update(down());
        assert_eq!(menu.update(accept()), None);
        assert_eq!(menu.view().page, MenuPage::Main);
    }

    #[optimize(size)]
    #[test]
    fn losing_the_music_rows_cannot_strand_the_cursor() {
        let mut menu = Menu::new();
        menu.update(down());
        menu.update(down());
        menu.update(accept());
        menu.sync_music(true, true, 0);
        for _ in 0..13 {
            menu.update(down());
        }
        assert_eq!(menu.view().selected, 13);
        menu.sync_music(false, true, 0);
        assert_eq!(menu.view().selected, 10);
        assert_eq!(menu.view().row(10), Some(MenuRow::plain("BACK")));
    }

    #[optimize(size)]
    #[test]
    fn volume_sliders_match_original_defaults_and_clamp() {
        let mut menu = Menu::new();
        assert_eq!(menu.view().sound_volume, DEFAULT_SOUND_VOLUME);
        assert_eq!(menu.view().music_volume, DEFAULT_MUSIC_VOLUME);
        menu.update(down());
        menu.update(down());
        menu.update(accept());
        for _ in 0..OPTIONS_SOUND_VOLUME_ROW {
            menu.update(down());
        }
        let left = MenuInput {
            left: true,
            ..MenuInput::default()
        };
        let right = MenuInput {
            right: true,
            ..MenuInput::default()
        };
        for _ in 0..20 {
            menu.update(left);
        }
        assert_eq!(menu.view().sound_volume, 0);
        for _ in 0..20 {
            menu.update(right);
        }
        assert_eq!(menu.view().sound_volume, VOLUME_STEPS);

        menu.sync_music(true, true, 0);
        menu.update(down());
        menu.update(down());
        assert_eq!(menu.view().selected, OPTIONS_MUSIC_VOLUME_ROW);
        for _ in 0..20 {
            menu.update(left);
        }
        assert_eq!(menu.view().music_volume, 0);
    }

    #[optimize(size)]
    #[test]
    fn menu_actions_raise_the_original_three_sound_roles() {
        let mut menu = Menu::new();
        assert_eq!(menu.take_sound(), Some(MenuSound::Enter));
        menu.update(down());
        assert_eq!(menu.take_sound(), Some(MenuSound::Move));
        menu.update(down());
        menu.update(accept());
        assert_eq!(menu.take_sound(), Some(MenuSound::Enter));
        menu.update(MenuInput {
            right: true,
            ..MenuInput::default()
        });
        assert_eq!(menu.take_sound(), Some(MenuSound::Adjust));
        menu.update(MenuInput {
            back: true,
            ..MenuInput::default()
        });
        assert_eq!(menu.take_sound(), Some(MenuSound::Enter));
    }

    #[optimize(size)]
    #[test]
    fn boot_starts_new_game_and_pause_can_resume() {
        let mut menu = Menu::new();
        assert_eq!(menu.update(accept()), Some(MenuAction::NewGame));
        assert!(!menu.active());

        menu.open_pause();
        assert_eq!(menu.update(accept()), Some(MenuAction::Resume));
        assert!(!menu.active());
    }

    #[optimize(size)]
    #[test]
    fn pause_back_resumes_without_moving_the_selection() {
        let mut menu = Menu::new();
        menu.open_pause();
        assert_eq!(
            menu.update(MenuInput {
                back: true,
                ..MenuInput::default()
            }),
            Some(MenuAction::Resume)
        );
    }

    #[optimize(size)]
    #[test]
    fn options_adjust_post_curve_look_settings() {
        let mut menu = Menu::new();
        menu.update(down());
        menu.update(down());
        menu.update(accept());
        assert_eq!(menu.view().page, MenuPage::Options);
        menu.update(MenuInput {
            right: true,
            ..MenuInput::default()
        });
        menu.update(down());
        menu.update(accept());
        assert_eq!(menu.apply_look_settings([40, 40]), [50, -50]);
    }

    #[optimize(size)]
    #[test]
    fn controls_page_exposes_policy_and_returns_to_main() {
        let mut menu = Menu::new();
        menu.update(MenuInput {
            up: true,
            ..MenuInput::default()
        });
        menu.update(accept());
        assert_eq!(menu.view().page, MenuPage::Controls);
        assert_eq!(menu.view().title(), "CONTROLS");
        assert_eq!(CONTROL_LINES.len(), 7);
        assert_eq!(menu.view().row(0), Some(MenuRow::plain("BACK")));
        menu.update(accept());
        assert_eq!(menu.view().page, MenuPage::Main);
    }

    #[optimize(size)]
    #[test]
    fn presentation_rows_follow_boot_and_pause_policy() {
        let mut menu = Menu::new();
        assert_eq!(menu.view().row_count(), 4);
        assert_eq!(menu.view().row(0), Some(MenuRow::plain("NEW GAME")));
        menu.open_pause();
        assert_eq!(menu.view().row_count(), 6);
        assert_eq!(menu.view().row(0), Some(MenuRow::plain("RESUME")));
        assert_eq!(menu.view().row(5), Some(MenuRow::plain("CHEATS")));
    }

    #[optimize(size)]
    #[test]
    fn pause_cheats_toggle_and_emit_impulse_nine() {
        let mut menu = Menu::new();
        menu.open_pause();
        for _ in 0..5 {
            menu.update(down());
        }
        assert_eq!(menu.update(accept()), None);
        assert_eq!(menu.view().page, MenuPage::Cheats);
        assert_eq!(menu.view().row(0), Some(MenuRow::valued("GOD MODE", "OFF")));
        menu.update(accept());
        assert!(menu.view().god_mode);
        menu.update(down());
        menu.update(accept());
        assert!(menu.view().noclip);
        menu.update(down());
        assert_eq!(menu.update(accept()), Some(MenuAction::Impulse9));
        assert_eq!(menu.take_sound(), Some(MenuSound::Adjust));
        menu.update(down());
        menu.update(accept());
        assert_eq!(menu.view().page, MenuPage::Main);
    }
}
