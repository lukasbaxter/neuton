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
    /// The drag being painted, if the button is down.
    pub drag: crate::clicks::Drag,
}

impl Default for Inventory {
    fn default() -> Self {
        Self {
            slots: vec![None; SLOTS],
            selected: 0,
            open: false,
            carried: None,
            state_id: 0,
            drag: crate::clicks::Drag::default(),
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

    /// Lifts a slot out to be worked on, leaving it empty.
    ///
    /// Clicks are easier to write against owned stacks than against two
    /// borrows of the same vector, and a slot that ends up empty has to become
    /// `None` rather than a stack of zero -- a zero-count stack draws as an
    /// item with no number on it, which is the sort of thing nobody can
    /// explain later.
    pub(crate) fn take_slot(&mut self, index: usize) -> Option<Stack> {
        self.slots.get_mut(index).and_then(Option::take).filter(|s| s.count > 0)
    }

    pub(crate) fn put_slot(&mut self, index: usize, stack: Option<Stack>) {
        if let Some(slot) = self.slots.get_mut(index) {
            *slot = stack.filter(|s| s.count > 0);
        }
    }

    pub(crate) fn take_carried(&mut self) -> Option<Stack> {
        self.carried.take().filter(|s| s.count > 0)
    }

    pub(crate) fn put_carried(&mut self, stack: Option<Stack>) {
        self.carried = stack.filter(|s| s.count > 0);
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
    held: HashMap<String, Option<std::sync::Arc<Held>>>,
    portrait: crate::portrait::Portrait,
}

/// What one item looks like in the hand.
///
/// The two kinds are the game's own and are not the same as "is it a block":
/// a torch places a block and is carried as a flat picture, because that is
/// what its item definition asks for.
pub struct Held {
    pub geometry: HeldGeometry,
    /// How the model is held, out of its own `display` block.
    pub display: neuton_assets::Display,
}

pub enum HeldGeometry {
    /// Drawn from the block atlas, using the baked model the world already has.
    Solid,
    /// A picture with a rim, built into a solid the way the game builds one.
    Sprite { texture: String, sides: Vec<neuton_assets::Side> },
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
            held: HashMap::new(),
            portrait: crate::portrait::Portrait::default(),
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
        let name = stack.name;
        if self.packs.is_none() {
            self.packs = neuton_assets::PackStack::discover("26.2");
        }
        let Self { packs, icons, .. } = self;
        let handle = packs
            .as_mut()
            .and_then(|packs| icons.render(packs, name))
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

    /// The player, drawn into the inventory panel's own window.
    ///
    /// Split out here rather than reached through the field, because the
    /// drawing needs the pack stack and the pack stack lives on the same
    /// struct.
    pub fn portrait(
        &mut self,
        ctx: &egui::Context,
        look: crate::portrait::Look,
        pixels: [u32; 2],
    ) -> Option<egui::TextureId> {
        let Self { packs, portrait, .. } = self;
        portrait.texture(ctx, packs, look, pixels)
    }

    /// What an item looks like in the hand, worked out once and kept.
    ///
    /// The rim of a flat item is a few dozen quads read out of the sprite's
    /// own alpha, which is cheap but not free, and the answer never changes
    /// for a given item.
    pub fn held(&mut self, name: &str) -> Option<std::sync::Arc<Held>> {
        if let Some(cached) = self.held.get(name) {
            return cached.clone();
        }
        if self.packs.is_none() {
            self.packs = neuton_assets::PackStack::discover("26.2");
        }
        let Self { packs, icons, .. } = self;
        let built = packs.as_mut().and_then(|packs| {
            let resolved = icons.models().item(packs, name)?;
            let display = icons.models().display(packs, &resolved.path, "firstperson_righthand");
            let geometry = match &resolved.geometry {
                neuton_assets::ItemGeometry::Solid(_) => HeldGeometry::Solid,
                neuton_assets::ItemGeometry::Sprite(layers) => {
                    let path = layers.first()?.clone();
                    let image = icons.texture(packs, &path)?;
                    let sides =
                        neuton_assets::extrude::sides(&image.rgba, image.width, image.height);
                    // The renderer names a texture by its path under
                    // `textures/`; a resolved model reference is the whole
                    // asset path, so the front of it comes off.
                    let texture = path
                        .strip_prefix("assets/minecraft/textures/")
                        .unwrap_or(&path)
                        .to_string();
                    HeldGeometry::Sprite { texture, sides }
                }
            };
            Some(std::sync::Arc::new(Held { geometry, display }))
        });
        self.held.insert(name.to_string(), built.clone());
        built
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

/// What the pointer is doing on the inventory screen, between frames.
///
/// A click on a slot is not decided when the button goes down. Holding the
/// button and moving across slots with a stack on the cursor is a drag, not a
/// click, and which of the two it was is only known when the button comes back
/// up -- so the press is remembered here and acted on later.
#[derive(Default)]
pub struct Cursor {
    /// The button that is down, and where it went down.
    press: Option<Press>,
    /// The slot and time of the last completed click, for spotting a double.
    last: Option<(usize, f64)>,
    /// Set once a press has turned into a drag, so the release ends it rather
    /// than counting as a click.
    dragging: bool,
    /// Set when the press was already spent on something else, so the release
    /// does nothing.
    spent: bool,
    /// The slot under the pointer, for the keys that act on it.
    pub hovered: Option<usize>,
}

struct Press {
    slot: Option<usize>,
    right: bool,
    at: egui::Pos2,
}

/// How far the pointer has to move before a held button counts as a drag
/// rather than a click, in screen pixels.
const DRAG_SLOP: f32 = 3.0;

/// How long two clicks on one slot can be apart and still be a double click.
const DOUBLE_CLICK: f64 = 0.25;

/// The whole inventory screen: draws it, and reads what the pointer did.
///
/// Clicks come back rather than being applied here, because applying one means
/// both changing what is in the slots and telling the server, and neither
/// belongs in a drawing function.
pub fn screen(
    ui: &egui::Ui,
    inventory: &Inventory,
    cursor: &mut Cursor,
    art: &mut ItemArt,
    scale: f32,
    creative: bool,
) -> Vec<crate::clicks::Click> {
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

    // The player, in the window the game keeps for them, looking at the
    // pointer the way the game's does.
    let window = egui::Rect::from_min_size(
        panel.min + egui::vec2(crate::portrait::AT[0] * scale, crate::portrait::AT[1] * scale),
        egui::vec2(crate::portrait::SIZE[0] * scale, crate::portrait::SIZE[1] * scale),
    );
    {
        let pointer = ctx.pointer_latest_pos().unwrap_or_else(|| window.center());
        // The game measures this in its own interface pixels, so the offset
        // has to come back out of the scale before it is used.
        let offset = (window.center() - pointer) / scale;
        let look = crate::portrait::Look::at(offset.x, offset.y);
        // Drawn at the size it will be shown at, in real pixels.
        let ppp = ctx.pixels_per_point();
        let pixels = [
            (window.width() * ppp).round() as u32,
            (window.height() * ppp).round() as u32,
        ];
        if let Some(id) = art.portrait(&ctx, look, pixels) {
            painter.image(id, window, full_uv(), egui::Color32::WHITE);
        }
    }

    let pointer = ctx.pointer_latest_pos();
    let cells: Vec<(usize, egui::Rect)> = slot_positions()
        .into_iter()
        .map(|(index, at)| {
            (
                index,
                egui::Rect::from_min_size(
                    panel.min + egui::vec2(at[0] * scale, at[1] * scale),
                    egui::vec2(16.0 * scale, 16.0 * scale),
                ),
            )
        })
        .collect();

    // The slot's own square is a pixel out from the item on every side, and
    // that square is what the pointer has to be inside.
    let hovered = pointer.and_then(|p| {
        cells.iter().find(|(_, cell)| cell.expand(scale).contains(p)).map(|(index, _)| *index)
    });
    cursor.hovered = hovered;

    let clicks = read_pointer(&ctx, inventory, cursor, hovered, pointer, panel, creative);

    // What a drag would put in each slot, so the split is visible before the
    // button comes up rather than only after.
    let painting = drag_preview(inventory);

    for (index, cell) in &cells {
        if hovered == Some(*index) {
            painter.rect_filled(cell.expand(scale), 0.0, egui::Color32::from_white_alpha(90));
        }
        let held = inventory.slot(*index);
        match painting.get(index) {
            Some(extra) => {
                let mut shown =
                    held.cloned().or_else(|| inventory.carried().map(|c| Stack { count: 0, ..c.clone() }));
                if let Some(stack) = shown.as_mut() {
                    stack.count += extra;
                }
                draw_slot(ui, painter, art, shown.as_ref(), *cell, scale);
            }
            None => draw_slot(ui, painter, art, held, *cell, scale),
        }
    }

    // Whatever is on the cursor rides with it, minus whatever a drag in
    // progress has already promised to other slots.
    let handed_out: i32 = painting.values().sum();
    if let (Some(stack), Some(at)) = (inventory.carried(), pointer)
        && stack.count > handed_out
        && let Some(id) = art.item(&ctx, stack)
    {
        let cell = egui::Rect::from_center_size(at, egui::vec2(16.0 * scale, 16.0 * scale));
        painter.image(id, cell, full_uv(), egui::Color32::WHITE);
        if stack.count - handed_out > 1 {
            count_label(painter, cell, stack.count - handed_out, scale);
        }
    }
    clicks
}

/// Turns this frame's pointer state into clicks.
fn read_pointer(
    ctx: &egui::Context,
    inventory: &Inventory,
    cursor: &mut Cursor,
    hovered: Option<usize>,
    pointer: Option<egui::Pos2>,
    panel: egui::Rect,
    creative: bool,
) -> Vec<crate::clicks::Click> {
    use crate::clicks::{Click, DragKind};
    use egui::PointerButton;

    let mut out = Vec::new();
    let (pressed_primary, pressed_secondary, pressed_middle, released, shift, now) = ctx
        .input(|i| {
            (
                i.pointer.button_pressed(PointerButton::Primary),
                i.pointer.button_pressed(PointerButton::Secondary),
                i.pointer.button_pressed(PointerButton::Middle),
                i.pointer.button_released(PointerButton::Primary)
                    || i.pointer.button_released(PointerButton::Secondary),
                i.modifiers.shift,
                i.time,
            )
        });

    // Middle click in creative fills the cursor, and does nothing anywhere
    // else, so it never becomes a press worth remembering.
    if pressed_middle && creative && let Some(slot) = hovered {
        out.push(Click::Clone { slot });
        return out;
    }

    if pressed_primary || pressed_secondary {
        let right = pressed_secondary && !pressed_primary;
        cursor.dragging = false;
        cursor.spent = false;
        cursor.press = Some(Press { slot: hovered, right, at: pointer.unwrap_or_default() });

        match (hovered, inventory.carried().is_some()) {
            // An empty cursor acts at once: there is nothing to drag, and
            // waiting for the button to come up only makes it feel slow.
            (Some(slot), false) => {
                cursor.spent = true;
                if shift {
                    out.push(Click::QuickMove { slot });
                } else {
                    out.push(Click::Pickup { slot, right });
                }
                cursor.last = Some((slot, now));
            }
            // A second click on the same slot, still holding what the first
            // one picked up: gather every matching stack onto the cursor.
            (Some(slot), true) => {
                let doubled = !right
                    && !shift
                    && cursor.last.is_some_and(|(last, at)| last == slot && now - at < DOUBLE_CLICK);
                if doubled {
                    cursor.spent = true;
                    cursor.last = None;
                    out.push(Click::Gather { slot });
                }
            }
            (None, _) => {}
        }
        return out;
    }

    // Holding a stack and moving across slots paints a drag. The first slot
    // painted is the one the button went down on, so a drag that starts on a
    // slot fills that one too.
    if let Some(press) = cursor.press.as_ref()
        && !cursor.spent
        && inventory.carried().is_some()
    {
        let moved = pointer.is_some_and(|p| (p - press.at).length() > DRAG_SLOP);
        let elsewhere = hovered.is_some() && hovered != press.slot;
        if !cursor.dragging && (moved || elsewhere) {
            cursor.dragging = true;
            let kind = if press.right { DragKind::One } else { DragKind::Even };
            out.push(Click::DragStart(kind));
            if let Some(slot) = press.slot {
                out.push(Click::DragOver { slot });
            }
        }
        if cursor.dragging && let Some(slot) = hovered {
            out.push(Click::DragOver { slot });
        }
    }

    if released {
        let press = cursor.press.take();
        let dragging = std::mem::take(&mut cursor.dragging);
        let spent = std::mem::take(&mut cursor.spent);
        if dragging {
            out.push(Click::DragEnd);
        } else if !spent && let Some(press) = press {
            match press.slot {
                Some(slot) if shift => {
                    out.push(Click::QuickMove { slot });
                    cursor.last = Some((slot, now));
                }
                Some(slot) => {
                    out.push(Click::Pickup { slot, right: press.right });
                    cursor.last = Some((slot, now));
                }
                // Away from the panel with something in hand: put it down, on
                // the ground. Without this the stack sticks to the pointer for
                // good, because the server was never told to let go of it.
                None if !panel.contains(press.at) && inventory.carried().is_some() => {
                    out.push(Click::DropCarried { whole_stack: !press.right });
                }
                None => {}
            }
        }
    }
    out
}

/// How many of the carried stack each painted slot would receive.
fn drag_preview(inventory: &Inventory) -> std::collections::HashMap<usize, i32> {
    use crate::clicks::DragKind;
    let mut out = std::collections::HashMap::new();
    let (Some(kind), Some(carried)) = (inventory.drag.kind, inventory.carried()) else {
        return out;
    };
    if inventory.drag.slots.is_empty() {
        return out;
    }
    let each = match kind {
        DragKind::Even => carried.count / inventory.drag.slots.len() as i32,
        DragKind::One => 1,
        DragKind::Fill => carried.count,
    };
    let mut left = carried.count;
    for index in &inventory.drag.slots {
        let already = inventory.slot(*index).map_or(0, |s| s.count);
        let space = (crate::clicks::slot_limit(*index, carried) - already).max(0);
        let moved = each.min(space).min(left);
        if moved > 0 {
            out.insert(*index, moved);
            left -= moved;
        }
    }
    out
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
