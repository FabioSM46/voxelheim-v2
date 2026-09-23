//! How dark the first dungeon's lower zones are drawn.
//!
//! The dungeon's upper halls are lit the way the rest of the world is. Below them the
//! drawing is cut into rock: the dark cave, whose only light is the sconces on its walls,
//! and under it the buried-sand hall, dim and warmer. What the eye sees there is decided
//! here, as a [`Grade`] of the light [`super::sky::drive_the_sky`] already computes — the
//! ambient term, the sun and the colour distance fades into.
//!
//! **The sconces are not this module's.** They are static props the server places
//! (`world.InstanceStaticProps`), and `castle_lighting` gives the nearest of them shadowed
//! point lights from its bounded pool, exactly as it does the capital's candles. Turning
//! the ambient term down is what lets those lights be seen: at the surface's six hundred
//! they are a tint, and in the cave they are all there is.
//!
//! ## Where the zones are
//!
//! The server places the drawing with floor 1 on world y = 1 and rotates it about the
//! vertical by the instance's seed, so a zone's **height** is the same in every instance
//! while its horizontal extent is not. The bands below are therefore read on height
//! alone, and they are copies of the server's `schematic_instance.go` constants rather
//! than a second design:
//!
//! - the cave stands on the shore's level (`dungeonShore` = 18, world y = −32), with the
//!   pool chamber beside it;
//! - the sand hall stands on `dungeonSandFloor` = 10 (world y = −40) and is `sandTall` = 8
//!   courses tall, so its ceiling is the cave's floor course;
//! - the king's arena stands on `dungeonKingFloor` = 1 (world y = −49), under both, and is
//!   left lit as the halls are — its boss fight was tuned against that light.
//!
//! Only the eye's height is read. A band edge on a stair is where the grade starts to
//! move, and [`Grade::approach`] eases it over about a second so a flight of steps is a
//! dusk rather than a switch.
//!
//! ## Presentation only
//!
//! A colour computed here decides nothing: which creatures stir in the dark and where is
//! the server's, and no rule may be read back out of how bright the screen is.

use bevy::prelude::*;

/// World y of the dungeon's floor-1 floor course: everything below it is cut into rock.
/// Floor 1 stands on world y = 1, so the course under it is world y = 0.
const UPPER_FLOOR_COURSE: f32 = 0.0;
/// World y of the cave's floor course (`dungeonShore − 1` in the drawing), which is also
/// the sand hall's ceiling: an eye above it is in the cave or the pool chamber.
const CAVE_FLOOR_COURSE: f32 = -33.0;
/// World y of the sand hall's floor course (`dungeonSandFloor − 1`), which is the king's
/// arena's ceiling: an eye below it is in the arena.
const SAND_FLOOR_COURSE: f32 = -41.0;

/// How quickly a grade closes on the one the eye's zone asks for, per second. About a
/// second from one zone's light to the next's.
const EASE_PER_SECOND: f32 = 4.0;

/// Which of the dungeon's lit zones the eye is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Depth {
    /// Anywhere lit as the world is: the open world, the dungeon's upper halls, the
    /// king's arena.
    Open,
    /// The dark cave and the pool chamber on its level.
    Cave,
    /// The buried-sand hall.
    Sand,
}

/// Which zone an eye at `eye_y` is in, in the world `world_id` names.
///
/// World 0 is the open world, and nothing there is graded however deep a mine goes.
/// **An eye under water in the lower zones is in the cave**, whatever its height: the
/// only water down there is the pool chamber's, and its bed sits lower than the cave's
/// floor course.
pub(super) fn depth_at(world_id: u64, eye_y: Option<f32>, submerged: bool) -> Depth {
    let Some(y) = eye_y.filter(|y| y.is_finite()) else {
        return Depth::Open;
    };
    if world_id == 0 || !(SAND_FLOOR_COURSE..UPPER_FLOOR_COURSE).contains(&y) {
        return Depth::Open;
    }
    if submerged || y >= CAVE_FLOOR_COURSE {
        Depth::Cave
    } else {
        Depth::Sand
    }
}

