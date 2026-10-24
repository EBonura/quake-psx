//! The guest's journey state block (`game/src/journey.rs`), read from RAM.

pub const WORDS: usize = 1151;

#[derive(Clone, Debug)]
pub struct Monster {
    pub index: u16,
    pub kind: u8,
    pub state: u8,
    pub flags: u8,
    pub health: i32,
    pub origin: [i32; 3],
}

impl Monster {
    pub fn alive(&self) -> bool {
        self.flags & 2 == 0 && self.health > 0
    }
    pub fn active(&self) -> bool {
        self.flags & 1 != 0
    }
}

#[derive(Clone, Debug)]
pub struct Pickup {
    pub index: u16,
    pub code: i32,
    pub origin: [i32; 3],
}

#[derive(Clone, Debug)]
pub struct Prop {
    pub index: u16,
    pub health: i32,
    pub origin: [i32; 3],
}

#[derive(Clone, Debug)]
pub struct Mover {
    pub index: u16,
    /// 0 Bottom, 1 Top, 2 Up, 3 Down.
    pub state: u8,
    pub class: u8,
    pub origin: [i32; 3],
}

#[derive(Clone, Debug)]
pub struct Obs {
    pub frame: u32,
    pub map: u32,
    pub mode: u32,
    pub skill: u32,
    pub runes: u32,
    /// Q12 world units.
    pub origin: [i32; 3],
    pub velocity: [i32; 3],
    /// Q12 turns: pitch, yaw, roll.
    pub angles: [i32; 3],
    pub grounded: bool,
    pub water: u32,
    pub health: i32,
    pub armor: u32,
    pub armor_tier: u32,
    pub ammo: [u32; 4],
    pub weapons: u32,
    pub weapon: u32,
    pub keys: u32,
    pub powerups: u32,
    pub kills: u32,
    pub kills_total: u32,
    pub secrets: u32,
    pub secrets_total: u32,
    pub intermission_elapsed: u32,
    pub monsters: Vec<Monster>,
    pub pickups: Vec<Pickup>,
    pub movers: Vec<Mover>,
    pub props: Vec<Prop>,
}

impl Obs {
    pub fn parse(bytes: &[u8]) -> Result<Obs, String> {
        let w = |i: usize| -> u32 { u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap()) };
        let s = |i: usize| -> i32 { w(i) as i32 };
        if w(0) != 0x4e52_4a51 {
            return Err(format!("bad journey magic {:#x}", w(0)));
        }
        let mut monsters = Vec::new();
        for i in 0..w(35) as usize {
            let b = 38 + i * 6;
            let ks = w(b + 1);
            monsters.push(Monster {
                index: w(b) as u16,
                kind: (ks >> 16) as u8,
                state: (ks >> 8) as u8,
                flags: ks as u8,
                health: s(b + 2),
                origin: [s(b + 3), s(b + 4), s(b + 5)],
            });
        }
        let mut pickups = Vec::new();
        for i in 0..w(36) as usize {
            let b = 326 + i * 5;
            pickups.push(Pickup { index: w(b) as u16, code: s(b + 1), origin: [s(b + 2), s(b + 3), s(b + 4)] });
        }
        let mut movers = Vec::new();
        for i in 0..w(37) as usize {
            let b = 646 + i * 6;
            movers.push(Mover { index: w(b) as u16, state: w(b + 1) as u8, class: w(b + 2) as u8, origin: [s(b + 3), s(b + 4), s(b + 5)] });
        }
        let mut props = Vec::new();
        for i in 0..w(1030) as usize {
            let b = 1031 + i * 5;
            props.push(Prop { index: w(b) as u16, health: s(b + 1), origin: [s(b + 2), s(b + 3), s(b + 4)] });
        }
        Ok(Obs {
            frame: w(3),
            map: w(4),
            mode: w(5),
            skill: w(6),
            runes: w(7),
            origin: [s(8), s(9), s(10)],
            velocity: [s(11), s(12), s(13)],
            angles: [s(14), s(15), s(16)],
            grounded: w(17) != 0,
            water: w(18),
            health: s(19),
            armor: w(20),
            armor_tier: w(21),
            ammo: [w(22), w(23), w(24), w(25)],
            weapons: w(26),
            weapon: w(27),
            keys: w(28),
            powerups: w(29),
            kills: w(30),
            kills_total: w(31),
            secrets: w(32),
            secrets_total: w(33),
            intermission_elapsed: w(34),
            monsters,
            pickups,
            movers,
            props,
        })
    }

    /// Whole world units.
    pub fn pos(&self) -> [i32; 3] {
        [self.origin[0] >> 12, self.origin[1] >> 12, self.origin[2] >> 12]
    }
}
