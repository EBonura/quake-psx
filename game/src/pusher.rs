//! Atomic rider movement for Quake brush pushers.
//!
//! `SV_PushMove` either moves the pusher and every carried body or rolls the
//! entire step back. [`push_pass`] records candidate moves in a
//! [`PushLedger`]; the caller applies them only when no participant blocks.
//! Collision policy remains in [`quake_core::push`].

use quake_core::collision::TraceScratch;
use quake_core::movement::MovementTrace;
use quake_core::push::{penetrates, push_move, rests_on, PushOutcome, RiderBody};
use quake_formats::Vec3I32;

/// Live bodies besides the player that one pusher may carry in a single step.
///
/// Running out of slots blocks and rolls back the pusher step.
pub const MAX_CARRIED_BODIES: usize = 8;

/// The participant whose push could not be completed.
///
/// Quake passes that participant to the pusher's `blocked` function. Keeping
/// the identity here is important: a monster that wedges a lift must take the
/// crush damage itself, rather than fabricating damage against the player just
/// because the player is stored separately from the entity pool.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PushBlocker {
    Player,
    Body(u16),
    /// The fixed carry ledger could not represent another participant. The
    /// step still rolls back, but there is no participant we can safely name
    /// as the victim.
    Capacity,
}

/// The player body handed to the brush-mover pass.
///
/// Quake's pushers are not obstacles that happen to move: `SV_PushMove` carries
/// whatever is resting on the pusher and blocks when it cannot. That needs the
/// player's own origin inside the mover pass, not just the box it occupied when
/// the pass began, so the caller lends it here and takes back whatever the
/// pushers did to it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Rider {
    /// Q20.12 player origin, updated in place by every pusher that carries it.
    pub origin: Vec3I32,
    /// Absolute Q20.12 player box, kept in step with `origin`.
    pub mins: Vec3I32,
    pub maxs: Vec3I32,
    /// `FL_ONGROUND`. Only a grounded body can be carried by standing on a
    /// pusher; anything else has to be caught inside the pusher's final
    /// position instead.
    pub grounded: bool,
    /// True once a pusher moved this body, so the caller knows to write the
    /// new origin back into the locomotion state.
    pub carried: bool,
}

impl Rider {
    pub fn new(origin: Vec3I32, mins: Vec3I32, maxs: Vec3I32, grounded: bool) -> Self {
        Self {
            origin,
            mins,
            maxs,
            grounded,
            carried: false,
        }
    }

    /// This rider's absolute box, as the shared push policy wants it.
    pub const fn body(&self) -> RiderBody {
        RiderBody::new(self.origin, self.mins, self.maxs)
    }

    /// Move the whole body by `delta`. Only a committed ledger may call this.
    pub fn translate(&mut self, delta: Vec3I32) {
        self.origin = add(self.origin, delta);
        self.mins = add(self.mins, delta);
        self.maxs = add(self.maxs, delta);
        self.carried = true;
    }
}

/// Every body move one pusher step decided, and whether the step may keep them.
///
/// A ledger is written by [`push_pass`] and read by the caller afterwards. Its
/// whole purpose is the gap between those two: while it is being written no
/// body has moved, so a block discovered on the last body is exactly as
/// recoverable as a block discovered on the first.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PushLedger {
    player: Option<Vec3I32>,
    bodies: [(u16, Vec3I32); MAX_CARRIED_BODIES],
    count: usize,
    blocker: Option<PushBlocker>,
}

impl PushLedger {
    const EMPTY_BODY: (u16, Vec3I32) = (0, Vec3I32 { x: 0, y: 0, z: 0 });

    pub const fn new() -> Self {
        Self {
            player: None,
            bodies: [Self::EMPTY_BODY; MAX_CARRIED_BODIES],
            count: 0,
            blocker: None,
        }
    }

    /// Record what the pass decided for the player. Nothing moves yet.
    pub fn stage_player(&mut self, outcome: PushOutcome) {
        if outcome.carried {
            self.player = Some(outcome.origin);
        }
        if outcome.blocked && self.blocker.is_none() {
            self.blocker = Some(match outcome.blocking_body {
                Some(index) => PushBlocker::Body(index),
                None => PushBlocker::Player,
            });
        }
    }