/// How the light the sky computed is changed for the zone the eye is in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Grade {
    /// Multiplies the ambient term.
    pub ambient: f32,
    /// Multiplies the sun's illuminance. Zero underground: the rock above the lower
    /// zones is further than the sun's shadow cascades reach, so the sun would otherwise
    /// light a cave floor through sixty courses of stone.
    pub sun: f32,
    /// The ambient term's colour, linear RGB.
    pub tint: [f32; 3],
    /// How far the sky and the fog are drawn towards [`Self::murk`], from 0 to 1.
    pub shroud: f32,
    /// The colour distance fades into down here, linear RGB.
    pub murk: [f32; 3],
}

impl Grade {
    /// The world's own light, untouched.
    pub const OPEN: Self = Self {
        ambient: 1.0,
        sun: 1.0,
        tint: [1.0, 1.0, 1.0],
        shroud: 0.0,
        murk: [0.0, 0.0, 0.0],
    };

    /// The dark cave: a twentieth of the ambient term, cool, and fog that fades into
    /// near black a few blocks past the last sconce.
    pub const CAVE: Self = Self {
        ambient: 0.05,
        sun: 0.0,
        tint: [0.7, 0.8, 1.0],
        shroud: 0.97,
        murk: [0.002, 0.0025, 0.003],
    };

    /// The buried-sand hall: dim, and warm with the colour of the sand it is full of.
    pub const SAND: Self = Self {
        ambient: 0.2,
        sun: 0.0,
        tint: [1.0, 0.72, 0.42],
        shroud: 0.9,
        murk: [0.03, 0.017, 0.006],
    };

    /// The grade a zone asks for.
    pub const fn of(depth: Depth) -> Self {
        match depth {
            Depth::Open => Self::OPEN,
            Depth::Cave => Self::CAVE,
            Depth::Sand => Self::SAND,
        }
    }

    /// This grade moved towards `target` by `seconds` of easing. Snaps when it is within
    /// a hair, so an eye that has stopped in one zone settles on its grade exactly and
    /// the sky stops rewriting the light. A zero or negative step leaves it where it is,
    /// and a non-finite one snaps: there is no easing to do without a clock.
    pub fn approach(self, target: Self, seconds: f32) -> Self {
        if !seconds.is_finite() || seconds <= 0.0 {
            return if seconds.is_finite() { self } else { target };
        }
        let t = 1.0 - (-EASE_PER_SECOND * seconds).exp();
        let lerp = |from: f32, to: f32| from + (to - from) * t;
        let mix = |from: [f32; 3], to: [f32; 3]| std::array::from_fn(|i| lerp(from[i], to[i]));
        let next = Self {
            ambient: lerp(self.ambient, target.ambient),
            sun: lerp(self.sun, target.sun),
            tint: mix(self.tint, target.tint),
            shroud: lerp(self.shroud, target.shroud),
            murk: mix(self.murk, target.murk),
        };
        if next.distance(target) < 1e-3 {
            target
        } else {
            next
        }
    }

    /// The largest difference between any two of the two grades' numbers.
    fn distance(self, other: Self) -> f32 {
        let pairs = [
            (self.ambient, other.ambient),
            (self.sun, other.sun),
            (self.shroud, other.shroud),
        ]
        .into_iter()
        .chain((0..3).map(|i| (self.tint[i], other.tint[i])))
        .chain((0..3).map(|i| (self.murk[i], other.murk[i])));
        pairs.map(|(a, b)| (a - b).abs()).fold(0.0, f32::max)
    }

    /// The ambient term's colour.
    pub fn ambient_colour(self) -> Color {
        Color::linear_rgb(self.tint[0], self.tint[1], self.tint[2])
    }

    /// `colour` drawn towards this grade's murk by its shroud.
    pub fn shrouded(self, colour: Color) -> Color {
        if self.shroud <= 0.0 {
            return colour;
        }
        let linear = colour.to_linear();
        let mix = |from: f32, to: f32| from + (to - from) * self.shroud;
        Color::linear_rgba(
            mix(linear.red, self.murk[0]),
            mix(linear.green, self.murk[1]),
            mix(linear.blue, self.murk[2]),
            linear.alpha,
        )
    }
}

