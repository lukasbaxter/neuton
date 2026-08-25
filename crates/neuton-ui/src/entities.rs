//! Everything in the world that is not the player and is not a block.
//!
//! The server sends most movement as a small step from wherever the entity
//! already was, twenty times a second. Drawing those steps directly is visibly
//! steppy, so each entity remembers where it was as well as where it is and
//! the drawing sits between the two.

use neuton_world::entities::EntityKind;
use std::collections::HashMap;

pub struct Entity {
    pub kind: &'static EntityKind,
    pub uuid: u128,
    pub position: [f64; 3],
    /// Where it was before the last update, to draw between.
    pub previous: [f64; 3],
    /// Which way the body faces, and where the head looks.
    pub yaw: f32,
    pub pitch: f32,
    pub head_yaw: f32,
    pub velocity: [f64; 3],
}

impl Entity {
    /// Where to draw it, part way between the last two positions.
    pub fn drawn_at(&self, alpha: f32) -> [f64; 3] {
        let t = f64::from(alpha.clamp(0.0, 1.0));
        [
            self.previous[0] + (self.position[0] - self.previous[0]) * t,
            self.previous[1] + (self.position[1] - self.previous[1]) * t,
            self.previous[2] + (self.position[2] - self.previous[2]) * t,
        ]
    }

    /// The box it stands in, centred on its feet position.
    pub fn hitbox(&self, alpha: f32) -> ([f32; 3], [f32; 3]) {
        let at = self.drawn_at(alpha);
        let half = f64::from(self.kind.width) / 2.0;
        (
            [(at[0] - half) as f32, at[1] as f32, (at[2] - half) as f32],
            [
                (at[0] + half) as f32,
                (at[1] + f64::from(self.kind.height)) as f32,
                (at[2] + half) as f32,
            ],
        )
    }
}

/// Every entity the server has told us about, by its id.
#[derive(Default)]
pub struct Entities {
    by_id: HashMap<i32, Entity>,
}

impl Entities {
    #[allow(clippy::too_many_arguments)]
    pub fn add(
        &mut self,
        id: i32,
        uuid: u128,
        kind: i32,
        position: [f64; 3],
        yaw: f32,
        pitch: f32,
        head_yaw: f32,
        velocity: [f64; 3],
    ) {
        // An entity this build has no name for is still an entity, and drawing
        // it as a player-sized box beats pretending it is not there.
        let kind = neuton_world::entities::kind(kind)
            .unwrap_or(&neuton_world::entities::ENTITY_KINDS[0]);
        self.by_id.insert(
            id,
            Entity {
                kind,
                uuid,
                position,
                previous: position,
                yaw,
                pitch,
                head_yaw,
                velocity,
            },
        );
    }

    pub fn remove(&mut self, ids: &[i32]) {
        for id in ids {
            self.by_id.remove(id);
        }
    }

    /// A small step from wherever it already was.
    pub fn moved(&mut self, id: i32, delta: [f64; 3], rotation: Option<(f32, f32)>) {
        let Some(entity) = self.by_id.get_mut(&id) else { return };
        entity.previous = entity.position;
        for (axis, step) in entity.position.iter_mut().zip(delta) {
            *axis += step;
        }
        if let Some((yaw, pitch)) = rotation {
            entity.yaw = yaw;
            entity.pitch = pitch;
        }
    }

    pub fn teleported(
        &mut self,
        id: i32,
        position: [f64; 3],
        yaw: f32,
        pitch: f32,
        velocity: [f64; 3],
    ) {
        let Some(entity) = self.by_id.get_mut(&id) else { return };
        // A teleport is a jump rather than a step, so there is nothing sensible
        // to draw between: both ends are the destination.
        entity.previous = position;
        entity.position = position;
        entity.yaw = yaw;
        entity.pitch = pitch;
        entity.velocity = velocity;
    }

    pub fn head_yaw(&mut self, id: i32, yaw: f32) {
        if let Some(entity) = self.by_id.get_mut(&id) {
            entity.head_yaw = yaw;
        }
    }

    /// Everything gone, which is what a world change means.
    pub fn clear(&mut self) {
        self.by_id.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entity> {
        self.by_id.values()
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// How many of each named type there are, for the debug overlay.
    pub fn count_of(&self, name: &str) -> usize {
        self.by_id.values().filter(|e| e.kind.name == name).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Entities {
        let mut all = Entities::default();
        // 156 is the player type in registry order.
        all.add(7, 0, 156, [10.0, 64.0, 20.0], 0.0, 0.0, 0.0, [0.0; 3]);
        all
    }

    #[test]
    fn a_step_moves_from_where_it_was() {
        let mut all = store();
        all.moved(7, [0.5, 0.0, -0.25], None);
        let entity = all.iter().next().unwrap();
        assert_eq!(entity.position, [10.5, 64.0, 19.75]);
        assert_eq!(entity.previous, [10.0, 64.0, 20.0]);
        // Half way between is half the step.
        assert_eq!(entity.drawn_at(0.5), [10.25, 64.0, 19.875]);
    }

    #[test]
    fn a_teleport_has_nothing_to_draw_between() {
        let mut all = store();
        all.teleported(7, [80.0, 70.0, 90.0], 90.0, 0.0, [0.0; 3]);
        let entity = all.iter().next().unwrap();
        assert_eq!(entity.drawn_at(0.0), [80.0, 70.0, 90.0]);
        assert_eq!(entity.drawn_at(1.0), [80.0, 70.0, 90.0]);
    }

    #[test]
    fn a_player_box_is_the_players_own_size() {
        let all = store();
        let (min, max) = all.iter().next().unwrap().hitbox(1.0);
        assert_eq!(min, [9.7, 64.0, 19.7]);
        assert_eq!(max, [10.3, 65.8, 20.3]);
    }

    #[test]
    fn an_update_for_something_gone_is_not_a_panic() {
        let mut all = Entities::default();
        all.moved(999, [1.0, 0.0, 0.0], None);
        all.head_yaw(999, 90.0);
        all.teleported(999, [0.0; 3], 0.0, 0.0, [0.0; 3]);
        assert!(all.is_empty());
    }

    #[test]
    fn removing_takes_them_away() {
        let mut all = store();
        all.remove(&[7]);
        assert!(all.is_empty());
    }
}
