//! What a click on a slot does, worked out here rather than waited for.
//!
//! The server has the last word on every one of these, and says so by sending
//! the whole container back. But a round trip is fifty milliseconds on a good
//! day and a hundred and fifty on a real server, and an inventory that only
//! moves once the server agrees feels broken in a way that is hard to argue
//! with: the stack you picked up is still in the slot, and the one on your
//! cursor is not there yet. So every click is applied locally first, exactly
//! the way the game applies it, and the server's answer either agrees or
//! corrects it.
//!
//! The rules below are the game's own `AbstractContainerMenu.doClick` and
//! `InventoryMenu.quickMoveStack`, kept in the same order so they can be read
//! against each other.

use crate::inventory::{
    ARMOUR, BACKPACK, CRAFTING_GRID, CRAFTING_OUTPUT, HOTBAR, Inventory, OFF_HAND, SLOTS,
};
use neuton_blocks::items::{Equips, item};
use neuton_net::items::Stack;

/// The slot number that means "not a slot at all": outside the window.
pub const OUTSIDE: i16 = -999;

/// The button number a swap uses for the off hand, rather than a hotbar slot.
pub const OFF_HAND_BUTTON: u8 = 40;

/// Which of the three drags is being painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragKind {
    /// Left button: what is carried is split evenly between the slots.
    Even,
    /// Right button: one goes in each slot.
    One,
    /// Middle button, and only in creative: each slot is filled.
    Fill,
}

impl DragKind {
    /// The high half of the button byte, which is what says which drag this is.
    fn code(self) -> u8 {
        match self {
            DragKind::Even => 0,
            DragKind::One => 1,
            DragKind::Fill => 2,
        }
    }
}

/// A click, as the player made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    /// Pick a stack up, put it down, or swap it for what is there. The right
    /// button takes half and places one.
    Pickup { slot: usize, right: bool },
    /// Shift-click: send the stack to the other half of the screen.
    QuickMove { slot: usize },
    /// A number key over a slot, or F for the off hand.
    Swap { slot: usize, button: u8 },
    /// Middle click in creative: fill the cursor with this.
    Clone { slot: usize },
    /// Q over a slot. Control makes it the whole stack.
    Throw { slot: usize, whole_stack: bool },
    /// Clicking away from the window while carrying something.
    DropCarried { whole_stack: bool },
    /// Double click: pull every matching stack onto the cursor.
    Gather { slot: usize },
    /// A drag has begun; nothing has been painted yet.
    DragStart(DragKind),
    /// One more slot painted by the drag in progress.
    DragOver { slot: usize },
    /// The button came back up: hand out what was painted.
    DragEnd,
}

/// One click packet, ready for the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wire {
    pub slot: i16,
    pub button: u8,
    pub mode: i32,
}

impl Wire {
    fn new(slot: i16, button: u8, mode: i32) -> Self {
        Self { slot, button, mode }
    }
}

/// How many of an item fit in one slot.
fn max_stack(stack: &Stack) -> i32 {
    item(stack.id).map_or(64, |i| i.max_stack as i32)
}

/// Whether two stacks are the same thing and so can be merged.
///
/// Everything but the count, which is what the game compares too. Two stacks
/// that differ only in a component this client cannot read yet will merge here
/// and be pulled apart again by the server's correction, which is the right
/// way round: the alternative is refusing to merge two identical stacks.
fn same(a: &Stack, b: &Stack) -> bool {
    a.id == b.id
        && a.damage == b.damage
        && a.enchanted == b.enchanted
        && a.custom_name == b.custom_name
}

/// Whether a stack can be piled up at all.
fn stackable(stack: &Stack) -> bool {
    max_stack(stack) > 1 && stack.damage == 0
}

/// Which armour slot a piece of armour belongs in, if it is armour.
///
/// The container numbers armour downwards from the head, which is why this is
/// a subtraction: the game writes it as `8 - slot.getIndex()`.
fn armour_slot(stack: &Stack) -> Option<usize> {
    match item(stack.id)?.equips? {
        Equips::Head => Some(5),
        Equips::Chest => Some(6),
        Equips::Legs => Some(7),
        Equips::Feet => Some(8),
        Equips::OffHand => None,
    }
}

