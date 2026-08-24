//! The launcher screen: accounts, and the server to join.

use crate::auth_task::{SignIn, SignInTask};
use crate::theme::{self, ACCENT, CARD, DANGER, DIM, FG, LINE, MID, WARN};
use egui::{Align, Frame, Layout, RichText, Stroke};
use neuton_auth::Accounts;
use std::path::PathBuf;

pub struct Launcher {
    accounts: Accounts,
    path: PathBuf,
    signin: SignInTask,
    server: String,
    /// Set when the user clicks remove, cleared on confirm or cancel.
    pending_remove: Option<u128>,
    notice: Option<(String, egui::Color32)>,
}

impl Launcher {
    pub fn new() -> Self {
        let path = Accounts::default_path().unwrap_or_else(|_| PathBuf::from("accounts.json"));
        Self {
            accounts: Accounts::load(&path),
            path,
            signin: SignInTask::default(),
            server: String::new(),
            pending_remove: None,
            notice: None,
        }
    }

    fn reload(&mut self) {
        self.accounts = Accounts::load(&self.path);
    }

    pub fn update(&mut self, ui: &mut egui::Ui) {
        if self.signin.poll() {
            self.reload();
        }
        let ctx = ui.ctx().clone();

        egui::CentralPanel::default()
            .frame(Frame::new().fill(theme::BG).inner_margin(egui::Margin::same(28)))
            .show(ui, |ui| {
                ui.set_max_width(760.0);
                self.header(ui);
                ui.add_space(22.0);
                self.accounts_section(ui);
                ui.add_space(22.0);
                self.play_section(ui);

                if let Some((msg, colour)) = &self.notice {
                    ui.add_space(14.0);
                    ui.label(RichText::new(msg).color(*colour).size(13.0));
                }
            });

        // While a sign-in is in flight the window must keep animating, since
        // the result arrives on a channel rather than as an input event.
        if self.signin.is_running() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 4.5, ACCENT);
            ui.add_space(2.0);
            ui.label(RichText::new("neuton").size(19.0).strong().color(FG));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(theme::mono("26.2 · protocol 776", DIM).size(12.0));
            });
        });
    }

    fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
        Frame::new()
            .fill(CARD)
            .stroke(Stroke::new(1.0, LINE))
            .corner_radius(10)
            .inner_margin(egui::Margin::same(18))
            .show(ui, add)
            .inner
    }

    fn accounts_section(&mut self, ui: &mut egui::Ui) {
        ui.label(theme::mono("ACCOUNTS", DIM).size(11.5));
        ui.add_space(8.0);

        Self::card(ui, |ui| {
            if self.accounts.is_empty() {
                ui.label(RichText::new("No accounts signed in.").color(MID).size(14.0));
                ui.add_space(4.0);
            }

            // Collected up front: the rows below mutate the store, and holding
            // a borrow across that would not compile.
            let rows: Vec<(u128, String, String, bool, bool)> = self
                .accounts
                .list()
                .iter()
                .map(|a| {
                    (
                        a.profile.uuid,
                        a.profile.name.clone(),
                        a.profile.uuid_hyphenated(),
                        a.is_valid(),
                        self.accounts.is_active(a),
                    )
                })
                .collect();

            let mut switch_to: Option<String> = None;
            let mut remove: Option<u128> = None;

            for (uuid, name, uuid_text, valid, active) in &rows {
                ui.horizontal(|ui| {
                    let dot = if *active { ACCENT } else { LINE };
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 3.5, dot);

                    ui.vertical(|ui| {
                        ui.label(RichText::new(name).size(14.5).strong().color(FG));
                        ui.label(theme::mono(uuid_text, DIM).size(11.0));
                    });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(RichText::new("Remove").size(12.5).color(DANGER)).clicked() {
                            remove = Some(*uuid);
                        }
                        if !*active && ui.button(RichText::new("Use").size(12.5)).clicked() {
                            switch_to = Some(name.clone());
                        }
                        if !*valid {
                            ui.label(RichText::new("needs refresh").color(WARN).size(11.5));
                        } else if *active {
                            ui.label(theme::mono("active", ACCENT).size(11.5));
                        }
                    });
                });
                ui.add_space(6.0);
            }

            if let Some(name) = switch_to {
                self.accounts.set_active(&name);
                let _ = self.accounts.save();
            }
            if let Some(uuid) = remove {
                self.pending_remove = Some(uuid);
            }

            self.confirm_remove(ui);
            ui.add_space(6.0);
            self.sign_in_controls(ui);
        });
    }

    fn confirm_remove(&mut self, ui: &mut egui::Ui) {
        let Some(uuid) = self.pending_remove else { return };
        let Some(name) = self
            .accounts
            .list()
            .iter()
            .find(|a| a.profile.uuid == uuid)
            .map(|a| a.profile.name.clone())
        else {
            self.pending_remove = None;
            return;
        };

        ui.add_space(4.0);
        Frame::new()
            .fill(theme::RAISE)
            .stroke(Stroke::new(1.0, DANGER.gamma_multiply(0.5)))
            .corner_radius(8)
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!("Sign {name} out of this computer?"))
                            .color(MID)
                            .size(13.5),
                    );
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Sign out").color(DANGER).size(13.0)).clicked() {
                        self.accounts.remove(&name);
                        let _ = self.accounts.save();
                        self.pending_remove = None;
                        self.notice = Some((format!("Signed {name} out."), MID));
                    }
                    if ui.button(RichText::new("Cancel").size(13.0)).clicked() {
                        self.pending_remove = None;
                    }
                });
            });
    }

    fn sign_in_controls(&mut self, ui: &mut egui::Ui) {
        match self.signin.state.clone() {
            SignIn::Idle => {
                if ui.button(RichText::new("Add account").size(13.5)).clicked() {
                    self.notice = None;
                    self.signin.start(self.path.clone());
                }
            }
            SignIn::Starting => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Contacting Microsoft...").color(MID).size(13.5));
                });
            }
            SignIn::Waiting { code, url } => {
                ui.add_space(4.0);
                Frame::new()
                    .fill(theme::RAISE)
                    .stroke(Stroke::new(1.0, ACCENT.gamma_multiply(0.45)))
                    .corner_radius(8)
                    .inner_margin(egui::Margin::same(14))
                    .show(ui, |ui| {
                        ui.label(RichText::new("Enter this code to sign in").color(MID).size(13.0));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(&code).monospace().size(26.0).strong().color(ACCENT),
                            );
                            ui.add_space(8.0);
                            if ui.button(RichText::new("Copy").size(12.5)).clicked() {
                                ui.ctx().copy_text(code.clone());
                            }
                        });
                        ui.add_space(6.0);
                        ui.hyperlink_to(theme::mono(&url, MID).size(12.5), &url);
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(RichText::new("Waiting for approval").color(DIM).size(12.5));
                        });
                    });
            }
            SignIn::Done { name } => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Signed in as {name}")).color(ACCENT).size(13.5));
                    if ui.button(RichText::new("OK").size(12.5)).clicked() {
                        self.signin.dismiss();
                    }
                });
            }
            SignIn::Failed(why) => {
                ui.add_space(4.0);
                Frame::new()
                    .fill(theme::RAISE)
                    .stroke(Stroke::new(1.0, WARN.gamma_multiply(0.5)))
                    .corner_radius(8)
                    .inner_margin(egui::Margin::same(13))
                    .show(ui, |ui| {
                        ui.label(RichText::new("Could not sign in").color(WARN).size(13.5).strong());
                        ui.add_space(5.0);
                        ui.label(RichText::new(&why).color(MID).size(12.5));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button(RichText::new("Try again").size(12.5)).clicked() {
                                self.signin.dismiss();
                                self.signin.start(self.path.clone());
                            }
                            if ui.button(RichText::new("Dismiss").size(12.5)).clicked() {
                                self.signin.dismiss();
                            }
                        });
                    });
            }
        }
    }

    fn play_section(&mut self, ui: &mut egui::Ui) {
        ui.label(theme::mono("PLAY", DIM).size(11.5));
        ui.add_space(8.0);

        Self::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Server").color(MID).size(13.5));
                ui.add(
                    egui::TextEdit::singleline(&mut self.server)
                        .hint_text("play.example.com")
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace),
                );
            });
            ui.add_space(10.0);

            let ready = self.accounts.active().is_some() && !self.server.trim().is_empty();
            ui.horizontal(|ui| {
                let play = ui.add_enabled(
                    ready,
                    egui::Button::new(RichText::new("Play").size(14.5).strong()),
                );
                if play.clicked() {
                    // The renderer does not exist yet, so this is where the
                    // launcher will hand over rather than something to fake.
                    self.notice = Some((
                        "Not yet: the renderer is still being built. The connection layer works today via `neuton join`."
                            .to_string(),
                        WARN,
                    ));
                }
                match self.accounts.active() {
                    Some(a) => ui.label(
                        RichText::new(format!("as {}", a.profile.name)).color(DIM).size(12.5),
                    ),
                    None => ui.label(RichText::new("add an account first").color(DIM).size(12.5)),
                };
            });
        });
    }
}

impl Default for Launcher {
    fn default() -> Self {
        Self::new()
    }
}
