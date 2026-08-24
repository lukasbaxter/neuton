//! The player's items: what is in them, and how they are drawn.
//!
//! Slot numbers here are the container's own, because that is what the server
//! sends and what a click has to send back. For the player's own inventory that
//! means 0 is the crafting result, 5 to 8 are armour, 9 to 35 the three rows of
//! the backpack, 36 to 44 the hotbar and 45 the off hand.

use neuton_net::items::Stack;
use std::collections::HashMap;

pub const CRAFTING_OUTPUT: usize = 0;
pub const CRAFTING_GRID: std::ops::Range<usize> = 1..5;
pub const ARMOUR: std::ops::Range<usize> = 5..9;
pub const BACKPACK: std::ops::Range<usize> = 9..36;
pub const HOTBAR: std::ops::Range<usize> = 36..45;
pub const OFF_HAND: usize = 45;
pub const SLOTS: usize = 46;

/// What the player is carrying.
pub struct Inventory {
    slots: Vec<Option<Stack>>,
    /// Which of the nine hotbar slots is in hand, 0 to 8.
    pub selected: usize,
    /// Whether the inventory screen is up.
    pub open: bool,
}

impl Default for Inventory {
    fn default() -> Self {
        Self { slots: vec![None; SLOTS], selected: 0, open: false }
    }
}

impl Inventory {
    pub fn slot(&self, index: usize) -> Option<&Stack> {
        self.slots.get(index).and_then(|s| s.as_ref())
    }

    /// What is in the player's hand.
    pub fn held(&self) -> Option<&Stack> {
        self.slot(HOTBAR.start + self.selected)
    }

    /// Replaces everything, from a container packet.
    ///
    /// A short list is not an error: a stack this client cannot read yet stops
    /// the packet where it is, and the slots that did arrive are still worth
    /// showing.
    pub fn replace(&mut self, slots: Vec<Option<Stack>>) {
        for (index, stack) in slots.into_iter().enumerate() {
            if index < SLOTS {
                self.slots[index] = stack;
            }
        }
    }

    pub fn set(&mut self, index: i32, stack: Option<Stack>) {
        if let Ok(index) = usize::try_from(index)
            && index < SLOTS
        {
            self.slots[index] = stack;
        }
    }

    /// Moves the selection, wrapping the way the scroll wheel does.
    pub fn scroll(&mut self, by: i32) -> usize {
        let count = HOTBAR.len() as i32;
        self.selected = (self.selected as i32 - by).rem_euclid(count) as usize;
        self.selected
    }
}

/// Pictures for items and for the game's own interface sprites.
///
/// Both are drawn from the resource pack rather than reimplemented, so a pack
/// that restyles the hotbar restyles this one too. Icons are rendered the first
/// time an item is seen and kept, since an inventory holds few distinct items
/// and the hotbar redraws every frame.
pub struct ItemArt {
    packs: Option<neuton_assets::PackStack>,
    icons: neuton_assets::Icons,
    items: HashMap<i32, Option<egui::TextureHandle>>,
    sprites: HashMap<String, Option<egui::TextureHandle>>,
}

impl Default for ItemArt {
    fn default() -> Self {
        Self::new()
    }
}

impl ItemArt {
    pub fn new() -> Self {
        Self {
            packs: None,
            icons: neuton_assets::Icons::new(),
            items: HashMap::new(),
            sprites: HashMap::new(),
        }
    }

    fn packs(&mut self) -> Option<&mut neuton_assets::PackStack> {
        if self.packs.is_none() {
            self.packs = neuton_assets::PackStack::discover("26.2");
        }
        self.packs.as_mut()
    }

    /// The picture for one item, rendered on first sight.
    pub fn item(&mut self, ctx: &egui::Context, stack: &Stack) -> Option<egui::TextureId> {
        if let Some(cached) = self.items.get(&stack.id) {
            return cached.as_ref().map(|t| t.id());
        }
        let block = stack.block_state.map(|_| stack.name);
        let name = stack.name;
        if self.packs.is_none() {
            self.packs = neuton_assets::PackStack::discover("26.2");
        }
        let Self { packs, icons, .. } = self;
        let handle = packs
            .as_mut()
            .and_then(|packs| icons.render(packs, name, block))
            .map(|icon| {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [icon.size as usize, icon.size as usize],
                    &icon.pixels,
                );
                // Nearest neighbour: an item is pixel art and smoothing it is
                // the single most obvious way to look like something else.
                ctx.load_texture(format!("item:{name}"), image, nearest())
            });
        let id = handle.as_ref().map(|t| t.id());
        self.items.insert(stack.id, handle);
        id
    }

    /// One of the game's interface sprites, by its name under `gui/sprites`.
    pub fn sprite(&mut self, ctx: &egui::Context, name: &str) -> Option<(egui::TextureId, [f32; 2])> {
        if !self.sprites.contains_key(name) {
            let path = format!("assets/minecraft/textures/gui/sprites/{name}.png");
            let handle = self
                .packs()
                .and_then(|packs| packs.read(&path))
                .and_then(|bytes| crate::icons::decode(&bytes))
                .map(|image| ctx.load_texture(format!("gui:{name}"), image, nearest()));
            self.sprites.insert(name.to_string(), handle);
        }
        self.sprites.get(name).and_then(|h| h.as_ref()).map(|h| {
            let size = h.size();
            (h.id(), [size[0] as f32, size[1] as f32])
        })
    }
}

fn nearest() -> egui::TextureOptions {
    egui::TextureOptions {
        magnification: egui::TextureFilter::Nearest,
        minification: egui::TextureFilter::Linear,
        ..Default::default()
    }
}