/// Whether this stack is worn in the off hand, the way a shield is.
fn off_hand_item(stack: &Stack) -> bool {
    item(stack.id).and_then(|i| i.equips) == Some(Equips::OffHand)
}

/// How many of this a given slot will hold: the smaller of what the item
/// stacks to and what the slot allows. Armour slots hold one of anything.
pub(crate) fn slot_limit(index: usize, stack: &Stack) -> i32 {
    if ARMOUR.contains(&index) {
        return 1;
    }
    max_stack(stack)
}

/// Whether a slot will take this stack at all.
fn may_place(index: usize, stack: &Stack) -> bool {
    if index == CRAFTING_OUTPUT {
        // Things come out of it; nothing goes in.
        return false;
    }
    if ARMOUR.contains(&index) {
        return armour_slot(stack) == Some(index);
    }
    true
}

/// The state a drag being painted needs, between the button going down and
/// coming back up.
#[derive(Debug, Default, Clone)]
pub struct Drag {
    pub kind: Option<DragKind>,
    /// In the order they were painted, which is what decides who gets the
    /// remainder.
    pub slots: Vec<usize>,
}

impl Drag {
    pub fn is_active(&self) -> bool {
        self.kind.is_some()
    }

    fn clear(&mut self) {
        self.kind = None;
        self.slots.clear();
    }
}

impl Inventory {
    /// Applies a click and returns the packets that say so.
    ///
    /// The local state moves first and the packets describe what was done, so
    /// what the player sees is immediate and what the server sees is the same
    /// click it would have got from the game.
    pub fn click(&mut self, click: Click) -> Vec<Wire> {
        match click {
            Click::Pickup { slot, right } => self.pickup(slot, right),
            Click::QuickMove { slot } => self.quick_move(slot),
            Click::Swap { slot, button } => self.swap(slot, button),
            Click::Clone { slot } => self.clone_slot(slot),
            Click::Throw { slot, whole_stack } => self.throw(slot, whole_stack),
            Click::DropCarried { whole_stack } => self.drop_carried(whole_stack),
            Click::Gather { slot } => self.gather(slot),
            Click::DragStart(kind) => self.drag_start(kind),
            Click::DragOver { slot } => self.drag_over(slot),
            Click::DragEnd => self.drag_end(),
        }
    }

    /// Left or right click on a slot: mode zero.
    fn pickup(&mut self, index: usize, right: bool) -> Vec<Wire> {
        if index >= SLOTS {
            return Vec::new();
        }
        let wire = vec![Wire::new(index as i16, u8::from(right), 0)];
        let mut carried = self.take_carried();
        let mut slot = self.take_slot(index);

        match (&mut slot, &mut carried) {
            // An empty slot takes what is carried, all of it or one of it.
            (None, Some(held)) => {
                if may_place(index, held) {
                    let want = if right { 1 } else { held.count };
                    let moved = want.min(slot_limit(index, held)).min(held.count);
                    if moved > 0 {
                        slot = Some(Stack { count: moved, ..held.clone() });
                        held.count -= moved;
                    }
                }
            }
            // A full slot and an empty hand: take all of it, or half of it
            // rounded up, which is what makes right-clicking one item take it.
            (Some(there), None) => {
                let want = if right { (there.count + 1) / 2 } else { there.count };
                carried = Some(Stack { count: want, ..there.clone() });
                there.count -= want;
            }
            (Some(there), Some(held)) => {
                if same(there, held) && stackable(there) {
                    if may_place(index, held) {
                        // Pile onto what is there.
                        let space = (slot_limit(index, there) - there.count).max(0);
                        let want = if right { 1 } else { held.count };
                        let moved = want.min(space).min(held.count);
                        there.count += moved;
                        held.count -= moved;
                    } else {
                        // A slot that will not take anything -- the crafting
                        // result -- still gives its items up onto a matching
                        // cursor.
                        let space = (max_stack(held) - held.count).max(0);
                        let moved = space.min(there.count);
                        held.count += moved;
                        there.count -= moved;
                    }
                } else if may_place(index, held) && held.count <= slot_limit(index, held) {
                    std::mem::swap(there, held);
                }
            }
            (None, None) => {}
        }

        self.put_slot(index, slot);
        self.put_carried(carried);
        wire
    }

