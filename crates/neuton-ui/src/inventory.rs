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
    /// What is being dragged on the cursor.
    carried: Option<Stack>,
    /// The server's revision of this container, echoed back with every click.
    pub state_id: i32,
}

impl Default for Inventory {
    fn default() -> Self {
        Self {
            slots: vec![None; SLOTS],
            selected: 0,
            open: false,
            carried: None,
            state_id: 0,
        }
    }
}

impl Inventory {
    pub fn slot(&self, index: usize) -> Option<&Stack> {
        self.slots.get(index).and_then(|s| s.as_ref())
    }

    /// What is on the cursor, mid-drag.
    pub fn carried(&self) -> Option<&Stack> {
        self.carried.as_ref()
    }

    pub fn set_carried(&mut self, stack: Option<Stack>) {
        self.carried = stack;
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

    pub fn packs(&mut self) -> Option<&mut neuton_assets::PackStack> {
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

    /// One of the game's interface pictures, by its path under `textures/gui`.
    pub fn sprite(&mut self, ctx: &egui::Context, name: &str) -> Option<(egui::TextureId, [f32; 2])> {
        if !self.sprites.contains_key(name) {
            let path = format!("assets/minecraft/textures/gui/{name}.png");
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

    match art.sprite(&ctx, "sprites/hud/hotbar") {
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
    match art.sprite(&ctx, "sprites/hud/hotbar_selection") {
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

/// What the player is holding, in the corner of the screen.
///
/// The game builds this from the item's model in a little scene of its own.
/// This draws the same picture the inventory uses, which for a block is already
/// that block seen from a corner, scaled up and sat where a hand would be. It
/// bobs with the walk, because a held item that never moves reads as painted on
/// the screen rather than carried.
pub fn held_item(
    ui: &egui::Ui,
    inventory: &Inventory,
    art: &mut ItemArt,
    scale: f32,
    bob: f32,
) {
    let Some(stack) = inventory.held() else { return };
    if stack.is_empty() {
        return;
    }
    let ctx = ui.ctx().clone();
    let Some(id) = art.item(&ctx, stack) else { return };

    let screen = ui.clip_rect();
    let size = 56.0 * scale;
    // Sat in from the corner, and low enough that most of it is on screen but
    // its bottom edge is not.
    let centre = egui::pos2(
        screen.right() - size * 0.72 + bob.cos() * 3.0 * scale,
        screen.bottom() - size * 0.42 + bob.sin() * 4.0 * scale,
    );
    let rect = egui::Rect::from_center_size(centre, egui::vec2(size, size));
    ui.painter().image(id, rect, full_uv(), egui::Color32::WHITE);
}

/// Hearts and hunger, above the hotbar, drawn from the pack's own sprites.
///
/// Twenty points is ten icons, and a point is half an icon, which is why a
/// player at nineteen health shows nine and a half hearts rather than a bar
/// that is ninety five percent full.
pub fn vitals(ui: &egui::Ui, art: &mut ItemArt, scale: f32, health: f32, food: i32) {
    let screen = ui.clip_rect();
    let ctx = ui.ctx().clone();
    let painter = ui.painter();

    let bar_w = 182.0 * scale;
    let left = (screen.width() - bar_w) / 2.0 + screen.left();
    let top = screen.bottom() - 22.0 * scale - 4.0 * scale - 10.0 * scale;
    let icon = 9.0 * scale;

    let mut row = |x: f32, points: f32, full: &str, half: &str, empty: &str, rightwards: bool| {
        for i in 0..10 {
            let slot = if rightwards { i } else { 9 - i };
            let cell = egui::Rect::from_min_size(
                egui::pos2(x + slot as f32 * 8.0 * scale, top),
                egui::vec2(icon, icon),
            );
            let filled = points - i as f32 * 2.0;
            let sprite = if filled >= 2.0 {
                full
            } else if filled >= 1.0 {
                half
            } else {
                empty
            };
            // The empty icon goes down first, so a half icon has a socket
            // behind it rather than a hole.
            for name in [empty, sprite] {
                if let Some((id, _)) = art.sprite(&ctx, name) {
                    painter.image(id, cell, full_uv(), egui::Color32::WHITE);
                }
                if name == sprite {
                    break;
                }
            }
        }
    };

    row(left, health, "sprites/hud/heart/full", "sprites/hud/heart/half", "sprites/hud/heart/container", true);
    row(
        left + bar_w - 9.0 * 8.0 * scale - icon,
        food as f32,
        "sprites/hud/food_full",
        "sprites/hud/food_half",
        "sprites/hud/food_empty",
        false,
    );
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
        count_label(painter, cell, stack.count, scale);
    }
}

/// The stack size, in the corner of a slot.
fn count_label(painter: &egui::Painter, cell: egui::Rect, count: i32, scale: f32) {
    let at = egui::pos2(cell.right() + scale, cell.bottom() + scale);
    let font = egui::FontId::proportional(9.0 * scale);
    // The game draws item counts with a hard black shadow one pixel down and
    // right, and it is a surprising amount of why they read clearly.
    painter.text(
        at + egui::vec2(scale, scale),
        egui::Align2::RIGHT_BOTTOM,
        count.to_string(),
        font.clone(),
        egui::Color32::from_rgb(0x3E, 0x3E, 0x3E),
    );
    painter.text(at, egui::Align2::RIGHT_BOTTOM, count.to_string(), font, egui::Color32::WHITE);
}

/// Where each slot sits on the inventory screen, in the game's own pixels
/// against the 176 by 166 panel.
///
/// Indices are the container's, which is why they run in this order: the
/// crafting result, the grid, armour, the backpack, the hotbar, the off hand.
fn slot_positions() -> Vec<(usize, [f32; 2])> {
    let mut out = Vec::with_capacity(SLOTS);
    out.push((CRAFTING_OUTPUT, [154.0, 28.0]));
    for i in CRAFTING_GRID {
        let n = i - CRAFTING_GRID.start;
        out.push((i, [98.0 + (n % 2) as f32 * 18.0, 18.0 + (n / 2) as f32 * 18.0]));
    }
    for i in ARMOUR {
        out.push((i, [8.0, 8.0 + (i - ARMOUR.start) as f32 * 18.0]));
    }
    for i in BACKPACK {
        let n = i - BACKPACK.start;
        out.push((i, [8.0 + (n % 9) as f32 * 18.0, 84.0 + (n / 9) as f32 * 18.0]));
    }
    for i in HOTBAR {
        out.push((i, [8.0 + (i - HOTBAR.start) as f32 * 18.0, 142.0]));
    }
    out.push((OFF_HAND, [77.0, 62.0]));
    out
}

/// The whole inventory screen. Returns the slot that was clicked, if one was.
pub fn screen(
    ui: &egui::Ui,
    inventory: &Inventory,
    art: &mut ItemArt,
    scale: f32,
) -> Option<(usize, bool)> {
    let screen = ui.clip_rect();
    let ctx = ui.ctx().clone();
    let painter = ui.painter();
    painter.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(140));

    // The panel is 176 by 166 of the game's pixels, and every slot is placed
    // against that grid rather than against the window.
    let panel = egui::Rect::from_center_size(
        screen.center(),
        egui::vec2(176.0 * scale, 166.0 * scale),
    );
    match art.sprite(&ctx, "container/inventory") {
        Some((id, size)) => {
            // The texture is a 256 square with the panel in its top left.
            let uv = egui::Rect::from_min_max(
                egui::pos2(0.0, 0.0),
                egui::pos2(176.0 / size[0], 166.0 / size[1]),
            );
            painter.image(id, panel, uv, egui::Color32::WHITE);
        }
        None => {
            painter.rect_filled(panel, 4.0, egui::Color32::from_gray(198));
        }
    }

    let pointer = ui.ctx().pointer_latest_pos();
    let mut hit = None;
    for (index, at) in slot_positions() {
        let cell = egui::Rect::from_min_size(
            panel.min + egui::vec2(at[0] * scale, at[1] * scale),
            egui::vec2(16.0 * scale, 16.0 * scale),
        );
        // The slot's own square is a pixel out from the item on every side.
        let square = cell.expand(scale);
        if pointer.is_some_and(|p| square.contains(p)) {
            painter.rect_filled(square, 0.0, egui::Color32::from_white_alpha(90));
            if ui.ctx().input(|i| i.pointer.primary_pressed()) {
                hit = Some((index, false));
            } else if ui.ctx().input(|i| i.pointer.secondary_pressed()) {
                hit = Some((index, true));
            }
        }
        draw_slot(ui, painter, art, inventory.slot(index), cell, scale);
    }

    // Whatever is on the cursor rides with it.
    if let (Some(stack), Some(at)) = (inventory.carried(), pointer)
        && let Some(id) = art.item(&ctx, stack)
    {
        let cell = egui::Rect::from_center_size(at, egui::vec2(16.0 * scale, 16.0 * scale));
        painter.image(id, cell, full_uv(), egui::Color32::WHITE);
        if stack.count > 1 {
            count_label(painter, cell, stack.count, scale);
        }
    }
    hit
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