    /// Record what the pass decided for one carried body. Nothing moves yet.
    ///
    /// A carry that does not fit the pool blocks the step. The alternative is
    /// to drop it, which reads as the pusher sliding out from under a body that
    /// was standing on it: the deck moves, the monster does not, and nothing
    /// anywhere reports that it happened. Blocking is the same answer the
    /// original gives to any body the pusher cannot take with it, and the
    /// caller already knows how to undo a blocked step completely.
    pub fn stage_body(&mut self, index: u16, outcome: PushOutcome) {
        if outcome.blocked && self.blocker.is_none() {
            self.blocker = Some(match outcome.blocking_body {
                Some(blocker) => PushBlocker::Body(blocker),
                None => PushBlocker::Body(index),
            });
        }
        if !outcome.carried {
            return;
        }
        if self.count == self.bodies.len() {
            if self.blocker.is_none() {
                self.blocker = Some(PushBlocker::Capacity);
            }
            return;
        }
        self.bodies[self.count] = (index, outcome.origin);
        self.count += 1;
    }

    /// The original's `block`: the caller must put the pusher back and run its
    /// `blocked` function.
    pub const fn blocked(&self) -> bool {
        self.blocker.is_some()
    }

    /// The participant passed to the original pusher's `blocked` function.
    pub const fn blocker(&self) -> Option<PushBlocker> {
        self.blocker
    }

    /// Where the player ends up, or `None` if it was not carried or the step
    /// was blocked.
    pub const fn player_move(&self) -> Option<Vec3I32> {
        if self.blocked() {
            return None;
        }
        self.player
    }

    /// Where each carried body ends up, empty when the step was blocked.
    pub fn body_moves(&self) -> &[(u16, Vec3I32)] {
        if self.blocked() {
            return &[];
        }
        &self.bodies[..self.count]
    }
}

impl Default for PushLedger {
    fn default() -> Self {
        Self::new()
    }
}

/// Run `SV_PushMove`'s rider half over the player and every candidate body.
///
/// `collision` must be the world with this pusher taken out of it, exactly as
/// [`quake_core::push::push_move`] requires. `delta` is the motion the pusher
/// already performed and `mover_mins`/`mover_maxs` are its absolute volume
/// after that motion. `rider_standing_on_pusher` is Quake's `FL_ONGROUND &&
/// groundentity == pusher` for the player; every other body gets the same
/// question answered geometrically by [`quake_core::push::rests_on`].
///
/// `bodies` is every live body that could be carried, already filtered for
/// aliveness and solidity by the caller. Bodies neither resting on nor driven
/// into the pusher are skipped here, so the caller does not need that policy.
///
/// Nothing is mutated. The returned ledger is the only output.
pub fn push_pass<C, I>(
    collision: &C,
    scratch: &mut TraceScratch,
    rider: &Rider,
    rider_standing_on_pusher: bool,
    bodies: I,
    delta: Vec3I32,
    mover_mins: Vec3I32,
    mover_maxs: Vec3I32,
) -> PushLedger
where
    C: MovementTrace + ?Sized,
    I: Iterator<Item = (u16, RiderBody)>,
{
    let mut ledger = PushLedger::new();
    ledger.stage_player(push_move(
        collision,
        scratch,
        rider.body(),
        rider_standing_on_pusher,
        delta,
        mover_mins,
        mover_maxs,
    ));
    for (index, body) in bodies {
        if !rests_on(body, mover_mins, mover_maxs) && !penetrates(body, mover_mins, mover_maxs) {
            continue;
        }
        ledger.stage_body(
            index,
            push_move(
                collision,
                scratch,
                body,
                rests_on(body, mover_mins, mover_maxs),
                delta,
                mover_mins,
                mover_maxs,
            ),
        );
    }
    ledger
}

const fn add(left: Vec3I32, right: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: left.x.saturating_add(right.x),
        y: left.y.saturating_add(right.y),
        z: left.z.saturating_add(right.z),
    }
}