    /// Shift-click: mode one.
    ///
    /// Where a stack goes is the game's own order for the player's own screen,
    /// and the order is the whole of what makes shift-clicking feel right: out
    /// of the hotbar goes up into the backpack, out of the backpack goes down
    /// into the hotbar, and a helmet goes on your head before it goes anywhere.
    fn quick_move(&mut self, index: usize) -> Vec<Wire> {
        if index >= SLOTS {
            return Vec::new();
        }
        let wire = vec![Wire::new(index as i16, 0, 1)];
        let Some(mut stack) = self.take_slot(index) else { return wire };

        let armour = armour_slot(&stack);
        let backpack_and_hotbar = BACKPACK.start..OFF_HAND;

        if index == CRAFTING_OUTPUT {
            // Filled from the back, so a crafted stack lands in the hotbar
            // rather than the top left of the backpack.
            self.move_into(&mut stack, backpack_and_hotbar, true);
        } else if CRAFTING_GRID.contains(&index) || ARMOUR.contains(&index) || index == OFF_HAND {
            self.move_into(&mut stack, backpack_and_hotbar, false);
        } else if armour.is_some_and(|slot| self.slot(slot).is_none()) {
            let slot = armour.unwrap_or(ARMOUR.start);
            self.move_into(&mut stack, slot..slot + 1, false);
        } else if off_hand_item(&stack) && self.slot(OFF_HAND).is_none() {
            self.move_into(&mut stack, OFF_HAND..OFF_HAND + 1, false);
        } else if BACKPACK.contains(&index) {
            self.move_into(&mut stack, HOTBAR, false);
        } else if HOTBAR.contains(&index) {
            self.move_into(&mut stack, BACKPACK, false);
        } else {
            self.move_into(&mut stack, backpack_and_hotbar, false);
        }

        self.put_slot(index, (stack.count > 0).then_some(stack));
        wire
    }

    /// A number key or F over a slot: mode two.
    fn swap(&mut self, index: usize, button: u8) -> Vec<Wire> {
        let target = match button {
            OFF_HAND_BUTTON => OFF_HAND,
            n if (n as usize) < HOTBAR.len() => HOTBAR.start + n as usize,
            _ => return Vec::new(),
        };
        if index >= SLOTS || index == target {
            return Vec::new();
        }
        let wire = vec![Wire::new(index as i16, button, 2)];
        let here = self.take_slot(index);
        let there = self.take_slot(target);
        // The armour slots are the only ones that can refuse, and refusing
        // means nothing moves rather than a piece vanishing.
        let allowed = here.as_ref().is_none_or(|s| may_place(target, s))
            && there.as_ref().is_none_or(|s| may_place(index, s));
        if allowed {
            self.put_slot(index, there);
            self.put_slot(target, here);
        } else {
            self.put_slot(index, here);
            self.put_slot(target, there);
        }
        wire
    }

    /// Middle click in creative: mode three.
    fn clone_slot(&mut self, index: usize) -> Vec<Wire> {
        if index >= SLOTS {
            return Vec::new();
        }
        let wire = vec![Wire::new(index as i16, 2, 3)];
        if self.carried().is_none()
            && let Some(there) = self.slot(index)
        {
            let full = Stack { count: max_stack(there), ..there.clone() };
            self.put_carried(Some(full));
        }
        wire
    }

