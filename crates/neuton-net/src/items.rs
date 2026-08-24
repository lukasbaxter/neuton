//! Reading item stacks off the wire.
//!
//! A stack is a count, an item ID, and a list of data components. The catch is
//! that what follows a component ID depends entirely on which component it is:
//! there is no length prefix to skip past. Reading the slot after a sword with
//! an enchantment on it means knowing how an enchantment is written.
//!
//! This client knows the components it has met. When it meets one it does not,
//! it stops reading that packet rather than guessing -- packets are length
//! framed, so an abandoned read costs the rest of one packet and nothing else.
//! `unknown_components` names what was abandoned, so the list below can grow to
//! cover it.

use neuton_blocks::items::{component, item};
use neuton_protocol::buf::Reader;
use neuton_protocol::error::{Error, Result};

/// What is in one inventory slot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Stack {
    pub count: i32,
    /// Item protocol ID.
    pub id: i32,
    /// Registry name without the namespace, e.g. `diamond_pickaxe`.
    pub name: &'static str,
    /// The block this item places, if it places one.
    pub block_state: Option<u32>,
    /// Durability used, where the item carries it.
    pub damage: i32,
    /// Set for anything carrying enchantments, so it can be drawn with a sheen.
    pub enchanted: bool,
    /// A name the server gave this item, in place of the item's own.
    pub custom_name: Option<String>,
}

impl Stack {
    pub fn is_empty(&self) -> bool {
        self.count <= 0
    }
}

/// Reads one stack, or `None` for an empty slot.
pub fn read_stack(r: &mut Reader<'_>) -> Result<Option<Stack>> {
    let count = r.read_varint()?;
    if count <= 0 {
        return Ok(None);
    }
    let id = r.read_varint()?;
    let known = item(id);
    let mut stack = Stack {
        count,
        id,
        name: known.map_or("air", |i| i.name),
        block_state: known.and_then(|i| i.block_state),
        ..Default::default()
    };

    let added = r.read_varint()?;
    let removed = r.read_varint()?;
    for _ in 0..added.clamp(0, 512) {
        let kind = r.read_varint()?;
        read_component(r, kind, &mut stack)?;
    }
    for _ in 0..removed.clamp(0, 512) {
        r.read_varint()?;
    }
    Ok(Some(stack))
}

/// A component this client cannot read, by name, for reporting.
fn unreadable(kind: i32) -> Error {
    Error::UnknownComponent { name: component(kind).unwrap_or("out of range") }
}

/// Consumes one component, keeping the parts worth showing.
fn read_component(r: &mut Reader<'_>, kind: i32, stack: &mut Stack) -> Result<()> {
    let Some(name) = component(kind) else { return Err(unreadable(kind)) };
    match name {
        // Nothing on the wire at all.
        "unbreakable" | "creative_slot_lock" | "intangible_projectile" | "glider" => {}

        // A single VarInt.
        "max_stack_size" | "max_damage" | "repair_cost" | "rarity" | "map_id"
        | "ominous_bottle_amplifier" => {
            r.read_varint()?;
        }
        "damage" => stack.damage = r.read_varint()?,

        // A single boolean.
        "enchantment_glint_override" => {
            r.read_bool()?;
        }

        // A single fixed-width number.
        "dyed_color" | "map_color" | "base_color" => {
            r.read_i32()?;
        }

        // An identifier.
        "item_model" | "tooltip_style" | "instrument" | "jukebox_playable"
        | "note_block_sound" | "provides_banner_patterns" => {
            r.read_str()?;
        }

        // A text component, sent as network NBT.
        "custom_name" | "item_name" => {
            skip_nbt(r)?;
            // The text is a tree of styled parts; the flat name is enough for a
            // tooltip, and reading it properly belongs with the chat renderer.
            stack.custom_name = None;
        }
        "custom_data" => skip_nbt(r)?,
        "lore" => {
            let count = r.read_varint()?;
            for _ in 0..count.clamp(0, 256) {
                skip_nbt(r)?;
            }
        }

        // Enchantments: an ID and a level each.
        "enchantments" | "stored_enchantments" => {
            let count = r.read_varint()?;
            stack.enchanted |= count > 0;
            for _ in 0..count.clamp(0, 256) {
                r.read_varint()?;
                r.read_varint()?;
            }
        }

        // Which parts of the tooltip to hide: a flag, then a list of component
        // IDs.
        "tooltip_display" => {
            r.read_bool()?;
            let count = r.read_varint()?;
            for _ in 0..count.clamp(0, 256) {
                r.read_varint()?;
            }
        }

        // Food, and the two components that travel with it.
        "food" => {
            r.read_varint()?;
            r.read_f32()?;
            r.read_bool()?;
        }

        _ => return Err(unreadable(kind)),
    }
    Ok(())
}

fn skip_nbt(r: &mut Reader<'_>) -> Result<()> {
    let used = neuton_nbt::skip_network(r.rest()).map_err(|_| Error::BadNbt)?;
    r.read_bytes(used)?;
    Ok(())
}

/// Reads a run of stacks, stopping at the first one it cannot.
///
/// Returns what it managed, and why it stopped if it did. A partial inventory
/// shows the slots that were readable rather than nothing at all.
pub fn read_stacks(r: &mut Reader<'_>, count: usize) -> (Vec<Option<Stack>>, Option<String>) {
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        match read_stack(r) {
            Ok(slot) => out.push(slot),
            Err(e) => return (out, Some(e.to_string())),
        }
    }
    (out, None)
}