impl Default for Grade {
    fn default() -> Self {
        Self::OPEN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSTANCE: u64 = 3;

    #[test]
    fn the_open_world_is_never_graded_however_deep_the_eye_goes() {
        for y in [-100.0, -45.0, -36.0, -30.0, 0.5, 80.0] {
            assert_eq!(depth_at(0, Some(y), false), Depth::Open, "y {y}");
            assert_eq!(depth_at(0, Some(y), true), Depth::Open, "y {y}");
        }
    }

    #[test]
    fn the_dungeon_is_graded_by_the_height_of_its_zones() {
        // Standing eyes, a body's eye height over each zone's standing level.
        let eye = 1.6;
        assert_eq!(depth_at(INSTANCE, Some(1.0 + eye), false), Depth::Open);
        assert_eq!(depth_at(INSTANCE, Some(-32.0 + eye), false), Depth::Cave);
        assert_eq!(depth_at(INSTANCE, Some(-40.0 + eye), false), Depth::Sand);
        assert_eq!(depth_at(INSTANCE, Some(-49.0 + eye), false), Depth::Open);
        // The chasm down from floor 1 is rock on every side, and dark all the way.
        assert_eq!(depth_at(INSTANCE, Some(-10.0), false), Depth::Cave);
        // The pool: its surface is below the cave's floor course, and it is the cave's.
        assert_eq!(depth_at(INSTANCE, Some(-35.0), true), Depth::Cave);
        // No eye, or an eye at no height, is lit as the world is rather than guessed.
        assert_eq!(depth_at(INSTANCE, None, false), Depth::Open);
        assert_eq!(depth_at(INSTANCE, Some(f32::NAN), false), Depth::Open);
    }

    #[test]
    fn the_cave_is_darker_than_the_sand_and_the_sand_warmer_than_the_cave() {
        let (cave, sand) = (Grade::CAVE, Grade::SAND);
        assert!(cave.ambient < sand.ambient && sand.ambient < Grade::OPEN.ambient);
        assert_eq!(cave.sun, 0.0);
        assert_eq!(sand.sun, 0.0);
        let warmth = |tint: [f32; 3]| tint[0] - tint[2];
        assert!(warmth(sand.tint) > 0.0, "the sand hall is warm");
        assert!(warmth(cave.tint) < 0.0, "the cave is cold");
        // Fog in the cave fades into something darker than the sand hall's.
        let luminance = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        assert!(luminance(cave.murk) < luminance(sand.murk));
    }

    #[test]
    fn a_grade_eases_between_zones_and_settles_exactly() {
        let mut grade = Grade::OPEN;
        grade = grade.approach(Grade::CAVE, 1.0 / 60.0);
        assert!(grade.ambient < 1.0 && grade.ambient > Grade::CAVE.ambient);
        for _ in 0..600 {
            grade = grade.approach(Grade::CAVE, 1.0 / 60.0);
        }
        assert_eq!(grade, Grade::CAVE, "ten seconds in the cave is the cave");
        // A second is most of the way there, and not all of it.
        let mut stepped = Grade::OPEN;
        for _ in 0..60 {
            stepped = stepped.approach(Grade::SAND, 1.0 / 60.0);
        }
        let covered = (1.0 - stepped.ambient) / (1.0 - Grade::SAND.ambient);
        assert!(covered > 0.9 && covered < 1.0, "{covered}");
        // No clock is no easing: the target at once, and no motion on a zero step.
        assert_eq!(Grade::OPEN.approach(Grade::CAVE, f32::NAN), Grade::CAVE);
        assert_eq!(Grade::OPEN.approach(Grade::CAVE, 0.0), Grade::OPEN);
    }

    #[test]
    fn the_open_grade_changes_nothing_it_touches() {
        let sky = Color::srgb(0.4, 0.6, 0.9);
        assert_eq!(Grade::OPEN.shrouded(sky), sky);
        assert_eq!(
            Grade::OPEN.ambient_colour(),
            Color::linear_rgb(1.0, 1.0, 1.0)
        );
        let shrouded = Grade::CAVE.shrouded(sky).to_linear();
        assert!(shrouded.red < 0.02 && shrouded.green < 0.02 && shrouded.blue < 0.03);
    }
}