    /// Q over a slot: mode four.
    fn throw(&mut self, index: usize, whole_stack: bool) -> Vec<Wire> {
        if index >= SLOTS || self.carried().is_some() {
            return Vec::new();
        }
        let wire = vec![Wire::new(index as i16, u8::from(whole_stack), 4)];
        let Some(mut stack) = self.take_slot(index) else { return Vec::new() };
        if whole_stack {
            stack.count = 0;
        } else {
            stack.count -= 1;
        }
        self.put_slot(index, (stack.count > 0).then_some(stack));
        wire
    }

    /// Clicking away from the window with something on the cursor.
    ///
    /// This is the one that leaves a ghost when it is missing: nothing is sent,
    /// so the server still believes the stack is on the cursor, and it stays
    /// stuck to the pointer with no way to put it down.
    fn drop_carried(&mut self, whole_stack: bool) -> Vec<Wire> {
        let Some(mut carried) = self.take_carried() else { return Vec::new() };
        let wire = vec![Wire::new(OUTSIDE, u8::from(!whole_stack), 0)];
        if whole_stack {
            carried.count = 0;
        } else {
            carried.count -= 1;
        }
        self.put_carried((carried.count > 0).then_some(carried));
        wire
    }

    /// Double click: mode six.
    ///
    /// Twice over the whole container, because the first pass leaves full
    /// stacks alone: gathering out of a full stack first would break it up for
    /// no gain, and the game goes round again to take them only if it has to.
    fn gather(&mut self, index: usize) -> Vec<Wire> {
        if index >= SLOTS {
            return Vec::new();
        }
        let wire = vec![Wire::new(index as i16, 0, 6)];
        let Some(mut carried) = self.take_carried() else { return Vec::new() };
        let ceiling = max_stack(&carried);
        for pass in 0..2 {
            for slot in 0..SLOTS {
                if carried.count >= ceiling {
                    break;
                }
                if slot == CRAFTING_OUTPUT {
                    continue;
                }
                let Some(mut there) = self.take_slot(slot) else { continue };
                let full = there.count >= max_stack(&there);
                if same(&there, &carried) && !(pass == 0 && full) {
                    let moved = (ceiling - carried.count).min(there.count);
                    carried.count += moved;
                    there.count -= moved;
                }
                self.put_slot(slot, (there.count > 0).then_some(there));
            }
        }
        self.put_carried(Some(carried));
        wire
    }

    /// The button went down with a stack on the cursor: mode five, stage zero.
    fn drag_start(&mut self, kind: DragKind) -> Vec<Wire> {
        if self.carried().is_none() {
            return Vec::new();
        }
        self.drag.clear();
        self.drag.kind = Some(kind);
        vec![Wire::new(OUTSIDE, kind.code() * 4, 5)]
    }

    /// One more slot under the pointer: mode five, stage one.
    fn drag_over(&mut self, index: usize) -> Vec<Wire> {
        let Some(kind) = self.drag.kind else { return Vec::new() };
        if index >= SLOTS || self.drag.slots.contains(&index) {
            return Vec::new();
        }
        let Some(carried) = self.carried().cloned() else { return Vec::new() };
        // A slot that cannot take it is not painted, and a slot already full
        // of something else is not either.
        if !may_place(index, &carried) {
            return Vec::new();
        }
        match self.slot(index) {
            Some(there) if !same(there, &carried) => return Vec::new(),
            Some(there) if there.count >= slot_limit(index, there) => return Vec::new(),
            _ => {}
        }
        self.drag.slots.push(index);
        vec![Wire::new(index as i16, kind.code() * 4 + 1, 5)]
    }