/// How big one of the game's interface pixels should be, for a screen this
/// size.
///
/// The game picks the largest whole number that still leaves 320 by 240 of its
/// own pixels to lay out in, and so does this: a hotbar that is a fraction of a
/// pixel off its grid looks wrong in a way that is hard to name.
pub fn interface_scale(screen: egui::Vec2) -> f32 {
    let fit = (screen.x / 320.0).min(screen.y / 240.0);
    fit.floor().clamp(1.0, 4.0)
}

/// The hotbar, along the bottom of the screen.
pub fn hotbar(ui: &egui::Ui, inventory: &Inventory, art: &mut ItemArt, scale: f32) {
    let screen = ui.clip_rect();
    let ctx = ui.ctx().clone();
    let painter = ui.painter();

    // The bar is 182 by 22 in the game's own pixels, and everything on it is
    // placed against that grid rather than against the window.
    let (bar_w, bar_h) = (182.0 * scale, 22.0 * scale);
    let left = (screen.width() - bar_w) / 2.0 + screen.left();
    let top = screen.bottom() - bar_h - 4.0 * scale;
    let bar = egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(bar_w, bar_h));

    match art.sprite(&ctx, "hud/hotbar") {
        Some((id, _)) => {
            painter.image(id, bar, full_uv(), egui::Color32::WHITE);
        }
        None => {
            // Without the pack, something the same shape rather than nothing.
            painter.rect_filled(bar, 2.0, egui::Color32::from_black_alpha(160));
        }
    }

    for (index, slot) in HOTBAR.enumerate() {
        let x = left + (3.0 + index as f32 * 20.0) * scale;
        let y = top + 3.0 * scale;
        let cell = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(16.0 * scale, 16.0 * scale));
        draw_slot(ui, painter, art, inventory.slot(slot), cell, scale);
    }

    // The selection frame is drawn over the items, one game pixel outside the
    // slot on every side.
    let x = left + (inventory.selected as f32 * 20.0 - 1.0) * scale;
    let selection = egui::Rect::from_min_size(
        egui::pos2(x, top - 1.0 * scale),
        egui::vec2(24.0 * scale, 24.0 * scale),
    );
    match art.sprite(&ctx, "hud/hotbar_selection") {
        Some((id, _)) => {
            painter.image(id, selection, full_uv(), egui::Color32::WHITE);
        }
        None => {
            painter.rect_stroke(
                selection,
                0.0,
                egui::Stroke::new(scale.max(1.0), egui::Color32::WHITE),
                egui::StrokeKind::Inside,
            );
        }
    }

    // What is in hand, named above the bar, as the game does when it changes.
    if let Some(stack) = inventory.held() {
        let label = stack.custom_name.clone().unwrap_or_else(|| pretty(stack.name));
        painter.text(
            egui::pos2(bar.center().x, top - 6.0 * scale),
            egui::Align2::CENTER_BOTTOM,
            label,
            egui::FontId::proportional(9.0 * scale),
            egui::Color32::from_white_alpha(220),
        );
    }
}

/// One slot's contents: the picture, the count, and a durability bar.
fn draw_slot(
    ui: &egui::Ui,
    painter: &egui::Painter,
    art: &mut ItemArt,
    stack: Option<&Stack>,
    cell: egui::Rect,
    scale: f32,
) {
    let Some(stack) = stack else { return };
    if stack.is_empty() {
        return;
    }
    let ctx = ui.ctx().clone();
    match art.item(&ctx, stack) {
        Some(id) => {
            painter.image(id, cell, full_uv(), egui::Color32::WHITE);
        }
        None => {
            // An item with no picture still has to be visible, or a slot that
            // is full looks empty.
            painter.rect_filled(cell.shrink(2.0 * scale), 1.0, egui::Color32::from_gray(140));
        }
    }

    if stack.count > 1 {
        let at = egui::pos2(cell.right() + scale, cell.bottom() + scale);
        let font = egui::FontId::proportional(9.0 * scale);
        // The game draws item counts with a hard black shadow one pixel down
        // and right, and it is a surprising amount of why they read clearly.
        painter.text(
            at + egui::vec2(scale, scale),
            egui::Align2::RIGHT_BOTTOM,
            stack.count.to_string(),
            font.clone(),
            egui::Color32::from_rgb(0x3E, 0x3E, 0x3E),
        );
        painter.text(
            at,
            egui::Align2::RIGHT_BOTTOM,
            stack.count.to_string(),
            font,
            egui::Color32::WHITE,
        );
    }
}

fn full_uv() -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
}

/// `diamond_pickaxe` -> `Diamond Pickaxe`.
fn pretty(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for word in name.split('_') {
        if !out.is_empty() {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_read_as_names() {
        assert_eq!(pretty("diamond_pickaxe"), "Diamond Pickaxe");
        assert_eq!(pretty("stone"), "Stone");
    }

    #[test]
    fn the_hotbar_wraps_both_ways() {
        let mut inventory = Inventory::default();
        assert_eq!(inventory.scroll(1), 8, "scrolling up from the first wraps to the last");
        assert_eq!(inventory.scroll(-1), 0);
        inventory.selected = 8;
        assert_eq!(inventory.scroll(-1), 0, "and past the last comes back to the first");
    }

    #[test]
    fn a_short_container_packet_fills_what_it_can() {
        let mut inventory = Inventory::default();
        let stack = Stack { count: 3, id: 1, name: "stone", ..Default::default() };
        inventory.replace(vec![None, Some(stack.clone())]);
        assert_eq!(inventory.slot(1), Some(&stack));
        assert_eq!(inventory.slot(40), None, "slots the packet never reached stay empty");
    }
}