    /// The button came up: mode five, stage two, and the paint is applied.
    fn drag_end(&mut self) -> Vec<Wire> {
        let Some(kind) = self.drag.kind else { return Vec::new() };
        let painted = std::mem::take(&mut self.drag.slots);
        self.drag.clear();
        let wire = vec![Wire::new(OUTSIDE, kind.code() * 4 + 2, 5)];
        let Some(mut carried) = self.take_carried() else { return wire };
        if painted.is_empty() {
            self.put_carried(Some(carried));
            return wire;
        }

        // An even drag hands out the same number to each slot and keeps the
        // remainder, which is why dragging seven across two slots leaves one
        // behind rather than making one of them heavier.
        let each = match kind {
            DragKind::Even => carried.count / painted.len() as i32,
            DragKind::One => 1,
            DragKind::Fill => max_stack(&carried),
        };
        for index in painted {
            if carried.count <= 0 {
                break;
            }
            let mut there = self.take_slot(index);
            let already = there.as_ref().map_or(0, |s| s.count);
            let space = (slot_limit(index, &carried) - already).max(0);
            let moved = each.min(space).min(carried.count);
            if moved > 0 {
                match &mut there {
                    Some(stack) => stack.count += moved,
                    None => there = Some(Stack { count: moved, ..carried.clone() }),
                }
                carried.count -= moved;
            }
            self.put_slot(index, there);
        }
        self.put_carried((carried.count > 0).then_some(carried));
        wire
    }

    /// The game's `moveItemStackTo`: pile onto matching stacks first, then
    /// take the first empty slot.
    fn move_into(&mut self, stack: &mut Stack, range: std::ops::Range<usize>, reverse: bool) {
        let order: Vec<usize> =
            if reverse { range.clone().rev().collect() } else { range.clone().collect() };

        if stackable(stack) {
            for index in &order {
                if stack.count <= 0 {
                    break;
                }
                let Some(mut there) = self.take_slot(*index) else { continue };
                if same(&there, stack) && may_place(*index, stack) {
                    let space = (slot_limit(*index, &there) - there.count).max(0);
                    let moved = space.min(stack.count);
                    there.count += moved;
                    stack.count -= moved;
                }
                self.put_slot(*index, Some(there));
            }
        }
        if stack.count <= 0 {
            return;
        }
        for index in &order {
            if self.slot(*index).is_some() || !may_place(*index, stack) {
                continue;
            }
            let moved = stack.count.min(slot_limit(*index, stack));
            self.put_slot(*index, Some(Stack { count: moved, ..stack.clone() }));
            stack.count -= moved;
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(name: &'static str, count: i32) -> Stack {
        // Real protocol IDs, so the stack limits come out of the generated
        // table rather than being made up here.
        let id = neuton_blocks::items::ITEMS
            .iter()
            .position(|i| i.name == name)
            .expect("no such item") as i32;
        Stack { count, id, name, ..Default::default() }
    }

    fn with(slots: &[(usize, Stack)]) -> Inventory {
        let mut inventory = Inventory::default();
        for (index, s) in slots {
            inventory.set(*index as i32, Some(s.clone()));
        }
        inventory
    }

    #[test]
    fn a_left_click_takes_the_whole_stack_and_a_right_click_takes_half() {
        let mut inventory = with(&[(9, stack("stone", 7))]);
        inventory.click(Click::Pickup { slot: 9, right: false });
        assert_eq!(inventory.carried().map(|s| s.count), Some(7));
        assert!(inventory.slot(9).is_none());

        let mut inventory = with(&[(9, stack("stone", 7))]);
        inventory.click(Click::Pickup { slot: 9, right: true });
        assert_eq!(inventory.carried().map(|s| s.count), Some(4), "half, rounded up");
        assert_eq!(inventory.slot(9).map(|s| s.count), Some(3));
    }

    #[test]
    fn a_right_click_puts_one_down_at_a_time() {
        let mut inventory = Inventory::default();
        inventory.set_carried(Some(stack("stone", 3)));
        inventory.click(Click::Pickup { slot: 9, right: true });
        assert_eq!(inventory.slot(9).map(|s| s.count), Some(1));
        assert_eq!(inventory.carried().map(|s| s.count), Some(2));
    }

    #[test]
    fn a_stack_only_takes_what_fits() {
        let mut inventory = with(&[(9, stack("ender_pearl", 14))]);
        inventory.set_carried(Some(stack("ender_pearl", 5)));
        inventory.click(Click::Pickup { slot: 9, right: false });
        assert_eq!(inventory.slot(9).map(|s| s.count), Some(16), "pearls stop at sixteen");
        assert_eq!(inventory.carried().map(|s| s.count), Some(3));
    }

    #[test]
    fn two_different_things_swap_places() {
        let mut inventory = with(&[(9, stack("stone", 4))]);
        inventory.set_carried(Some(stack("dirt", 2)));
        inventory.click(Click::Pickup { slot: 9, right: false });
        assert_eq!(inventory.slot(9).map(|s| s.name), Some("dirt"));
        assert_eq!(inventory.carried().map(|s| s.name), Some("stone"));
    }

    #[test]
    fn dropping_outside_the_window_lets_go_of_it() {
        let mut inventory = Inventory::default();
        inventory.set_carried(Some(stack("stone", 5)));
        let wire = inventory.click(Click::DropCarried { whole_stack: true });
        assert!(inventory.carried().is_none(), "the cursor is empty afterwards");
        assert_eq!(wire, vec![Wire { slot: OUTSIDE, button: 0, mode: 0 }]);

        let mut inventory = Inventory::default();
        inventory.set_carried(Some(stack("stone", 5)));
        let wire = inventory.click(Click::DropCarried { whole_stack: false });
        assert_eq!(inventory.carried().map(|s| s.count), Some(4), "one at a time");
        assert_eq!(wire, vec![Wire { slot: OUTSIDE, button: 1, mode: 0 }]);
    }

    #[test]
    fn shift_click_sends_the_hotbar_up_and_the_backpack_down() {
        let mut inventory = with(&[(36, stack("stone", 5))]);
        inventory.click(Click::QuickMove { slot: 36 });
        assert!(inventory.slot(36).is_none());
        assert_eq!(inventory.slot(9).map(|s| s.count), Some(5), "into the backpack");

        let mut inventory = with(&[(9, stack("stone", 5))]);
        inventory.click(Click::QuickMove { slot: 9 });
        assert!(inventory.slot(9).is_none());
        assert_eq!(inventory.slot(36).map(|s| s.count), Some(5), "into the hotbar");
    }

    #[test]
    fn shift_click_piles_onto_a_matching_stack_before_taking_a_new_slot() {
        let mut inventory = with(&[(36, stack("stone", 5)), (10, stack("stone", 60))]);
        inventory.click(Click::QuickMove { slot: 36 });
        assert_eq!(inventory.slot(10).map(|s| s.count), Some(64), "filled to the top first");
        assert_eq!(inventory.slot(9).map(|s| s.count), Some(1), "and the rest goes elsewhere");
    }

    #[test]
    fn a_helmet_shift_clicks_onto_your_head() {
        let mut inventory = with(&[(9, stack("diamond_helmet", 1))]);
        inventory.click(Click::QuickMove { slot: 9 });
        assert_eq!(inventory.slot(5).map(|s| s.name), Some("diamond_helmet"));
        assert!(inventory.slot(9).is_none());
    }

    #[test]
    fn a_shield_shift_clicks_into_the_off_hand() {
        let mut inventory = with(&[(9, stack("shield", 1))]);
        inventory.click(Click::QuickMove { slot: 9 });
        assert_eq!(inventory.slot(OFF_HAND).map(|s| s.name), Some("shield"));
    }

    #[test]
    fn an_armour_slot_refuses_anything_that_is_not_that_armour() {
        let mut inventory = Inventory::default();
        inventory.set_carried(Some(stack("stone", 5)));
        inventory.click(Click::Pickup { slot: 5, right: false });
        assert!(inventory.slot(5).is_none(), "stone does not go on your head");
        assert_eq!(inventory.carried().map(|s| s.count), Some(5), "and is still carried");
    }

    #[test]
    fn a_number_key_swaps_with_that_hotbar_slot() {
        let mut inventory = with(&[(9, stack("stone", 5)), (38, stack("dirt", 2))]);
        inventory.click(Click::Swap { slot: 9, button: 2 });
        assert_eq!(inventory.slot(9).map(|s| s.name), Some("dirt"));
        assert_eq!(inventory.slot(38).map(|s| s.name), Some("stone"));
    }

    #[test]
    fn throwing_takes_one_or_all_of_a_slot() {
        let mut inventory = with(&[(9, stack("stone", 5))]);
        inventory.click(Click::Throw { slot: 9, whole_stack: false });
        assert_eq!(inventory.slot(9).map(|s| s.count), Some(4));
        inventory.click(Click::Throw { slot: 9, whole_stack: true });
        assert!(inventory.slot(9).is_none());
    }

    #[test]
    fn a_double_click_gathers_the_loose_stacks_first() {
        let mut inventory =
            with(&[(9, stack("stone", 64)), (10, stack("stone", 5)), (11, stack("stone", 3))]);
        inventory.click(Click::Pickup { slot: 11, right: false });
        inventory.click(Click::Gather { slot: 11 });
        assert_eq!(inventory.carried().map(|s| s.count), Some(64));
        assert!(inventory.slot(10).is_none(), "the loose stack went first");
        assert_eq!(inventory.slot(9).map(|s| s.count), Some(8), "the full one gave up the rest");
    }

    #[test]
    fn an_even_drag_splits_and_keeps_the_remainder() {
        let mut inventory = Inventory::default();
        inventory.set_carried(Some(stack("stone", 7)));
        inventory.click(Click::DragStart(DragKind::Even));
        inventory.click(Click::DragOver { slot: 9 });
        inventory.click(Click::DragOver { slot: 10 });
        let wire = inventory.click(Click::DragEnd);
        assert_eq!(inventory.slot(9).map(|s| s.count), Some(3));
        assert_eq!(inventory.slot(10).map(|s| s.count), Some(3));
        assert_eq!(inventory.carried().map(|s| s.count), Some(1), "the odd one stays put");
        assert_eq!(wire, vec![Wire { slot: OUTSIDE, button: 2, mode: 5 }]);
    }

    #[test]
    fn a_right_drag_puts_one_in_each() {
        let mut inventory = Inventory::default();
        inventory.set_carried(Some(stack("stone", 7)));
        inventory.click(Click::DragStart(DragKind::One));
        for slot in [9, 10, 11] {
            inventory.click(Click::DragOver { slot });
        }
        inventory.click(Click::DragEnd);
        for slot in [9, 10, 11] {
            assert_eq!(inventory.slot(slot).map(|s| s.count), Some(1));
        }
        assert_eq!(inventory.carried().map(|s| s.count), Some(4));
    }

    #[test]
    fn a_drag_does_not_paint_a_slot_holding_something_else() {
        let mut inventory = with(&[(10, stack("dirt", 1))]);
        inventory.set_carried(Some(stack("stone", 4)));
        inventory.click(Click::DragStart(DragKind::Even));
        assert!(inventory.click(Click::DragOver { slot: 10 }).is_empty());
        inventory.click(Click::DragOver { slot: 9 });
        inventory.click(Click::DragEnd);
        assert_eq!(inventory.slot(10).map(|s| s.name), Some("dirt"), "left alone");
        assert_eq!(inventory.slot(9).map(|s| s.count), Some(4));
    }

    #[test]
    fn the_wire_numbers_are_the_ones_the_server_expects() {
        let mut inventory = with(&[(9, stack("stone", 1))]);
        assert_eq!(
            inventory.click(Click::Pickup { slot: 9, right: true }),
            vec![Wire { slot: 9, button: 1, mode: 0 }]
        );
        assert_eq!(
            inventory.click(Click::QuickMove { slot: 9 }),
            vec![Wire { slot: 9, button: 0, mode: 1 }]
        );
        let mut inventory = Inventory::default();
        inventory.set_carried(Some(stack("stone", 4)));
        assert_eq!(
            inventory.click(Click::DragStart(DragKind::One)),
            vec![Wire { slot: OUTSIDE, button: 4, mode: 5 }],
            "a right drag starts at four"
        );
        assert_eq!(
            inventory.click(Click::DragOver { slot: 9 }),
            vec![Wire { slot: 9, button: 5, mode: 5 }]
        );
    }
}
